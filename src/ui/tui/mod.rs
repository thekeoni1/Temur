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
pub mod view;
pub mod wrap;

use crate::agent::events::AgentEvent;
use app::{Action, App, TICK_MS};
use ratatui::backend::Backend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyModifiers};
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
}

enum ToUi {
    Event(AgentEvent),
    /// The agent is blocked in `read_input` — authoritative idle signal.
    PromptOpen,
    Shutdown,
}

/// Where terminal events come from; lets the whole runtime run headless in
/// tests with a scripted key sequence.
pub trait EventSource: Send {
    fn next(&mut self, timeout: Duration) -> std::io::Result<Option<Event>>;
}

struct CrosstermEvents;

impl EventSource for CrosstermEvents {
    fn next(&mut self, timeout: Duration) -> std::io::Result<Option<Event>> {
        if event::poll(timeout)? {
            event::read().map(Some)
        } else {
            Ok(None)
        }
    }
}

/// Scripted source for headless tests: one event per poll, then quiet.
pub struct ScriptedEvents(std::collections::VecDeque<Event>);

impl ScriptedEvents {
    pub fn new(events: Vec<Event>) -> Self {
        ScriptedEvents(events.into())
    }
}

impl EventSource for ScriptedEvents {
    fn next(&mut self, _timeout: Duration) -> std::io::Result<Option<Event>> {
        Ok(self.0.pop_front())
    }
}

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
    pub fn new(info: SessionInfo) -> std::io::Result<TuiUi> {
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
                let app = App::new(info.model, info.thinking, info.cwd, info.version);
                let (_, end) =
                    render_loop(terminal, app, rx, tx_input, &mut CrosstermEvents);
                ratatui::restore();
                if matches!(end, LoopEnd::ForceQuit) {
                    eprintln!("opencode-rust: force quit while a turn was running");
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
    /// but over a `TestBackend` and a scripted key sequence. The final frame
    /// is captured into the returned handle when the loop shuts down.
    pub fn headless(
        info: SessionInfo,
        width: u16,
        height: u16,
        script: Vec<Event>,
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
                let app = App::new(info.model, info.thinking, info.cwd, info.version);
                let mut source = ScriptedEvents::new(script);
                let (terminal, _) = render_loop(terminal, app, rx, tx_input, &mut source);
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
) -> (Terminal<B>, LoopEnd) {
    let start = Instant::now();
    let end = loop {
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

        match events.next(Duration::from_millis(TICK_MS)) {
            Ok(Some(Event::Key(key))) => match app.handle_key(key) {
                Action::Submit(line) => {
                    if line == "exit" || line == "quit" {
                        let _ = tx_input.send(None);
                    } else {
                        app.submit(&line);
                        let _ = tx_input.send(Some(line));
                    }
                }
                Action::Quit => {
                    let _ = tx_input.send(None);
                }
                Action::ForceQuit => break LoopEnd::ForceQuit,
                Action::None => {}
            },
            // Resize just needs the redraw that happens next iteration.
            Ok(Some(_)) | Ok(None) => {}
            Err(_) => {
                let _ = tx_input.send(None);
                break LoopEnd::Shutdown;
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
                "opencode-rust tui-probe — press q to quit (auto-quits in 10s)",
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
