//! Ratatui TUI (milestone B). This module owns everything terminal-related;
//! the agent core only ever sees the `Ui` trait.
//!
//! Threading model: `Session::turn` blocks the agent (main) thread, so the
//! terminal lives on a dedicated render thread that keeps polling input,
//! folding `AgentEvent`s, and redrawing even while a provider round-trip or
//! a long tool call is in flight. The `Ui` impl is a thin channel proxy.
//! Plain `std::sync::mpsc` — no async runtime, matching the blocking-ureq
//! decision. See docs/TUI.md for the seam assumptions and known limits.

pub mod app;
pub mod markdown;
pub mod view;
pub mod wrap;

use crate::agent::events::AgentEvent;
use crate::cancel::CancelToken;
use app::{Action, App, TICK_MS};
use ratatui::backend::Backend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

/// Static session facts the chrome displays (model/thinking stay whatever
/// the config said — the TUI only renders them).
pub struct SessionInfo {
    pub model: String,
    pub thinking: bool,
    pub cwd: String,
    pub version: String,
    /// Profile names for `/model` Tab completion (T9); startup-validated in
    /// main, so this is display/completion data only.
    pub profiles: Vec<String>,
    /// The provider active at startup (T16): the baseline the cached-ids
    /// clear-on-provider-change comparison starts from.
    pub provider: String,
}

enum ToUi {
    Event(AgentEvent),
    /// The agent is blocked in `read_input` — authoritative idle signal.
    PromptOpen,
    /// T21: the agent thread is blocked inside the bash approver, waiting
    /// for a y/N answer about this exact command.
    ApprovalRequest {
        command: String,
        reply: mpsc::Sender<bool>,
    },
    Shutdown,
}

/// App-state facts the render loop hands each poll (T21/P3). Scripted
/// sources gate delivery on them; the real crossterm source ignores them
/// (a human types against the same rendered state).
#[derive(Debug, Clone, Copy)]
pub struct Readiness {
    /// Not mid-turn: a line submitted now cannot hit the deliberate
    /// busy-Enter drop in `App::handle_key`.
    pub idle: bool,
    /// A bash approval prompt is open and consuming keys.
    pub approval_open: bool,
}

/// Where terminal events come from; lets the whole runtime run headless in
/// tests with a scripted key sequence.
pub trait EventSource: Send {
    fn next(&mut self, timeout: Duration, ready: Readiness) -> std::io::Result<Option<Event>>;
}

struct CrosstermEvents;

impl EventSource for CrosstermEvents {
    fn next(&mut self, timeout: Duration, _ready: Readiness) -> std::io::Result<Option<Event>> {
        if event::poll(timeout)? {
            event::read().map(Some)
        } else {
            Ok(None)
        }
    }
}

/// Raw scripted source for headless tests: one event per poll, zero delay,
/// then quiet. Deliberate-timing tests (Esc mid-turn) use this; anything
/// scripting MULTIPLE lines around a turn belongs on [`ScriptedSteps`],
/// because zero-delay delivery races the busy-Enter drop (P3).
pub struct ScriptedEvents(std::collections::VecDeque<Event>);

impl ScriptedEvents {
    pub fn new(events: Vec<Event>) -> Self {
        ScriptedEvents(events.into())
    }
}

impl EventSource for ScriptedEvents {
    fn next(&mut self, timeout: Duration, _ready: Readiness) -> std::io::Result<Option<Event>> {
        // T43/P1: the render loop now drains queued events with a zero
        // timeout before each draw. A scripted source models a human typing
        // against a RENDERED frame, so it stays one event per real poll and
        // declines the drain polls; otherwise a whole script would land in a
        // single batch and the deliberate-timing tests (Esc mid-turn) would
        // stop describing what they claim to. Bursts get their own source.
        if timeout.is_zero() {
            return Ok(None);
        }
        Ok(self.0.pop_front())
    }
}

/// One unit of a readiness-gated headless script (T21/P3).
pub enum ScriptStep {
    /// Type the line and press Enter, starting only once the app is idle.
    /// This is what makes the two recorded flake modes impossible by
    /// construction: no Enter can be delivered while `busy` (so none is
    /// dropped and no lines merge), and no line is consumed early (so the
    /// driver's `read_input` always gets every scripted line).
    Line(String),
    /// Press one key, only once the approval prompt is open; a scripted
    /// answer delivered earlier would be typed into the input line instead.
    ApprovalKey(KeyCode),
    /// Deliver immediately, whatever the app state (e.g. Esc mid-turn).
    Raw(Event),
}

/// Readiness-gated scripted source (T21/P3): delivers one event per poll
/// like [`ScriptedEvents`], but starts each step only when the app state
/// says the step's keys can land the way a human's would.
pub struct ScriptedSteps {
    steps: std::collections::VecDeque<ScriptStep>,
    /// Key events of the step currently being delivered.
    buf: std::collections::VecDeque<Event>,
}

impl ScriptedSteps {
    pub fn new(steps: Vec<ScriptStep>) -> Self {
        ScriptedSteps {
            steps: steps.into(),
            buf: std::collections::VecDeque::new(),
        }
    }
}

impl EventSource for ScriptedSteps {
    fn next(&mut self, timeout: Duration, ready: Readiness) -> std::io::Result<Option<Event>> {
        // T43/P1: decline the drain polls, exactly as `ScriptedEvents` does.
        // This source exists so a step starts only when the app state says
        // its keys can land the way a human's would; letting a zero-timeout
        // drain pull the NEXT step's keys into the current batch would
        // re-open both flake modes the readiness gate was built to close.
        if timeout.is_zero() {
            return Ok(None);
        }
        if let Some(ev) = self.buf.pop_front() {
            return Ok(Some(ev));
        }
        match self.steps.front() {
            None => Ok(None),
            Some(ScriptStep::Line(_)) if !ready.idle => Ok(None),
            Some(ScriptStep::ApprovalKey(_)) if !ready.approval_open => Ok(None),
            Some(_) => match self.steps.pop_front().expect("front checked") {
                ScriptStep::Line(s) => {
                    for c in s.chars() {
                        self.buf.push_back(Event::Key(KeyEvent::new(
                            KeyCode::Char(c),
                            KeyModifiers::NONE,
                        )));
                    }
                    self.buf
                        .push_back(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
                    Ok(self.buf.pop_front())
                }
                ScriptStep::ApprovalKey(code) => {
                    Ok(Some(Event::Key(KeyEvent::new(code, KeyModifiers::NONE))))
                }
                ScriptStep::Raw(ev) => Ok(Some(ev)),
            },
        }
    }
}

/// T43/P2: does this event mean "stop the running turn" rather than "type
/// this"? Esc and Ctrl+C are the two the TUI already treats that way while
/// busy. Key RELEASES are not presses and never count.
fn is_busy_interrupt(ev: &Event) -> bool {
    match ev {
        Event::Key(key) if key.kind != KeyEventKind::Release => {
            key.code == KeyCode::Esc
                || (key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL))
        }
        _ => false,
    }
}

/// T43/P2: interrupt priority over one drained batch.
///
/// While a turn is running, a batch holding Esc or Ctrl+C is a user trying
/// to STOP it, and everything else in that batch is input they no longer
/// want delivered. So the first such key is kept and the entire rest of the
/// batch is discarded, the keys BEFORE it included: a paste followed by Esc
/// must not leave the paste sitting in the input line. Discarded input is
/// lost by design and USAGE says so.
///
/// Idle batches are returned untouched and process in order.
///
/// An open approval prompt is excluded: its keys are a modal y/N answer, not
/// input, and Esc there already means "deny this command" rather than
/// "interrupt the turn".
///
/// Known bounded edge, documented rather than solved: busy-ness is evaluated
/// once, at scan time. A batch that is idle at scan time but contains an
/// Enter processes in order, and an Esc later in that same batch interrupts
/// the turn the Enter just started.
pub fn interrupt_priority(batch: Vec<Event>, busy: bool, approval_open: bool) -> Vec<Event> {
    if !busy || approval_open {
        return batch;
    }
    match batch.iter().position(is_busy_interrupt) {
        Some(i) => vec![batch.into_iter().nth(i).expect("position is in range")],
        None => batch,
    }
}

/// Cap on terminal events processed in ONE `render_loop` iteration (T43/P1).
/// Sized well above any realistic paste, so ordinary input is never split,
/// and far below "unbounded", so a stream that never goes quiet cannot hold
/// the loop away from its draw. Anything past the cap stays queued for the
/// next iteration; nothing is dropped here.
const MAX_EVENTS_PER_ITERATION: usize = 4096;

enum LoopEnd {
    Shutdown,
    ForceQuit,
}

pub struct TuiUi {
    tx: mpsc::Sender<ToUi>,
    rx_input: mpsc::Receiver<Option<String>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl TuiUi {
    /// Real terminal: alternate screen + raw mode on a render thread.
    /// Restoration is covered three ways: the normal path after the loop,
    /// a chained panic hook, and `Drop` (which joins the thread).
    ///
    /// `cancel` is the session's cancel token (T6): the render thread holds
    /// this clone — never a `Session` reference — and sets it on Esc.
    pub fn new(info: SessionInfo, cancel: CancelToken) -> std::io::Result<TuiUi> {
        // Fail fast on a broken tty from the calling thread, so the error
        // surfaces before the agent starts (raw-mode state is global).
        ratatui::crossterm::terminal::enable_raw_mode()?;
        ratatui::crossterm::terminal::disable_raw_mode()?;

        static PANIC_HOOK: Once = Once::new();
        PANIC_HOOK.call_once(|| {
            let prev = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |panic_info| {
                ratatui::restore(); // before the message, so it's readable
                prev(panic_info);
            }));
        });

        let (tx, rx) = mpsc::channel::<ToUi>();
        let (tx_input, rx_input) = mpsc::channel::<Option<String>>();
        let thread = std::thread::Builder::new()
            .name("tui-render".into())
            .spawn(move || {
                let terminal = match ratatui::try_init() {
                    Ok(t) => t,
                    Err(_) => {
                        // Terminal unusable: unblock the agent and bail.
                        let _ = tx_input.send(None);
                        return;
                    }
                };
                let mut app = App::new(info.model, info.thinking, info.cwd, info.version);
                app.profiles = info.profiles;
                app.provider = info.provider;
                let (_, end) =
                    render_loop(terminal, app, rx, tx_input, &mut CrosstermEvents, cancel);
                ratatui::restore();
                if matches!(end, LoopEnd::ForceQuit) {
                    eprintln!("temur: force quit while a turn was running");
                    std::process::exit(130);
                }
            })?;
        Ok(TuiUi {
            tx,
            rx_input,
            thread: Some(thread),
        })
    }

    /// Headless runtime for tests: same threads, channels, and render loop,
    /// but over a `TestBackend` and a raw scripted key sequence. The final
    /// frame is captured into the returned handle when the loop shuts down.
    pub fn headless(
        info: SessionInfo,
        width: u16,
        height: u16,
        script: Vec<Event>,
        cancel: CancelToken,
    ) -> (TuiUi, Arc<Mutex<Vec<String>>>) {
        Self::headless_with_source(info, width, height, ScriptedEvents::new(script), cancel)
    }

    /// Headless runtime over a readiness-gated step script (T21/P3): the
    /// harness for anything that submits multiple lines around turns, or
    /// answers an approval prompt.
    pub fn headless_steps(
        info: SessionInfo,
        width: u16,
        height: u16,
        steps: Vec<ScriptStep>,
        cancel: CancelToken,
    ) -> (TuiUi, Arc<Mutex<Vec<String>>>) {
        Self::headless_with_source(info, width, height, ScriptedSteps::new(steps), cancel)
    }

    /// Headless runtime over an arbitrary [`EventSource`]. Public so tests
    /// can drive the loop from a source of their own (T43/P1 burst tests).
    pub fn headless_with_source(
        info: SessionInfo,
        width: u16,
        height: u16,
        mut source: impl EventSource + 'static,
        cancel: CancelToken,
    ) -> (TuiUi, Arc<Mutex<Vec<String>>>) {
        let snapshot = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&snapshot);
        let (tx, rx) = mpsc::channel::<ToUi>();
        let (tx_input, rx_input) = mpsc::channel::<Option<String>>();
        let thread = std::thread::Builder::new()
            .name("tui-render-headless".into())
            .spawn(move || {
                let backend = ratatui::backend::TestBackend::new(width, height);
                let terminal = Terminal::new(backend).expect("test backend");
                let mut app = App::new(info.model, info.thinking, info.cwd, info.version);
                app.profiles = info.profiles;
                app.provider = info.provider;
                let (terminal, _) =
                    render_loop(terminal, app, rx, tx_input, &mut source, cancel);
                let buf = terminal.backend().buffer();
                let rows: Vec<String> = (0..height)
                    .map(|y| {
                        (0..width)
                            .map(|x| buf[(x, y)].symbol())
                            .collect::<String>()
                            .trim_end()
                            .to_string()
                    })
                    .collect();
                *captured.lock().unwrap() = rows;
            })
            .expect("spawn headless render thread");
        (
            TuiUi {
                tx,
                rx_input,
                thread: Some(thread),
            },
            snapshot,
        )
    }

    /// The per-command bash approver an interactive TUI session installs
    /// (T21): sends the exact command to the render thread and blocks the
    /// calling (agent) thread until the user answers the rendered y/N
    /// prompt. Any channel breakage (render thread gone, shutdown while
    /// pending) denies.
    pub fn bash_approver(&self) -> Box<dyn FnMut(&str) -> bool> {
        let tx = self.tx.clone();
        Box::new(move |command: &str| {
            let (reply_tx, reply_rx) = mpsc::channel();
            if tx
                .send(ToUi::ApprovalRequest {
                    command: command.to_string(),
                    reply: reply_tx,
                })
                .is_err()
            {
                return false;
            }
            reply_rx.recv().unwrap_or(false)
        })
    }
}

impl super::Ui for TuiUi {
    fn event(&mut self, ev: &AgentEvent) {
        let _ = self.tx.send(ToUi::Event(ev.clone()));
    }

    fn read_input(&mut self) -> Option<String> {
        if self.tx.send(ToUi::PromptOpen).is_err() {
            return None; // render thread gone: quit cleanly
        }
        self.rx_input.recv().ok().flatten()
    }
}

impl Drop for TuiUi {
    fn drop(&mut self) {
        let _ = self.tx.send(ToUi::Shutdown);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

fn render_loop<B: Backend>(
    mut terminal: Terminal<B>,
    mut app: App,
    rx: mpsc::Receiver<ToUi>,
    tx_input: mpsc::Sender<Option<String>>,
    events: &mut dyn EventSource,
    cancel: CancelToken,
) -> (Terminal<B>, LoopEnd) {
    let start = Instant::now();
    // T21: the reply channel of the approval prompt currently on screen.
    // Dropped un-answered on any loop exit, which the blocked agent thread
    // reads as a denial.
    let mut pending_approval: Option<mpsc::Sender<bool>> = None;
    let end = 'main: loop {
        // Drain everything the agent thread sent since the last frame; a
        // shutdown still gets one final draw below so the last frame shows
        // every folded event (the headless harness snapshots that frame).
        let mut shutdown = false;
        loop {
            match rx.try_recv() {
                Ok(ToUi::Event(ev)) => {
                    app.now_ms = start.elapsed().as_millis() as u64;
                    app.fold(&ev);
                }
                Ok(ToUi::PromptOpen) => app.prompt_open(),
                Ok(ToUi::ApprovalRequest { command, reply }) => {
                    app.approval = Some(command);
                    pending_approval = Some(reply);
                }
                // Shutdown/disconnect can only follow every other message
                // (Drop sends it last), so nothing is left behind here.
                Ok(ToUi::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => {
                    shutdown = true;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }

        app.now_ms = start.elapsed().as_millis() as u64;
        if terminal.draw(|frame| view::draw(&mut app, frame)).is_err() {
            // Unusable terminal mid-session: unblock the agent and stop.
            let _ = tx_input.send(None);
            break LoopEnd::Shutdown;
        }
        if shutdown {
            break LoopEnd::Shutdown;
        }

        let ready = Readiness {
            idle: !app.busy,
            approval_open: app.approval.is_some(),
        };
        // T43/P1: one blocking poll for the first event, then drain whatever
        // else the terminal already has queued using a zero timeout, and draw
        // ONCE for the whole batch. A paste arrives as thousands of key
        // events; handling one per frame made redraw cost scale with the
        // length of the paste instead of with the frame rate.
        let mut batch: Vec<Event> = Vec::new();
        let mut source_failed = false;
        match events.next(Duration::from_millis(TICK_MS), ready) {
            Ok(Some(ev)) => batch.push(ev),
            Ok(None) => {}
            Err(_) => source_failed = true,
        }
        if !source_failed && !batch.is_empty() {
            while batch.len() < MAX_EVENTS_PER_ITERATION {
                match events.next(Duration::ZERO, ready) {
                    Ok(Some(ev)) => batch.push(ev),
                    Ok(None) => break,
                    Err(_) => {
                        source_failed = true;
                        break;
                    }
                }
            }
        }
        if source_failed {
            let _ = tx_input.send(None);
            break LoopEnd::Shutdown;
        }
        let batch = interrupt_priority(batch, app.busy, app.approval.is_some());
        for ev in batch {
            let key = match ev {
                Event::Key(key) => key,
                // Resize just needs the redraw that happens next iteration.
                _ => continue,
            };
            match app.handle_key(key) {
                Action::Submit(line) => {
                    if line == "exit" || line == "quit" {
                        let _ = tx_input.send(None);
                    } else if line.starts_with('/') {
                        // T8 command line: recorded + echoed dim and
                        // recallable, but NOT a prompt — no App::submit (no
                        // User cell, no title, no busy spinner) and no
                        // cancel-token clear (no turn starts). The main
                        // loop executes it and events fold back as usual.
                        app.submit_command(&line);
                        let _ = tx_input.send(Some(line));
                    } else {
                        // F7: clear the cancel token at SUBMISSION, on the
                        // same thread that processes Esc — a stale Esc from
                        // after the previous turn is wiped here, and an Esc
                        // arriving after this line is a real interrupt that
                        // `Session::turn` must never clear away.
                        cancel.clear();
                        app.submit(&line);
                        let _ = tx_input.send(Some(line));
                    }
                }
                Action::Quit => {
                    let _ = tx_input.send(None);
                }
                Action::ForceQuit => break 'main LoopEnd::ForceQuit,
                // The whole interrupt mechanism from this thread's side:
                // set the flag; the blocked agent thread notices at its
                // next cooperative checkpoint and lands the turn.
                Action::Interrupt => cancel.set(),
                // T21: unblock the agent thread with the user's answer. A
                // vanished receiver just means the approver gave up (it
                // denies on its own); nothing to do.
                Action::Approval(approve) => {
                    if let Some(reply) = pending_approval.take() {
                        let _ = reply.send(approve);
                    }
                }
                Action::None => {}
            }
        }
    };
    (terminal, end)
}

/// Terminal prove-it (the M0 `tls-probe` pattern applied to the TUI stack):
/// enter the alternate screen through the real crossterm path, draw one
/// frame, wait for `q` (or a 10s deadline so scripted runs can never hang),
/// and restore the terminal. Verifies raw mode + alt screen + key input on
/// the actual tty without needing the agent at all.
pub fn probe() -> std::io::Result<()> {
    let mut terminal = ratatui::try_init()?;
    let result = probe_loop(&mut terminal);
    ratatui::restore();
    // Only report after the terminal is back to normal.
    match &result {
        Ok(quit) => println!("tui-probe OK: alternate screen entered and restored ({quit})"),
        Err(e) => println!("tui-probe FAILED: {e}"),
    }
    result.map(|_| ())
}

fn probe_loop(terminal: &mut ratatui::DefaultTerminal) -> std::io::Result<&'static str> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        terminal.draw(|frame| {
            let text = ratatui::widgets::Paragraph::new(
                "temur tui-probe — press q to quit (auto-quits in 10s)",
            );
            frame.render_widget(text, frame.area());
        })?;
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => return Ok("quit by key"),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok("quit by ctrl-c")
                    }
                    _ => {}
                }
            }
        }
        if Instant::now() >= deadline {
            return Ok("quit by deadline");
        }
    }
}
