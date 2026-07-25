//! TUI offline tests, layers 1–2 of the milestone-B test strategy:
//! event-fold tests over App, and frame snapshots via ratatui's TestBackend.
//! No terminal, no threads, no network — runs identically on host and in the
//! i386 container.

use temur::agent::events::AgentEvent;
use temur::agent::{Session, SessionConfig};
use temur::provider::anthropic::transport::ReplayTransport;
use temur::provider::anthropic::AnthropicProvider;
use temur::provider::{CancelToken, ChatRequest, Provider, ProviderError, ResponseMessage, StreamEvent, Usage};
use temur::tools::Registry;
use temur::ui::tui::app::{Action, App, Cell};
use temur::ui::tui::view::draw;
use temur::ui::tui::{SessionInfo, TuiUi};
use temur::ui::Ui;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

fn app() -> App {
    App::new(
        "claude-sonnet-5".into(),
        false,
        "/mnt/c/RustCode".into(),
        "0.1.0".into(),
    )
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn type_str(a: &mut App, s: &str) {
    for c in s.chars() {
        a.handle_key(key(KeyCode::Char(c)));
    }
}

fn usage(input: u64, output: u64) -> Usage {
    Usage {
        input_tokens: Some(input),
        output_tokens: Some(output),
        ..Default::default()
    }
}

/// Render at the given size and return the buffer as plain-text rows
/// (styles dropped; targeted style checks read the buffer directly).
fn render(app: &mut App, w: u16, h: u16) -> Vec<String> {
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(app, f)).unwrap();
    let buf = terminal.backend().buffer().clone();
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

// ---------------------------------------------------------------- layer 1: folds

#[test]
fn text_deltas_accumulate_into_one_cell() {
    let mut a = app();
    a.fold(&AgentEvent::TextDelta("Hello ".into()));
    a.fold(&AgentEvent::TextDelta("world".into()));
    assert_eq!(a.cells, vec![Cell::AssistantText("Hello world".into())]);
}

#[test]
fn thinking_deltas_collapse_to_one_indicator() {
    let mut a = app();
    a.fold(&AgentEvent::ThinkingDelta("a".into()));
    a.fold(&AgentEvent::ThinkingDelta("b".into()));
    a.fold(&AgentEvent::TextDelta("answer".into()));
    assert_eq!(
        a.cells,
        vec![Cell::Thinking, Cell::AssistantText("answer".into())]
    );
}

#[test]
fn absent_usage_renders_dashes_not_zero() {
    // Local servers may never report usage: the tail and footer must show
    // "—" per unreported field, never a fake 0.
    let mut a = app();
    a.fold(&AgentEvent::TurnComplete {
        turn_usage: Usage::default(),
        session_usage: Usage::default(),
    });
    let rows = render(&mut a, 100, 12);
    let all = rows.join("\n");
    assert!(
        all.contains("— in / — out"),
        "expected — for unreported counts:\n{all}"
    );
    assert!(
        !all.contains("0 in / 0 out"),
        "absent usage must never render as 0:\n{all}"
    );
}

#[test]
fn fifo_pairing_matches_parallel_tools_in_order() {
    let mut a = app();
    // Two tool_use blocks stream first, then execute sequentially in the
    // same order — the seam has no call id (documented assumption).
    a.fold(&AgentEvent::ToolStart { name: "read".into() });
    a.fold(&AgentEvent::ToolStart { name: "bash".into() });
    a.fold(&AgentEvent::ToolEnd {
        name: "read".into(),
        title: "src/main.rs".into(),
        is_error: false,
    });
    a.fold(&AgentEvent::ToolEnd {
        name: "bash".into(),
        title: "cargo build".into(),
        is_error: true,
    });
    match (&a.cells[0], &a.cells[1]) {
        (Cell::Tool(first), Cell::Tool(second)) => {
            assert_eq!(first.name, "read");
            assert_eq!(first.title.as_deref(), Some("src/main.rs"));
            assert!(!first.is_error);
            assert_eq!(second.name, "bash");
            assert_eq!(second.title.as_deref(), Some("cargo build"));
            assert!(second.is_error);
        }
        other => panic!("expected two tool cells, got {other:?}"),
    }
}

#[test]
fn unmatched_tool_end_is_not_lost() {
    let mut a = app();
    a.fold(&AgentEvent::ToolEnd {
        name: "grep".into(),
        title: "3 matches".into(),
        is_error: false,
    });
    assert!(matches!(&a.cells[0], Cell::Tool(t) if t.title.is_some()));
}

#[test]
fn turn_complete_updates_usage_and_appends_tail() {
    let mut a = app();
    a.now_ms = 1000;
    a.submit("do the thing");
    assert!(a.busy);
    a.now_ms = 4200;
    a.fold(&AgentEvent::TurnComplete {
        turn_usage: usage(10, 20),
        session_usage: usage(110, 220),
    });
    assert!(!a.busy);
    assert_eq!(a.session_usage.input_tokens, Some(110));
    assert!(
        matches!(a.cells.last(), Some(Cell::TurnTail { secs: 3, usage: u }) if u.output_tokens == Some(20))
    );
}

#[test]
fn refusal_shape_notice_then_turn_complete() {
    // The event shape the agent emits for a refusal (M4 semantics):
    // Notice followed by TurnComplete, no text kept.
    let mut a = app();
    a.now_ms = 500;
    a.submit("bad request");
    a.fold(&AgentEvent::Notice(
        "the model refused this request (category: safety)".into(),
    ));
    a.fold(&AgentEvent::TurnComplete {
        turn_usage: usage(5, 0),
        session_usage: usage(5, 0),
    });
    assert!(matches!(&a.cells[1], Cell::Notice(n) if n.contains("refused")));
    assert!(!a.busy);
}

// ------------------------------------------------------------- input editing

#[test]
fn input_editing_cursor_and_history() {
    let mut a = app();
    type_str(&mut a, "hxi");
    a.handle_key(key(KeyCode::Left));
    a.handle_key(key(KeyCode::Backspace));
    assert_eq!(a.input, "hi");
    assert_eq!(a.cursor, 1);
    a.handle_key(key(KeyCode::End));
    assert_eq!(a.handle_key(key(KeyCode::Enter)), Action::Submit("hi".into()));
    a.submit("hi");

    // History recall, then back down to the (empty) draft.
    a.handle_key(key(KeyCode::Up));
    assert_eq!(a.input, "hi");
    a.handle_key(key(KeyCode::Down));
    assert_eq!(a.input, "");
}

#[test]
fn enter_is_disabled_while_busy_and_quit_semantics_hold() {
    let mut a = app();
    a.submit("first");
    type_str(&mut a, "queued");
    assert_eq!(a.handle_key(key(KeyCode::Enter)), Action::None);

    // Ctrl+C during a turn arms force-quit; second one fires it.
    assert_eq!(a.handle_key(ctrl('c')), Action::None);
    assert!(a.force_quit_armed);
    assert_eq!(a.handle_key(ctrl('c')), Action::ForceQuit);

    // Any other key disarms.
    a.force_quit_armed = true;
    a.handle_key(key(KeyCode::Char('x')));
    assert!(!a.force_quit_armed);
    assert_eq!(a.handle_key(ctrl('c')), Action::None);

    // Idle + empty input: Ctrl+C and Ctrl+D quit; non-empty Ctrl+C clears.
    a.prompt_open();
    a.input.clear();
    a.cursor = 0;
    assert_eq!(a.handle_key(ctrl('d')), Action::Quit);
    type_str(&mut a, "draft");
    assert_eq!(a.handle_key(ctrl('c')), Action::None);
    assert_eq!(a.input, "");
    assert_eq!(a.handle_key(ctrl('c')), Action::Quit);
}

// --------------------------------------------------------- layer 2: snapshots

#[test]
fn frame_empty_welcome_state() {
    let mut a = app();
    let rows = render(&mut a, 90, 8);
    assert!(rows[0].starts_with(" # new session"));
    assert!(rows[0].contains("claude-sonnet-5 · temur 0.1.0"));
    assert!(rows[5].contains("▌ > ask anything… (exit to quit)"));
    assert!(rows[6].contains("enter send"));
    assert!(rows[7].contains("/mnt/c/RustCode"));
    assert!(rows[7].contains("thinking off"));
}

#[test]
fn frame_mid_turn_streaming_with_busy_row() {
    let mut a = app();
    a.now_ms = 0;
    a.submit("read the file");
    a.fold(&AgentEvent::ToolStart { name: "read".into() });
    a.now_ms = 100; // deterministic spinner frame ⠙
    let rows = render(&mut a, 60, 10);
    assert!(rows[0].starts_with(" # read the file"), "title from first input: {}", rows[0]);
    assert!(rows.iter().any(|r| r.contains("▌ read the file")), "user cell: {rows:?}");
    assert!(rows.iter().any(|r| r.contains("~ read…")), "running tool: {rows:?}");
    assert!(rows[8].contains("⠙ working…"), "busy row: {}", rows[8]);
    assert!(rows[8].contains("enter disabled during turn"));

    // T8-P2 styling pass: the running-tool line is dim, matching the
    // thinking indicator and the ⚙ of a completed inline tool.
    let backend = TestBackend::new(60, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(&mut a, f)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let y = rows.iter().position(|r| r.contains("~ read…")).unwrap() as u16;
    let x = rows[y as usize].find('~').unwrap() as u16;
    assert!(
        buf[(x, y)].style().add_modifier.contains(ratatui::style::Modifier::DIM),
        "running tool line renders dim"
    );
}

#[test]
fn frame_inline_and_block_tools() {
    let mut a = app();
    a.submit("go");
    a.fold(&AgentEvent::ToolStart { name: "read".into() });
    a.fold(&AgentEvent::ToolEnd {
        name: "read".into(),
        title: "src/main.rs (140 lines)".into(),
        is_error: false,
    });
    a.fold(&AgentEvent::ToolStart { name: "bash".into() });
    a.fold(&AgentEvent::ToolEnd {
        name: "bash".into(),
        title: "cargo build --quiet".into(),
        is_error: false,
    });
    a.fold(&AgentEvent::ToolStart { name: "grep".into() });
    a.fold(&AgentEvent::ToolEnd {
        name: "grep".into(),
        title: "no matches".into(),
        is_error: true,
    });
    let rows = render(&mut a, 60, 14);
    let body = rows.join("\n");
    assert!(body.contains("⚙ read: src/main.rs (140 lines)"), "inline ok tool:\n{body}");
    assert!(body.contains("▌ # bash"), "block tool header:\n{body}");
    assert!(body.contains("▌   cargo build --quiet"), "block tool body:\n{body}");
    assert!(body.contains("✗ grep: no matches"), "inline error tool:\n{body}");

    // The error line renders red.
    let backend = TestBackend::new(60, 14);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(&mut a, f)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let y = rows.iter().position(|r| r.contains("✗ grep")).unwrap() as u16;
    let x = rows[y as usize].find('✗').unwrap() as u16;
    assert_eq!(buf[(x, y)].style().fg, Some(ratatui::style::Color::Red));
}

#[test]
fn frame_resume_notice_renders_before_any_input() {
    // T5: the resume summary arrives as a plain Notice after UI construction
    // and before the first prompt — exactly what main.rs emits on
    // --continue. It must render through the existing notice pattern with an
    // empty transcript, including the em dash for never-reported usage.
    let mut a = app();
    a.fold(&AgentEvent::Notice(
        "resumed session: 12 messages, ~3400 tokens in / — out".into(),
    ));
    let rows = render(&mut a, 70, 10);
    let body = rows.join("\n");
    assert!(
        body.contains("▌ [!] resumed session: 12 messages"),
        "resume notice block:\n{body}"
    );
    assert!(
        body.contains("~3400 tokens in / — out"),
        "absent usage stays an em dash, not a zero:\n{body}"
    );
}

#[test]
fn frame_notice_and_turn_tail_and_footer_totals() {
    let mut a = app();
    a.now_ms = 0;
    a.submit("hi");
    a.fold(&AgentEvent::TextDelta("Hello!".into()));
    a.fold(&AgentEvent::Notice("response truncated: max_tokens reached".into()));
    a.now_ms = 2000;
    a.fold(&AgentEvent::TurnComplete {
        turn_usage: usage(12, 34),
        session_usage: usage(120, 340),
    });
    let rows = render(&mut a, 70, 14);
    let body = rows.join("\n");
    assert!(body.contains("   Hello!"), "assistant text indented:\n{body}");
    assert!(body.contains("▌ [!] response truncated"), "notice block:\n{body}");
    assert!(body.contains("▣ temur · claude-sonnet-5 · 2s · 12 in / 34 out"), "tail:\n{body}");
    assert!(rows[13].contains("session 120 in / 340 out"), "footer: {}", rows[13]);
}

#[test]
fn frame_wrapping_at_narrow_width() {
    let mut a = app();
    a.submit("go");
    a.fold(&AgentEvent::TextDelta(
        "one two three four five six seven eight nine ten".into(),
    ));
    let rows = render(&mut a, 40, 12);
    let text_rows: Vec<&String> = rows.iter().filter(|r| r.starts_with("    ")).collect();
    assert!(text_rows.len() >= 2, "narrow width forces wrap: {rows:?}");
    // No row exceeds the terminal width (already guaranteed by buffer, but
    // wrapped words must not be truncated away entirely).
    assert!(rows.join(" ").contains("ten"));
}

#[test]
fn scroll_unsticks_on_pageup_and_resticks_at_bottom() {
    let mut a = app();
    a.submit("fill");
    // Markdown-era migration: single newlines are CommonMark soft breaks
    // (reflowed as spaces), so the fill uses hard breaks (trailing double
    // space) to keep the original one-row-per-line scroll math.
    for i in 0..30 {
        a.fold(&AgentEvent::TextDelta(format!("line {i}  \n")));
    }
    // First render pins to bottom and records metrics.
    let rows = render(&mut a, 40, 12);
    assert!(rows.iter().any(|r| r.contains("line 29")), "sticky bottom: {rows:?}");

    a.handle_key(key(KeyCode::PageUp));
    assert!(!a.stick_bottom);
    let rows = render(&mut a, 40, 12);
    assert!(!rows.iter().any(|r| r.contains("line 29")), "scrolled up: {rows:?}");
    assert!(rows.iter().any(|r| r.contains("[scroll")), "indicator: {rows:?}");

    a.handle_key(key(KeyCode::PageDown));
    assert!(a.stick_bottom);
    let rows = render(&mut a, 40, 12);
    assert!(rows.iter().any(|r| r.contains("line 29")), "restuck: {rows:?}");
}

// ------------------------------------------------- layer 3: headless seam e2e

/// The real `TuiUi` runtime — render thread, channels, `read_input`,
/// `event`, shutdown — driven end-to-end against a real `Session` over the
/// same replay fixtures check.sh's mock REPL smoke uses. Only the terminal
/// backend (TestBackend) and the key source (scripted) are substituted.
#[test]
fn headless_end_to_end_through_the_ui_seam() {
    let dir = tempfile::tempdir().unwrap();
    let provider = AnthropicProvider::new(
        "https://mock.invalid",
        "mock-key".into(),
        Box::new(ReplayTransport::new(vec![
            format!("{}/tests/fixtures/tool_use_parallel.sse", env!("CARGO_MANIFEST_DIR")).into(),
            format!("{}/tests/fixtures/text_simple.sse", env!("CARGO_MANIFEST_DIR")).into(),
        ])),
    );
    let cfg = SessionConfig {
        model: "claude-sonnet-5".into(),
        max_tokens: 32_000,
        system: Some("test system".into()),
        thinking: false,
        cwd: dir.path().to_path_buf(),
        max_iterations: 50,
        temperature: None,
        top_p: None,
        context_window: None,
    };
    let mut session = Session::new(Box::new(provider), Registry::standard(), cfg);

    let mut script: Vec<Event> = "do the smoke task"
        .chars()
        .map(|c| Event::Key(key(KeyCode::Char(c))))
        .collect();
    script.push(Event::Key(key(KeyCode::Enter)));

    let (mut ui, snapshot) = TuiUi::headless(
        SessionInfo {
            model: "claude-sonnet-5".into(),
            thinking: false,
            cwd: dir.path().display().to_string(),
            version: "test".into(),
        },
        100,
        30,
        script,
        session.cancel_token(),
    );

    let line = ui.read_input().expect("scripted submit reaches read_input");
    assert_eq!(line, "do the smoke task");
    session.turn(&line, &mut |ev| ui.event(&ev)).unwrap();
    drop(ui); // shutdown + join; the final frame lands in `snapshot`

    let rows = snapshot.lock().unwrap().clone();
    assert!(!rows.is_empty(), "render thread captured a final frame");
    let body = rows.join("\n");
    assert!(rows[0].contains("# do the smoke task"), "title:\n{body}");
    assert!(body.contains("▌ do the smoke task"), "user cell:\n{body}");
    assert!(
        body.contains("I'll read the file and list the directory."),
        "streamed text:\n{body}"
    );
    assert!(body.contains("read:"), "read tool completed:\n{body}");
    assert!(body.contains("▌ # bash"), "bash block tool:\n{body}");
    assert!(body.contains("Hello, world!"), "second round text:\n{body}");
    assert!(body.contains("▣ temur · claude-sonnet-5"), "turn tail:\n{body}");
}

#[test]
fn resize_rewraps_same_state() {
    let mut a = app();
    a.submit("go");
    a.fold(&AgentEvent::TextDelta(
        "alpha beta gamma delta epsilon zeta eta theta".into(),
    ));
    let wide = render(&mut a, 80, 10);
    let narrow = render(&mut a, 30, 10);
    assert!(wide.iter().any(|r| r.contains("alpha beta gamma delta")));
    assert!(narrow.iter().any(|r| r.contains("alpha")));
    assert!(narrow.iter().all(|r| r.chars().count() <= 30));
    // Content survives both widths.
    assert!(narrow.join(" ").contains("theta"));
}
// ---- to append to tests/tui.rs (T6 I4) ----

// ------------------------------------------------------ T6 (I4): interrupt

#[test]
fn esc_interrupts_only_while_busy_and_is_idempotent() {
    let mut a = app();
    // Idle: Esc is a no-op.
    assert_eq!(a.handle_key(key(KeyCode::Esc)), Action::None);
    assert!(!a.interrupting);

    a.submit("go"); // busy
    assert_eq!(a.handle_key(key(KeyCode::Esc)), Action::Interrupt);
    assert!(a.interrupting);
    // Second Esc: idempotent (setting an already-set token is harmless).
    assert_eq!(a.handle_key(key(KeyCode::Esc)), Action::Interrupt);

    // TurnComplete clears the interrupting state along with busy.
    a.fold(&AgentEvent::TurnComplete {
        turn_usage: usage(1, 1),
        session_usage: usage(1, 1),
    });
    assert!(!a.busy);
    assert!(!a.interrupting);
    assert_eq!(a.handle_key(key(KeyCode::Esc)), Action::None);
}

#[test]
fn esc_disarms_force_quit_and_ctrl_c_semantics_unchanged() {
    let mut a = app();
    a.submit("go");
    assert_eq!(a.handle_key(ctrl('c')), Action::None); // arms
    assert!(a.force_quit_armed);
    // Esc participates in the "any key disarms" rule…
    assert_eq!(a.handle_key(key(KeyCode::Esc)), Action::Interrupt);
    assert!(!a.force_quit_armed);
    // …so the next Ctrl+C re-arms instead of force-quitting.
    assert_eq!(a.handle_key(ctrl('c')), Action::None);
    assert!(a.force_quit_armed);
    // Ctrl+C twice in a row still force-quits, interrupting or not.
    assert_eq!(a.handle_key(ctrl('c')), Action::ForceQuit);
}

#[test]
fn status_row_shows_esc_hint_and_interrupting_state() {
    let mut a = app();
    a.submit("go");
    let rows = render(&mut a, 80, 12);
    let body = rows.join("\n");
    assert!(body.contains("esc interrupt"), "busy hint:\n{body}");

    a.handle_key(key(KeyCode::Esc));
    let rows = render(&mut a, 80, 12);
    let body = rows.join("\n");
    assert!(body.contains("interrupting…"), "interrupting state:\n{body}");

    a.fold(&AgentEvent::TurnComplete {
        turn_usage: usage(1, 1),
        session_usage: usage(1, 1),
    });
    let rows = render(&mut a, 80, 12);
    let body = rows.join("\n");
    assert!(!body.contains("interrupting…"), "cleared after turn:\n{body}");
    assert!(body.contains("enter send"), "idle hint back:\n{body}");
}

/// Provider that streams a partial tail and then BLOCKS in 10 ms slices
/// until the cancel token is set — it can only finish through the
/// render-thread → token → agent chain, so this e2e has no timing races.
struct BlockUntilCancelled;

impl Provider for BlockUntilCancelled {
    fn stream(
        &self,
        _req: &ChatRequest,
        on_event: &mut dyn FnMut(StreamEvent),
        cancel: &CancelToken,
    ) -> Result<ResponseMessage, ProviderError> {
        on_event(StreamEvent::TextDelta("partial tail".into()));
        while !cancel.is_set() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let value = serde_json::json!({
            "id": "msg_block",
            "model": "claude-sonnet-5",
            "role": "assistant",
            "content": [],
            "usage": {}
        });
        let mut m: ResponseMessage = serde_json::from_value(value).unwrap();
        m.content = vec![temur::provider::ContentBlock::Text {
            text: "partial tail".into(),
        }];
        Ok(m)
    }
}

/// F7 regression: a STALE token (Esc that landed after the previous turn
/// finished) is cleared at SUBMISSION by the render thread — not by
/// `Session::turn`, which no longer clears. The turn after a stale Esc
/// must complete normally.
#[test]
fn headless_submission_clears_a_stale_token() {
    let dir = tempfile::tempdir().unwrap();
    let provider = AnthropicProvider::new(
        "https://mock.invalid",
        "mock-key".into(),
        Box::new(ReplayTransport::new(vec![format!(
            "{}/tests/fixtures/text_simple.sse",
            env!("CARGO_MANIFEST_DIR")
        )
        .into()])),
    );
    let cfg = SessionConfig {
        model: "claude-sonnet-5".into(),
        max_tokens: 32_000,
        system: Some("test system".into()),
        thinking: false,
        cwd: dir.path().to_path_buf(),
        max_iterations: 50,
        temperature: None,
        top_p: None,
        context_window: None,
    };
    let mut session = Session::new(Box::new(provider), Registry::standard(), cfg);
    // The stale Esc: set after the (zeroth) turn ended, before submission.
    session.cancel_token().set();

    let mut script: Vec<Event> = "hello"
        .chars()
        .map(|c| Event::Key(key(KeyCode::Char(c))))
        .collect();
    script.push(Event::Key(key(KeyCode::Enter)));

    let (mut ui, snapshot) = TuiUi::headless(
        SessionInfo {
            model: "claude-sonnet-5".into(),
            thinking: false,
            cwd: dir.path().display().to_string(),
            version: "test".into(),
        },
        100,
        30,
        script,
        session.cancel_token(),
    );

    let line = ui.read_input().expect("scripted submit reaches read_input");
    session.turn(&line, &mut |ev| ui.event(&ev)).unwrap();
    drop(ui);

    let rows = snapshot.lock().unwrap().clone();
    let body = rows.join("\n");
    assert!(
        !body.contains("turn interrupted"),
        "stale token must be wiped at submission:\n{body}"
    );
    assert!(body.contains("Hello, world!"), "normal completion:\n{body}");
}

/// F7 regression: coalesced Enter+Esc. The render thread clears the token
/// in the Submit arm BEFORE forwarding the line, and processes the Esc
/// right after — with the old turn-entry clear, `Session::turn` (agent
/// thread) could wipe that Esc and the interrupt was lost (this test then
/// hangs on the blocking provider). Now the interrupt deterministically
/// survives: clear-at-submission is ordered before the Esc on the SAME
/// thread, and nothing later clears the token.
#[test]
fn headless_coalesced_enter_esc_interrupt_survives() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = SessionConfig {
        model: "claude-sonnet-5".into(),
        max_tokens: 32_000,
        system: Some("test system".into()),
        thinking: false,
        cwd: dir.path().to_path_buf(),
        max_iterations: 50,
        temperature: None,
        top_p: None,
        context_window: None,
    };
    let mut session = Session::new(Box::new(BlockUntilCancelled), Registry::standard(), cfg);

    let mut script: Vec<Event> = "race me"
        .chars()
        .map(|c| Event::Key(key(KeyCode::Char(c))))
        .collect();
    script.push(Event::Key(key(KeyCode::Enter)));
    script.push(Event::Key(key(KeyCode::Esc)));

    let (mut ui, snapshot) = TuiUi::headless(
        SessionInfo {
            model: "claude-sonnet-5".into(),
            thinking: false,
            cwd: dir.path().display().to_string(),
            version: "test".into(),
        },
        100,
        30,
        script,
        session.cancel_token(),
    );

    let line = ui.read_input().expect("scripted submit reaches read_input");
    session.turn(&line, &mut |ev| ui.event(&ev)).unwrap();
    drop(ui);

    let rows = snapshot.lock().unwrap().clone();
    let body = rows.join("\n");
    assert!(body.contains("turn interrupted"), "interrupt survives:\n{body}");
    assert!(body.contains("partial tail"), "kept partial:\n{body}");
}

/// The full interrupt chain, headless: scripted prompt + Enter + Esc; the
/// Esc must unblock the agent thread via the shared token, land the turn,
/// and return to the prompt.
#[test]
fn headless_esc_interrupts_a_blocked_turn_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = SessionConfig {
        model: "claude-sonnet-5".into(),
        max_tokens: 32_000,
        system: Some("test system".into()),
        thinking: false,
        cwd: dir.path().to_path_buf(),
        max_iterations: 50,
        temperature: None,
        top_p: None,
        context_window: None,
    };
    let mut session = Session::new(Box::new(BlockUntilCancelled), Registry::standard(), cfg);

    let mut script: Vec<Event> = "interrupt me"
        .chars()
        .map(|c| Event::Key(key(KeyCode::Char(c))))
        .collect();
    script.push(Event::Key(key(KeyCode::Enter)));
    script.push(Event::Key(key(KeyCode::Esc)));

    let (mut ui, snapshot) = TuiUi::headless(
        SessionInfo {
            model: "claude-sonnet-5".into(),
            thinking: false,
            cwd: dir.path().display().to_string(),
            version: "test".into(),
        },
        100,
        30,
        script,
        session.cancel_token(),
    );

    let line = ui.read_input().expect("scripted submit reaches read_input");
    session.turn(&line, &mut |ev| ui.event(&ev)).unwrap();
    drop(ui); // shutdown drains all pending events into the final frame

    let rows = snapshot.lock().unwrap().clone();
    let body = rows.join("\n");
    assert!(body.contains("partial tail"), "kept partial:\n{body}");
    assert!(body.contains("turn interrupted"), "notice:\n{body}");
    assert!(body.contains("▣"), "turn tail rendered:\n{body}");
    assert!(!body.contains("interrupting…"), "state cleared:\n{body}");
}

// ------------------------------------------------------------ T8: commands

#[test]
fn submit_command_no_title_no_user_cell_no_busy_and_recallable() {
    let mut a = app();
    a.submit_command("/status");
    assert_eq!(a.cells.last(), Some(&Cell::Command("/status".into())));
    assert!(a.title.is_none(), "commands never claim the title");
    assert!(!a.busy, "commands never spin");
    assert!(!a.cells.iter().any(|c| matches!(c, Cell::User(_))));
    // Recallable via ↑ like any input.
    a.handle_key(key(KeyCode::Up));
    assert_eq!(a.input, "/status");
}

#[test]
fn fold_model_switched_and_thinking_changed_update_chrome() {
    let mut a = app();
    a.fold(&AgentEvent::ModelSwitched { model: "model-b".into() });
    assert_eq!(a.model, "model-b");
    a.fold(&AgentEvent::ThinkingChanged(true));
    assert!(a.thinking);
    let rows = render(&mut a, 90, 12);
    let body = rows.join("\n");
    assert!(body.contains("model-b"), "header/footer chrome:\n{body}");
    assert!(body.contains("thinking on"), "footer chrome:\n{body}");
}

#[test]
fn fold_session_cleared_resets_transcript_title_and_usage() {
    let mut a = app();
    a.submit("hello");
    a.fold(&AgentEvent::TextDelta("answer".into()));
    a.fold(&AgentEvent::TurnComplete {
        turn_usage: usage(5, 7),
        session_usage: usage(5, 7),
    });
    assert!(a.title.is_some());
    a.fold(&AgentEvent::SessionCleared);
    a.fold(&AgentEvent::Notice("session cleared".into()));
    assert_eq!(a.cells.len(), 1, "only the post-clear notice survives");
    assert!(a.title.is_none());
    assert_eq!(a.session_usage, Usage::default());
    let rows = render(&mut a, 80, 12);
    let body = rows.join("\n");
    assert!(body.contains("new session"), "{body}");
    assert!(!body.contains("hello"), "{body}");
    assert!(body.contains("session cleared"), "{body}");
}

#[test]
fn frame_command_cell_renders_as_dim_line_not_user_block() {
    let mut a = app();
    a.submit_command("/model local");
    a.fold(&AgentEvent::Notice(
        "switched to local (openai-compat · qwen3-1.7b)".into(),
    ));
    let rows = render(&mut a, 90, 10);
    let body = rows.join("\n");
    assert!(body.contains("/model local"), "{body}");
    assert!(body.contains("switched to local"), "{body}");
    assert!(!body.contains("▌ /model"), "no user-block bar: {body}");
    assert!(rows[0].contains("new session"), "no title from a command: {body}");
}

/// The real render-thread Submit arm routes `/`-lines through
/// `submit_command` (no busy, no title, no User cell) and still forwards
/// them to the agent thread. The test plays the driver-loop role exactly as
/// main.rs does: read_input → commands::run → events back through the seam.
#[test]
fn headless_command_flow_status_leaves_title_alone() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = SessionConfig {
        model: "claude-sonnet-5".into(),
        max_tokens: 32_000,
        system: Some("test system".into()),
        thinking: false,
        cwd: dir.path().to_path_buf(),
        max_iterations: 50,
        temperature: None,
        top_p: None,
        context_window: None,
    };
    let mut session = Session::new(
        Box::new(BlockUntilCancelled), // never called: only commands run
        Registry::standard(),
        cfg,
    );

    // The final "exit" makes the Submit arm send None, ending the driver
    // loop below — without it read_input would block forever.
    let mut script: Vec<Event> = Vec::new();
    for line in ["/status", "exit"] {
        script.extend(line.chars().map(|c| Event::Key(key(KeyCode::Char(c)))));
        script.push(Event::Key(key(KeyCode::Enter)));
    }

    let (mut ui, snapshot) = TuiUi::headless(
        SessionInfo {
            model: "claude-sonnet-5".into(),
            thinking: false,
            cwd: dir.path().display().to_string(),
            version: "test".into(),
        },
        100,
        30,
        script,
        session.cancel_token(),
    );

    let mut profiles = std::collections::BTreeMap::new();
    profiles.insert(
        "sonnet-next".to_string(),
        temur::config::ResolvedProfile {
            provider: "anthropic".into(),
            model: "sonnet-next".into(),
            base_url: "https://mock.invalid".into(),
            api_key_file: None,
            max_tokens: 32_000,
            context_window: None,
        },
    );
    let mut active: Option<String> = None;
    let mut provider_name = "anthropic".to_string();
    let mut model = "claude-sonnet-5".to_string();
    let build = |_: &temur::config::ResolvedProfile| -> Result<
        Box<dyn Provider>,
        temur::error::Error,
    > { unreachable!("/status builds nothing") };

    while let Some(line) = ui.read_input() {
        assert!(line.starts_with('/'), "script only sends commands");
        let mut ctx = temur::commands::CommandCtx {
            session: &mut session,
            profiles: &profiles,
            active_profile: &mut active,
            provider_name: &mut provider_name,
            model: &mut model,
            persist_path: None,
            session_max_bytes: temur::config::DEFAULT_SESSION_MAX_BYTES,
            cwd_display: "/test",
            replay_mode: false,
            build_provider: &build,
        };
        for ev in temur::commands::run(temur::commands::parse(&line), &mut ctx) {
            ui.event(&ev);
        }
    }
    drop(ui);

    let rows = snapshot.lock().unwrap().clone();
    let body = rows.join("\n");
    assert!(rows[0].contains("new session"), "no title from commands:\n{body}");
    assert!(body.contains("/status"), "dim command echo:\n{body}");
    assert!(body.contains("no usage reported yet"), "status output:\n{body}");
    assert!(!body.contains("▌ /status"), "never a user cell:\n{body}");
}

/// Full switch-then-clear flow through the real runtime: `/model` updates
/// the header chrome; `/clear` wipes the transcript (including the command
/// echoes) and resets the title; the post-clear notice survives.
#[test]
fn headless_command_flow_switch_updates_chrome_and_clear_resets() {
    let dir = tempfile::tempdir().unwrap();
    let provider = AnthropicProvider::new(
        "https://mock.invalid",
        "mock-key".into(),
        Box::new(ReplayTransport::new(vec![format!(
            "{}/tests/fixtures/text_simple.sse",
            env!("CARGO_MANIFEST_DIR")
        )
        .into()])),
    );
    let cfg = SessionConfig {
        model: "claude-sonnet-5".into(),
        max_tokens: 32_000,
        system: Some("test system".into()),
        thinking: false,
        cwd: dir.path().to_path_buf(),
        max_iterations: 50,
        temperature: None,
        top_p: None,
        context_window: None,
    };
    let mut session = Session::new(Box::new(provider), Registry::standard(), cfg);

    let mut script: Vec<Event> = Vec::new();
    for line in ["hi", "/model sonnet-next", "/clear", "exit"] {
        script.extend(line.chars().map(|c| Event::Key(key(KeyCode::Char(c)))));
        script.push(Event::Key(key(KeyCode::Enter)));
    }

    let (mut ui, snapshot) = TuiUi::headless(
        SessionInfo {
            model: "claude-sonnet-5".into(),
            thinking: false,
            cwd: dir.path().display().to_string(),
            version: "test".into(),
        },
        100,
        30,
        script,
        session.cancel_token(),
    );

    let mut profiles = std::collections::BTreeMap::new();
    profiles.insert(
        "sonnet-next".to_string(),
        temur::config::ResolvedProfile {
            provider: "anthropic".into(),
            model: "sonnet-next".into(),
            base_url: "https://mock.invalid".into(),
            api_key_file: None,
            max_tokens: 32_000,
            context_window: None,
        },
    );
    let mut active: Option<String> = None;
    let mut provider_name = "anthropic".to_string();
    let mut model = "claude-sonnet-5".to_string();
    let build = |p: &temur::config::ResolvedProfile| -> Result<
        Box<dyn Provider>,
        temur::error::Error,
    > {
        // The switched-to provider is never exercised in this test; a
        // blocking stand-in proves construction happened without a network.
        assert_eq!(p.model, "sonnet-next");
        Ok(Box::new(BlockUntilCancelled))
    };

    while let Some(line) = ui.read_input() {
        if line.starts_with('/') {
            let mut ctx = temur::commands::CommandCtx {
                session: &mut session,
                profiles: &profiles,
                active_profile: &mut active,
                provider_name: &mut provider_name,
                model: &mut model,
                persist_path: None,
                session_max_bytes: temur::config::DEFAULT_SESSION_MAX_BYTES,
                cwd_display: "/test",
                replay_mode: false,
                build_provider: &build,
            };
            for ev in temur::commands::run(temur::commands::parse(&line), &mut ctx) {
                ui.event(&ev);
            }
        } else {
            session.turn(&line, &mut |ev| ui.event(&ev)).unwrap();
        }
    }
    drop(ui);

    assert_eq!(active.as_deref(), Some("sonnet-next"));
    assert_eq!(session.model(), "sonnet-next");
    assert!(session.history().is_empty(), "cleared");

    let rows = snapshot.lock().unwrap().clone();
    let body = rows.join("\n");
    assert!(rows[0].contains("sonnet-next"), "header chrome switched:\n{body}");
    assert!(rows[0].contains("new session"), "title reset by /clear:\n{body}");
    assert!(body.contains("session cleared"), "post-clear notice:\n{body}");
    assert!(!body.contains("Hello, world!"), "turn output wiped:\n{body}");
    assert!(!body.contains("/model sonnet-next"), "command echoes wiped too:\n{body}");
}

// ------------------------------------------------ T8-P2: markdown rendering

#[test]
fn frame_markdown_sample_at_two_widths() {
    let sample = "## Plan\n\nFirst `cargo build`, then:\n\n- fix the *parser*\n- run **all** tests\n\n```rust\nfn main() {}\n```";
    let mut a = app();
    a.submit("plan it");
    a.fold(&AgentEvent::TextDelta(sample.into()));

    // Wide: every construct on its own row (leading column is the margin).
    let rows = render(&mut a, 46, 22);
    let body = rows.join("\n");
    for expect in [
        "    Plan",
        "    First cargo build, then:",
        "    • fix the parser",
        "    • run all tests",
        "    ▌ rust",
        "    ▌ fn main() {}",
    ] {
        assert!(rows.iter().any(|r| r == expect), "missing {expect:?}:\n{body}");
    }
    // Styles land where they should: heading bold+underlined, inline code
    // cyan, gutter dim (buffer probe like the red-✗ check above).
    let backend = TestBackend::new(46, 22);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(&mut a, f)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let y = rows.iter().position(|r| r == "    Plan").unwrap() as u16;
    let style = buf[(4, y)].style();
    assert!(style.add_modifier.contains(ratatui::style::Modifier::BOLD));
    assert!(style.add_modifier.contains(ratatui::style::Modifier::UNDERLINED));
    let y = rows.iter().position(|r| r.contains("First cargo")).unwrap() as u16;
    let x = rows[y as usize].find("cargo").unwrap() as u16;
    assert_eq!(buf[(x, y)].style().fg, Some(ratatui::style::Color::Cyan));
    let y = rows.iter().position(|r| r.contains("▌ rust")).unwrap() as u16;
    let x = rows[y as usize].find('▌').unwrap() as u16;
    assert!(buf[(x, y)].style().add_modifier.contains(ratatui::style::Modifier::DIM));

    // Narrow: same content wraps with hanging indent, code stays verbatim.
    let rows = render(&mut a, 20, 24);
    let body = rows.join("\n");
    for expect in [
        "    Plan",
        "    First cargo",
        "    build, then:",
        "    • fix the",
        "      parser",
        "    • run all tests",
        "    ▌ rust",
        "    ▌ fn main() {}",
    ] {
        assert!(rows.iter().any(|r| r == expect), "missing {expect:?}:\n{body}");
    }
}

#[test]
fn markdown_applies_only_to_assistant_cells() {
    let mut a = app();
    a.submit("**stars** stay `raw` here");
    a.fold(&AgentEvent::Notice("*not* markdown".into()));
    a.fold(&AgentEvent::TextDelta("**is** markdown".into()));
    let rows = render(&mut a, 60, 12);
    let body = rows.join("\n");
    assert!(body.contains("▌ **stars** stay `raw` here"), "user verbatim:\n{body}");
    assert!(body.contains("[!] *not* markdown"), "notice verbatim:\n{body}");
    assert!(!body.contains("**is**"), "assistant cell renders markdown:\n{body}");
    assert!(body.contains("is markdown"), "bold text survives:\n{body}");
}

/// Documented limitation: a tool call mid-reply splits one logical reply
/// across AssistantText cells, and each cell re-parses alone. A fence
/// severed by the split renders its opener-cell as code (unclosed fence →
/// code to end of cell) while the closer's cell re-parses from scratch:
/// prose until the orphan ```, which opens a NEW fence swallowing the
/// tail as code. Nothing panics and nothing is lost.
#[test]
fn severed_fence_across_cells_renders_without_panic() {
    let mut a = app();
    a.submit("go");
    a.fold(&AgentEvent::TextDelta("Setup:\n\n```rust\nlet a = 1;".into()));
    a.fold(&AgentEvent::ToolStart { name: "read".into() });
    a.fold(&AgentEvent::ToolEnd {
        name: "read".into(),
        title: "f.rs".into(),
        is_error: false,
    });
    a.fold(&AgentEvent::TextDelta("let b = 2;\n```\n\nafter".into()));
    let rows = render(&mut a, 50, 24);
    let body = rows.join("\n");
    // Opener cell: unclosed fence renders as a code block (gutter + lang).
    assert!(rows.iter().any(|r| r.contains("▌ rust")), "{body}");
    assert!(rows.iter().any(|r| r.contains("▌ let a = 1;")), "{body}");
    // Closer cell head renders as prose (no gutter)…
    assert!(
        rows.iter().any(|r| r.trim_start().starts_with("let b = 2;") && !r.contains('▌')),
        "{body}"
    );
    // …and the orphan closer opens a new fence: the tail renders as code.
    assert!(rows.iter().any(|r| r.contains("▌ after")), "{body}");
}

/// Headless e2e over a markdown-bearing fixture: deltas split mid-word and
/// mid-fence accumulate into ONE cell and the final frame renders the
/// parsed markdown, not the raw text.
#[test]
fn headless_markdown_fixture_renders_in_final_frame() {
    let dir = tempfile::tempdir().unwrap();
    let provider = AnthropicProvider::new(
        "https://mock.invalid",
        "mock-key".into(),
        Box::new(ReplayTransport::new(vec![format!(
            "{}/tests/fixtures/markdown_sample.sse",
            env!("CARGO_MANIFEST_DIR")
        )
        .into()])),
    );
    let cfg = SessionConfig {
        model: "claude-sonnet-5".into(),
        max_tokens: 32_000,
        system: Some("test system".into()),
        thinking: false,
        cwd: dir.path().to_path_buf(),
        max_iterations: 50,
        temperature: None,
        top_p: None,
        context_window: None,
    };
    let mut session = Session::new(Box::new(provider), Registry::standard(), cfg);

    let mut script: Vec<Event> = "show the plan"
        .chars()
        .map(|c| Event::Key(key(KeyCode::Char(c))))
        .collect();
    script.push(Event::Key(key(KeyCode::Enter)));

    let (mut ui, snapshot) = TuiUi::headless(
        SessionInfo {
            model: "claude-sonnet-5".into(),
            thinking: false,
            cwd: dir.path().display().to_string(),
            version: "test".into(),
        },
        60,
        30,
        script,
        session.cancel_token(),
    );

    let line = ui.read_input().expect("scripted submit reaches read_input");
    session.turn(&line, &mut |ev| ui.event(&ev)).unwrap();
    drop(ui);

    let rows = snapshot.lock().unwrap().clone();
    let body = rows.join("\n");
    assert!(rows.iter().any(|r| r.trim_end() == "    Plan"), "heading:\n{body}");
    assert!(body.contains("First cargo build, then:"), "inline code text:\n{body}");
    assert!(body.contains("• fix the parser"), "bullet:\n{body}");
    assert!(body.contains("• run all tests"), "bold text flattened:\n{body}");
    assert!(body.contains("▌ rust"), "fence lang gutter:\n{body}");
    assert!(body.contains("▌ fn main() {}"), "fence body:\n{body}");
    assert!(!body.contains("```"), "no raw fence markers:\n{body}");
    assert!(!body.contains("**all**"), "no raw emphasis markers:\n{body}");
    assert!(body.contains("▣ temur · claude-sonnet-5"), "turn tail:\n{body}");
}
