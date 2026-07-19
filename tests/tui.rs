//! TUI offline tests, layers 1–2 of the milestone-B test strategy:
//! event-fold tests over App, and frame snapshots via ratatui's TestBackend.
//! No terminal, no threads, no network — runs identically on host and in the
//! i386 container.

use opencode_rust::agent::events::AgentEvent;
use opencode_rust::agent::{Session, SessionConfig};
use opencode_rust::provider::anthropic::transport::ReplayTransport;
use opencode_rust::provider::anthropic::AnthropicProvider;
use opencode_rust::provider::Usage;
use opencode_rust::tools::Registry;
use opencode_rust::ui::tui::app::{Action, App, Cell};
use opencode_rust::ui::tui::view::draw;
use opencode_rust::ui::tui::{SessionInfo, TuiUi};
use opencode_rust::ui::Ui;
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
        input_tokens: input,
        output_tokens: output,
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
    assert_eq!(a.session_usage.input_tokens, 110);
    assert!(
        matches!(a.cells.last(), Some(Cell::TurnTail { secs: 3, usage: u }) if u.output_tokens == 20)
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
    assert!(rows[0].contains("claude-sonnet-5 · opencode-rust 0.1.0"));
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
    assert!(body.contains("▣ opencode · claude-sonnet-5 · 2s · 12 in / 34 out"), "tail:\n{body}");
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
    for i in 0..30 {
        a.fold(&AgentEvent::TextDelta(format!("line {i}\n")));
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
    assert!(body.contains("▣ opencode · claude-sonnet-5"), "turn tail:\n{body}");
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
