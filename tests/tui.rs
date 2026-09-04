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
        matches!(a.cells.last(), Some(Cell::TurnTail { secs: 3, usage: u, .. }) if u.output_tokens == Some(20))
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
    assert!(a.force_quit_armed());
    assert_eq!(a.handle_key(ctrl('c')), Action::ForceQuit);

    // T43/P3: intervening keys no longer disarm; the window does.
    a.prompt_open();
    a.submit("second");
    assert_eq!(a.handle_key(ctrl('c')), Action::None);
    a.handle_key(key(KeyCode::Char('x')));
    assert!(a.force_quit_armed(), "a typed char does not disarm");
    assert_eq!(a.handle_key(ctrl('c')), Action::ForceQuit);

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
    a.fold(&AgentEvent::Notice(
        "response truncated: max_tokens (800, from config) reached; raise max_tokens in config.json".into(),
    ));
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
fn turn_tails_keep_the_model_they_ran_on_across_a_switch() {
    // T13 finding 2: the footer used to be formatted from the CURRENT model
    // at draw time, so a /model hop relabeled every past turn in the
    // scrollback. Each tail describes the turn that produced it.
    let mut a = app();
    a.now_ms = 0;
    a.submit("first");
    a.fold(&AgentEvent::TurnComplete {
        turn_usage: usage(12, 34),
        session_usage: usage(12, 34),
    });
    a.fold(&AgentEvent::ModelSwitched {
        model: "claude-opus-5".into(),
        provider: "anthropic".into(),
    });
    a.submit("second");
    a.fold(&AgentEvent::TurnComplete {
        turn_usage: usage(56, 78),
        session_usage: usage(68, 112),
    });
    let body = render(&mut a, 80, 20).join("\n");
    assert!(
        body.contains("▣ temur · claude-sonnet-5 · 0s · 12 in / 34 out"),
        "the first tail still names the model it ran on:\n{body}"
    );
    assert!(
        body.contains("▣ temur · claude-opus-5 · 0s · 56 in / 78 out"),
        "the second tail names the switched-to model:\n{body}"
    );
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
        max_tokens_source: None,
        prose_tool_calls: true,
        cost_rates: None,
        cost_advisory_step_usd: temur::config::DEFAULT_COST_ADVISORY_STEP_USD,
        auto_compact: false,
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
            profiles: vec![],
            provider: "anthropic".into(),
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
fn esc_between_two_ctrl_c_no_longer_defeats_force_quit() {
    // T43/P3 flips this case: Esc used to disarm, so the second Ctrl+C only
    // re-armed. Inside the window it now force-quits, interrupting or not.
    let mut a = app();
    a.submit("go");
    assert_eq!(a.handle_key(ctrl('c')), Action::None); // arms
    assert!(a.force_quit_armed());
    assert_eq!(a.handle_key(key(KeyCode::Esc)), Action::Interrupt);
    assert!(a.force_quit_armed(), "Esc does not disarm any more");
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
        max_tokens_source: None,
        prose_tool_calls: true,
        cost_rates: None,
        cost_advisory_step_usd: temur::config::DEFAULT_COST_ADVISORY_STEP_USD,
        auto_compact: false,
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
            profiles: vec![],
            provider: "anthropic".into(),
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
        max_tokens_source: None,
        prose_tool_calls: true,
        cost_rates: None,
        cost_advisory_step_usd: temur::config::DEFAULT_COST_ADVISORY_STEP_USD,
        auto_compact: false,
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
            profiles: vec![],
            provider: "anthropic".into(),
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
        max_tokens_source: None,
        prose_tool_calls: true,
        cost_rates: None,
        cost_advisory_step_usd: temur::config::DEFAULT_COST_ADVISORY_STEP_USD,
        auto_compact: false,
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
            profiles: vec![],
            provider: "anthropic".into(),
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

/// T16: a switch that changes the provider drops the `/models`-cached ids
/// (a llama.cpp listing must never complete anthropic ids); a same-provider
/// switch keeps them.
#[test]
fn fold_provider_change_clears_cached_model_ids_same_provider_keeps() {
    let mut a = app();
    a.provider = "openai-compat".into();
    a.fold(&AgentEvent::ModelsListed(vec!["qwen3-1.7b".into(), "qwen3-4b".into()]));
    assert_eq!(a.model_ids.len(), 2);
    // Raw switch on the SAME provider: cache survives.
    a.fold(&AgentEvent::ModelSwitched {
        model: "qwen3-4b".into(),
        provider: "openai-compat".into(),
    });
    assert_eq!(a.model_ids.len(), 2, "same-provider switch keeps the cache");
    // Profile switch to another provider: cache dropped, baseline updated.
    a.fold(&AgentEvent::ModelSwitched {
        model: "claude-sonnet-5".into(),
        provider: "anthropic".into(),
    });
    assert!(a.model_ids.is_empty(), "provider change drops the cache");
    assert_eq!(a.provider, "anthropic");
    // And switching back does NOT resurrect anything.
    a.fold(&AgentEvent::ModelSwitched {
        model: "qwen3-4b".into(),
        provider: "openai-compat".into(),
    });
    assert!(a.model_ids.is_empty());
}

#[test]
fn fold_model_switched_and_thinking_changed_update_chrome() {
    let mut a = app();
    a.fold(&AgentEvent::ModelSwitched { model: "model-b".into(), provider: String::new() });
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
        max_tokens_source: None,
        prose_tool_calls: true,
        cost_rates: None,
        cost_advisory_step_usd: temur::config::DEFAULT_COST_ADVISORY_STEP_USD,
        auto_compact: false,
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
            profiles: vec![],
            provider: "anthropic".into(),
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
            prompt_profile: temur::tools::PromptProfile::Full,
            prompt_profile_source: Default::default(),
            price_input_per_mtok: None,
            price_output_per_mtok: None,
            max_tokens_parameter: Default::default(),
        },
    );
    let mut active: Option<String> = None;
    let mut provider_name = "anthropic".to_string();
    let mut model = "claude-sonnet-5".to_string();
    let mut prompt_profile = temur::tools::PromptProfile::Full;
    let mut persist_path: Option<std::path::PathBuf> = None;
    let mut session_name: Option<String> = None;
    let rebuild = |_: temur::tools::PromptProfile| -> String { "test system".into() };
    let mut active_resolved = temur::config::ResolvedProfile {
        provider: "anthropic".into(),
        model: "claude-sonnet-5".into(),
        base_url: "https://mock.invalid".into(),
        api_key_file: None,
        max_tokens: 32_000,
        context_window: None,
        prompt_profile: temur::tools::PromptProfile::Full,
        prompt_profile_source: Default::default(),
        price_input_per_mtok: None,
        price_output_per_mtok: None,
        max_tokens_parameter: Default::default(),
    };
    let list = |_: &temur::config::ResolvedProfile| -> Result<
        Vec<temur::provider::ModelEntry>,
        temur::error::Error,
    > {
        unreachable!("no /models in this script")
    };
    let build = |_: &temur::config::ResolvedProfile| -> Result<
        Box<dyn Provider>,
        temur::error::Error,
    > { unreachable!("/status builds nothing") };

    let mut cached_models: Vec<temur::provider::ModelEntry> = Vec::new();
    while let Some(line) = ui.read_input() {
        assert!(line.starts_with('/'), "script only sends commands");
        let mut ctx = temur::commands::CommandCtx {
            session: &mut session,
            profiles: &profiles,
            active_profile: &mut active,
            provider_name: &mut provider_name,
            model: &mut model,
            persist_path: &mut persist_path,
            session_max_bytes: temur::config::DEFAULT_SESSION_MAX_BYTES,
            sessions_dir: std::path::Path::new("/nonexistent/temur-test-sessions"),
            cwd: std::path::Path::new("/test"),
            cwd_display: "/test",
            session_name: &mut session_name,
            replay_mode: false,
            prompt_profile: &mut prompt_profile,
            active_resolved: &mut active_resolved,
            config_path: std::path::Path::new("/nonexistent/temur-test-config.json"),
            cached_models: &mut cached_models,
            build_provider: &build,
            list_models: &list,
            rebuild_system: &rebuild,
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
        max_tokens_source: None,
        prose_tool_calls: true,
        cost_rates: None,
        cost_advisory_step_usd: temur::config::DEFAULT_COST_ADVISORY_STEP_USD,
        auto_compact: false,
    };
    let mut session = Session::new(Box::new(provider), Registry::standard(), cfg);

    // P3: readiness-gated steps, NOT raw events. The raw source delivers
    // with zero delay, so under machine load the "/model" line's Enter
    // could land while the "hi" turn was still busy and be dropped (the
    // deliberate busy-Enter rule), merging lines or blocking the driver
    // below forever. A Line step starts only once the app is idle.
    let steps: Vec<temur::ui::tui::ScriptStep> = ["hi", "/model sonnet-next", "/clear", "exit"]
        .into_iter()
        .map(|l| temur::ui::tui::ScriptStep::Line(l.into()))
        .collect();

    let (mut ui, snapshot) = TuiUi::headless_steps(
        SessionInfo {
            model: "claude-sonnet-5".into(),
            thinking: false,
            cwd: dir.path().display().to_string(),
            version: "test".into(),
            profiles: vec![],
            provider: "anthropic".into(),
        },
        100,
        30,
        steps,
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
            prompt_profile: temur::tools::PromptProfile::Full,
            prompt_profile_source: Default::default(),
            price_input_per_mtok: None,
            price_output_per_mtok: None,
            max_tokens_parameter: Default::default(),
        },
    );
    let mut active: Option<String> = None;
    let mut provider_name = "anthropic".to_string();
    let mut model = "claude-sonnet-5".to_string();
    let mut prompt_profile = temur::tools::PromptProfile::Full;
    let mut persist_path: Option<std::path::PathBuf> = None;
    let mut session_name: Option<String> = None;
    let rebuild = |_: temur::tools::PromptProfile| -> String { "test system".into() };
    let mut active_resolved = temur::config::ResolvedProfile {
        provider: "anthropic".into(),
        model: "claude-sonnet-5".into(),
        base_url: "https://mock.invalid".into(),
        api_key_file: None,
        max_tokens: 32_000,
        context_window: None,
        prompt_profile: temur::tools::PromptProfile::Full,
        prompt_profile_source: Default::default(),
        price_input_per_mtok: None,
        price_output_per_mtok: None,
        max_tokens_parameter: Default::default(),
    };
    let list = |_: &temur::config::ResolvedProfile| -> Result<
        Vec<temur::provider::ModelEntry>,
        temur::error::Error,
    > {
        unreachable!("no /models in this script")
    };
    let build = |p: &temur::config::ResolvedProfile| -> Result<
        Box<dyn Provider>,
        temur::error::Error,
    > {
        // The switched-to provider is never exercised in this test; a
        // blocking stand-in proves construction happened without a network.
        assert_eq!(p.model, "sonnet-next");
        Ok(Box::new(BlockUntilCancelled))
    };

    let mut cached_models: Vec<temur::provider::ModelEntry> = Vec::new();
    while let Some(line) = ui.read_input() {
        if line.starts_with('/') {
            let mut ctx = temur::commands::CommandCtx {
                session: &mut session,
                profiles: &profiles,
                active_profile: &mut active,
                provider_name: &mut provider_name,
                model: &mut model,
                persist_path: &mut persist_path,
                session_max_bytes: temur::config::DEFAULT_SESSION_MAX_BYTES,
                sessions_dir: std::path::Path::new("/nonexistent/temur-test-sessions"),
                cwd: std::path::Path::new("/test"),
                cwd_display: "/test",
                session_name: &mut session_name,
                replay_mode: false,
                prompt_profile: &mut prompt_profile,
                active_resolved: &mut active_resolved,
                config_path: std::path::Path::new("/nonexistent/temur-test-config.json"),
                cached_models: &mut cached_models,
                build_provider: &build,
                list_models: &list,
                rebuild_system: &rebuild,
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
        max_tokens_source: None,
        prose_tool_calls: true,
        cost_rates: None,
        cost_advisory_step_usd: temur::config::DEFAULT_COST_ADVISORY_STEP_USD,
        auto_compact: false,
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
            profiles: vec![],
            provider: "anthropic".into(),
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

// -------------------------------------------------- T9: /models in the TUI

#[test]
fn models_listed_folds_to_a_cell_and_caches_completion_ids() {
    let mut a = app();
    assert!(a.model_ids.is_empty());
    a.fold(&AgentEvent::ModelsListed(vec!["alpha-1".into(), "beta-2".into()]));
    assert_eq!(a.model_ids, vec!["alpha-1".to_string(), "beta-2".to_string()]);
    assert_eq!(
        a.cells,
        vec![Cell::Models(vec!["alpha-1".into(), "beta-2".into()])]
    );
    // A fresh listing REPLACES the cache (session-lifetime, last wins).
    a.fold(&AgentEvent::ModelsListed(vec!["gamma-3".into()]));
    assert_eq!(a.model_ids, vec!["gamma-3".to_string()]);
}

#[test]
fn frame_models_listing_renders_notice_style() {
    let mut a = app();
    a.submit_command("/models");
    a.fold(&AgentEvent::ModelsListed(vec!["alpha-1".into(), "beta-2".into()]));
    let rows = render(&mut a, 80, 12);
    let body = rows.join("\n");
    assert!(body.contains("/models"), "command echo:\n{body}");
    assert!(body.contains("▌ 2 model id(s) from the provider:"), "count line:\n{body}");
    assert!(body.contains("▌   alpha-1"), "first id:\n{body}");
    assert!(body.contains("▌   beta-2"), "second id:\n{body}");

    // Notice-style = yellow bar, like Cell::Notice.
    let backend = TestBackend::new(80, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(&mut a, f)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let y = rows.iter().position(|r| r.contains("alpha-1")).unwrap() as u16;
    let x = rows[y as usize].find('▌').unwrap() as u16;
    assert_eq!(buf[(x, y)].style().fg, Some(ratatui::style::Color::Yellow));
}

// ------------------------------------- T9: Tab completion + command styling

/// App with completion sources set, mirroring what the runtime threads in.
fn app_with_completion() -> App {
    let mut a = app();
    a.profiles = vec!["local".into(), "sonnet".into()];
    a.model_ids = vec!["qwen3-1.7b".into()];
    a
}

#[test]
fn tab_applies_a_unique_completion_at_end_of_input() {
    let mut a = app_with_completion();
    type_str(&mut a, "/sta");
    a.handle_key(key(KeyCode::Tab));
    assert_eq!(a.input, "/status");
    assert_eq!(a.cursor, a.input.len());
}

#[test]
fn tab_cycles_wraps_and_backtab_reverses() {
    let mut a = app_with_completion();
    type_str(&mut a, "/model ");
    a.handle_key(key(KeyCode::Tab));
    assert_eq!(a.input, "/model local", "profiles first");
    a.handle_key(key(KeyCode::Tab));
    assert_eq!(a.input, "/model sonnet");
    a.handle_key(key(KeyCode::Tab));
    assert_eq!(a.input, "/model qwen3-1.7b", "then cached ids");
    a.handle_key(key(KeyCode::Tab));
    assert_eq!(a.input, "/model local", "cycle wraps");
    a.handle_key(key(KeyCode::BackTab));
    assert_eq!(a.input, "/model qwen3-1.7b", "backtab reverses across the wrap");
    a.handle_key(key(KeyCode::BackTab));
    assert_eq!(a.input, "/model sonnet");
}

#[test]
fn edits_and_history_moves_invalidate_the_cycle() {
    let mut a = app_with_completion();
    // An edit invalidates: the next Tab recomputes from the NEW input.
    type_str(&mut a, "/model ");
    a.handle_key(key(KeyCode::Tab));
    assert_eq!(a.input, "/model local");
    type_str(&mut a, "x"); // "/model localx" — no candidates now
    a.handle_key(key(KeyCode::Tab));
    assert_eq!(a.input, "/model localx", "recompute found nothing: no-op");

    // History moves invalidate too, and completion never corrupts
    // history state: ↑ recalls the submitted line, ↓ restores the draft.
    let mut a = app_with_completion();
    a.submit("first prompt");
    a.fold(&AgentEvent::TurnComplete {
        turn_usage: Usage::default(),
        session_usage: Usage::default(),
    });
    type_str(&mut a, "/sta");
    a.handle_key(key(KeyCode::Tab));
    assert_eq!(a.input, "/status");
    a.handle_key(key(KeyCode::Up));
    assert_eq!(a.input, "first prompt", "history recall after Tab");
    a.handle_key(key(KeyCode::Down));
    assert_eq!(a.input, "/status", "draft (the completed line) restored");
}

#[test]
fn tab_is_a_noop_without_candidates_mid_input_or_while_busy() {
    // No candidates.
    let mut a = app_with_completion();
    type_str(&mut a, "hello");
    a.handle_key(key(KeyCode::Tab));
    assert_eq!(a.input, "hello");
    // Unknown command head.
    let mut a = app_with_completion();
    type_str(&mut a, "/zzz");
    a.handle_key(key(KeyCode::Tab));
    assert_eq!(a.input, "/zzz");
    // Cursor not at end-of-input.
    let mut a = app_with_completion();
    type_str(&mut a, "/sta");
    a.handle_key(key(KeyCode::Left));
    a.handle_key(key(KeyCode::Tab));
    assert_eq!(a.input, "/sta", "mid-input Tab is a no-op");
    // Busy (a turn is running).
    let mut a = app_with_completion();
    a.submit("go");
    assert!(a.busy);
    type_str(&mut a, "/sta");
    a.handle_key(key(KeyCode::Tab));
    assert_eq!(a.input, "/sta", "busy Tab is a no-op");
}

#[test]
fn tab_does_not_defeat_the_force_quit_window() {
    // T43/P3: Tab used to disarm like any other key. It no longer does.
    let mut a = app_with_completion();
    a.submit("go"); // busy
    assert_eq!(a.handle_key(ctrl('c')), Action::None);
    assert!(a.force_quit_armed());
    a.handle_key(key(KeyCode::Tab));
    assert!(a.force_quit_armed(), "Tab does not disarm");
    assert_eq!(a.handle_key(ctrl('c')), Action::ForceQuit);
}

#[test]
fn frame_command_input_renders_cyan_and_reverts_without_slash() {
    let mut a = app();
    type_str(&mut a, "/x");
    let rows = render(&mut a, 60, 8);
    let y = rows.iter().position(|r| r.contains("▌ > /x")).unwrap() as u16;
    // Column = CHAR position ("▌" is multi-byte; find() returns bytes).
    let x = rows[y as usize].chars().position(|c| c == '/').unwrap() as u16;
    let backend = TestBackend::new(60, 8);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(&mut a, f)).unwrap();
    let buf = terminal.backend().buffer().clone();
    assert_eq!(buf[(x, y)].style().fg, Some(ratatui::style::Color::Cyan));
    assert_eq!(buf[(x + 1, y)].style().fg, Some(ratatui::style::Color::Cyan));

    // Deleting the "/" reverts the styling to default.
    a.handle_key(key(KeyCode::Home));
    a.handle_key(key(KeyCode::Delete));
    assert_eq!(a.input, "x");
    let rows = render(&mut a, 60, 8);
    let y2 = rows.iter().position(|r| r.contains("▌ > x")).unwrap() as u16;
    let x2 = rows[y2 as usize].chars().position(|c| c == 'x').unwrap() as u16;
    let backend = TestBackend::new(60, 8);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(&mut a, f)).unwrap();
    let buf = terminal.backend().buffer().clone();
    assert_eq!(
        buf[(x2, y2)].style().fg,
        Some(ratatui::style::Color::Reset),
        "no accent without the slash"
    );

    // The empty placeholder state is untouched (and stays dim, not cyan).
    let mut a = app();
    let rows = render(&mut a, 90, 8);
    assert!(rows[5].contains("▌ > ask anything… (exit to quit)"));
}

#[test]
fn frame_status_hint_tracks_command_input() {
    // Unique prefix → the full table row.
    let mut a = app();
    type_str(&mut a, "/stat");
    let rows = render(&mut a, 100, 8);
    assert!(
        rows[6].contains("/status — profile, provider, model, thinking, prompt, context, session file"),
        "unique-match hint: {}",
        rows[6]
    );
    // Exact match wins over the /model|/models ambiguity.
    let mut a = app();
    type_str(&mut a, "/model");
    let rows = render(&mut a, 100, 8);
    assert!(rows[6].contains("/model [<profile>|<model-id>]"), "exact match: {}", rows[6]);
    // Multiple matches → names joined.
    let mut a = app();
    type_str(&mut a, "/mod");
    let rows = render(&mut a, 100, 8);
    assert!(rows[6].contains("/model · /models"), "candidates: {}", rows[6]);
    // No match → the /help nudge.
    let mut a = app();
    type_str(&mut a, "/zzz");
    let rows = render(&mut a, 100, 8);
    assert!(rows[6].contains("unknown command — /help"), "{}", rows[6]);
    // Non-command input keeps the standard idle hint; the EMPTY state is
    // already pinned by frame_empty_welcome_state.
    let mut a = app();
    type_str(&mut a, "hello");
    let rows = render(&mut a, 100, 8);
    assert!(rows[6].contains("enter send"), "{}", rows[6]);
}

/// Tab completion through the REAL runtime: the render thread receives a
/// scripted Tab, completes "/sta" to "/status", and the driver loop runs
/// the completed command — proven by the status output in the final frame.
#[test]
fn headless_tab_completion_submits_the_completed_command() {
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
        max_tokens_source: None,
        prose_tool_calls: true,
        cost_rates: None,
        cost_advisory_step_usd: temur::config::DEFAULT_COST_ADVISORY_STEP_USD,
        auto_compact: false,
    };
    let mut session = Session::new(
        Box::new(BlockUntilCancelled), // never called: only commands run
        Registry::standard(),
        cfg,
    );

    let mut script: Vec<Event> = Vec::new();
    script.extend("/sta".chars().map(|c| Event::Key(key(KeyCode::Char(c)))));
    script.push(Event::Key(key(KeyCode::Tab)));
    script.push(Event::Key(key(KeyCode::Enter)));
    script.extend("exit".chars().map(|c| Event::Key(key(KeyCode::Char(c)))));
    script.push(Event::Key(key(KeyCode::Enter)));

    let (mut ui, snapshot) = TuiUi::headless(
        SessionInfo {
            model: "claude-sonnet-5".into(),
            thinking: false,
            cwd: dir.path().display().to_string(),
            version: "test".into(),
            profiles: vec![],
            provider: "anthropic".into(),
        },
        100,
        30,
        script,
        session.cancel_token(),
    );

    let profiles = std::collections::BTreeMap::new();
    let mut active: Option<String> = None;
    let mut provider_name = "anthropic".to_string();
    let mut model = "claude-sonnet-5".to_string();
    let mut prompt_profile = temur::tools::PromptProfile::Full;
    let mut persist_path: Option<std::path::PathBuf> = None;
    let mut session_name: Option<String> = None;
    let rebuild = |_: temur::tools::PromptProfile| -> String { "test system".into() };
    let mut active_resolved = temur::config::ResolvedProfile {
        provider: "anthropic".into(),
        model: "claude-sonnet-5".into(),
        base_url: "https://mock.invalid".into(),
        api_key_file: None,
        max_tokens: 32_000,
        context_window: None,
        prompt_profile: temur::tools::PromptProfile::Full,
        prompt_profile_source: Default::default(),
        price_input_per_mtok: None,
        price_output_per_mtok: None,
        max_tokens_parameter: Default::default(),
    };
    let list = |_: &temur::config::ResolvedProfile| -> Result<
        Vec<temur::provider::ModelEntry>,
        temur::error::Error,
    > {
        unreachable!("no /models in this script")
    };
    let build = |_: &temur::config::ResolvedProfile| -> Result<
        Box<dyn Provider>,
        temur::error::Error,
    > { unreachable!("/status builds nothing") };

    let mut cached_models: Vec<temur::provider::ModelEntry> = Vec::new();
    while let Some(line) = ui.read_input() {
        assert_eq!(line, "/status", "Tab completed the head word before submit");
        let mut ctx = temur::commands::CommandCtx {
            session: &mut session,
            profiles: &profiles,
            active_profile: &mut active,
            provider_name: &mut provider_name,
            model: &mut model,
            persist_path: &mut persist_path,
            session_max_bytes: temur::config::DEFAULT_SESSION_MAX_BYTES,
            sessions_dir: std::path::Path::new("/nonexistent/temur-test-sessions"),
            cwd: std::path::Path::new("/test"),
            cwd_display: "/test",
            session_name: &mut session_name,
            replay_mode: false,
            prompt_profile: &mut prompt_profile,
            active_resolved: &mut active_resolved,
            config_path: std::path::Path::new("/nonexistent/temur-test-config.json"),
            cached_models: &mut cached_models,
            build_provider: &build,
            list_models: &list,
            rebuild_system: &rebuild,
        };
        for ev in temur::commands::run(temur::commands::parse(&line), &mut ctx) {
            ui.event(&ev);
        }
    }
    drop(ui);

    let rows = snapshot.lock().unwrap().clone();
    let body = rows.join("\n");
    assert!(body.contains("/status"), "command echo:\n{body}");
    assert!(body.contains("no usage reported yet"), "status ran:\n{body}");
    assert!(body.contains("prompt: full"), "T9 status field:\n{body}");
}

// ------------------------------------------------------------- T10: sessions

use temur::session_store::ReplayItem;

#[test]
fn sessions_listed_folds_to_a_cell_and_caches_completion_keys() {
    let mut a = app();
    a.fold(&AgentEvent::SessionsListed {
        lines: vec![
            "* (default) · /test · 4 msg(s) · test-1111.json · fix it".into(),
            "  alpha · /other · 2 msg(s) · other-2222-alpha.json".into(),
        ],
        keys: vec!["test-1111.json".into(), "alpha".into()],
    });
    assert!(matches!(&a.cells[0], Cell::Sessions(l) if l.len() == 2));
    assert_eq!(a.session_keys, vec!["test-1111.json".to_string(), "alpha".to_string()]);

    // Tab after "/resume " cycles the cached keys (same machinery as
    // /model ids; complete() is table-tested in commands.rs).
    type_str(&mut a, "/resume ");
    a.handle_key(key(KeyCode::Tab));
    assert_eq!(a.input, "/resume test-1111.json");
    a.handle_key(key(KeyCode::Tab));
    assert_eq!(a.input, "/resume alpha");

    // A fresh listing replaces the cache.
    a.fold(&AgentEvent::SessionsListed {
        lines: vec!["  beta · /x · 1 msg(s) · x-3333-beta.json".into()],
        keys: vec!["beta".into()],
    });
    assert_eq!(a.session_keys, vec!["beta".to_string()]);
}

#[test]
fn frame_sessions_listing_renders_notice_style() {
    let mut a = app();
    a.fold(&AgentEvent::SessionsListed {
        lines: vec![
            "* (default) · /test · 4 msg(s) · test-1111.json".into(),
            "  alpha · /other · 2 msg(s) · other-2222-alpha.json".into(),
        ],
        keys: vec![],
    });
    let rows = render(&mut a, 80, 10);
    let body = rows.join("\n");
    assert!(body.contains("▌ 2 session(s):"), "count line:\n{body}");
    assert!(body.contains("▌   * (default) · /test"), "active line:\n{body}");
    assert!(body.contains("▌   alpha · /other"), "listing line:\n{body}");
}

#[test]
fn session_loaded_rebuilds_transcript_title_and_usage() {
    let mut a = app();
    // A live conversation AND a half-typed line are on screen.
    a.submit("live prompt");
    a.fold(&AgentEvent::TextDelta("live answer".into()));
    a.fold(&AgentEvent::TurnComplete {
        turn_usage: usage(12, 34),
        session_usage: usage(120, 340),
    });
    type_str(&mut a, "half-typed");

    a.fold(&AgentEvent::SessionLoaded {
        items: vec![
            ReplayItem::User("older prompt".into()),
            ReplayItem::Assistant("older answer".into()),
            ReplayItem::Tool { name: "bash".into() },
            ReplayItem::Tool { name: "read".into() },
            ReplayItem::User("second prompt".into()),
        ],
        notice: "resumed session: 5 messages, ~70 tokens in / 30 out".into(),
    });

    // SessionCleared semantics: old cells and usage gone, then the replay.
    assert_eq!(a.cells.len(), 6, "{:?}", a.cells);
    assert!(matches!(&a.cells[0], Cell::User(t) if t == "older prompt"));
    assert!(matches!(&a.cells[1], Cell::AssistantText(t) if t == "older answer"));
    match (&a.cells[2], &a.cells[3]) {
        (Cell::Tool(b), Cell::Tool(r)) => {
            assert!(b.replay && r.replay);
            assert_eq!(b.title.as_deref(), Some("bash"));
            assert!(!b.is_block(), "replay bash is a one-liner, never a box");
        }
        other => panic!("tool cells: {other:?}"),
    }
    assert!(matches!(&a.cells[5], Cell::Notice(n) if n.contains("resumed session")));
    // Title claimed by the FIRST replayed user prompt (fixes "new session").
    assert_eq!(a.title.as_deref(), Some("older prompt"));
    assert_eq!(a.session_usage, Usage::default(), "usage wiped like SessionCleared");
    assert!(!a.busy);
    assert_eq!(a.input, "half-typed", "input untouched by a resume");

    // Empty history: notice-only transcript, no panic, no title.
    a.fold(&AgentEvent::SessionLoaded {
        items: vec![],
        notice: "resumed session: 0 messages, ~— tokens in / — out".into(),
    });
    assert_eq!(a.cells.len(), 1);
    assert!(matches!(&a.cells[0], Cell::Notice(_)));
    assert_eq!(a.title, None);
}

#[test]
fn frame_resume_backscroll_tool_heavy_with_interrupt_shape() {
    let mut a = app();
    a.fold(&AgentEvent::SessionLoaded {
        items: vec![
            ReplayItem::User("go build it".into()),
            ReplayItem::Assistant("Starting with the *tests*.".into()),
            ReplayItem::Tool { name: "bash".into() },
            ReplayItem::Tool { name: "edit".into() },
            ReplayItem::Tool { name: "grep".into() },
            ReplayItem::User("and the interrupted one".into()),
            ReplayItem::Tool { name: "bash".into() },
        ],
        notice: "resumed session: 7 messages, ~70 tokens in / 30 out".into(),
    });
    a.fold(&AgentEvent::Notice(
        "session was recorded in /elsewhere; tools run in the current directory /test".into(),
    ));
    let rows = render(&mut a, 80, 20);
    let body = rows.join("\n");
    assert!(body.contains("▌ go build it"), "user block:\n{body}");
    assert!(body.contains("Starting with the"), "assistant text:\n{body}");
    assert!(body.contains("   ⚙ bash"), "replay tool one-liner:\n{body}");
    assert!(!body.contains("# bash"), "block tools never box on replay:\n{body}");
    assert!(!body.contains("bash: bash"), "no duplicated title:\n{body}");
    assert!(body.contains("▌ [!] resumed session: 7 messages"), "summary:\n{body}");
    assert!(body.contains("tools run in the current directory"), "advisory:\n{body}");
    // Header title = first replayed prompt.
    assert!(rows[0].contains("go build it"), "title: {}", rows[0]);
    // Consecutive replay tools group like live inline tools (no blank rows
    // between ⚙ lines).
    let gear_rows: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.contains('⚙'))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(gear_rows.len(), 4, "{body}");
    assert_eq!(gear_rows[1], gear_rows[0] + 1, "grouped: {body}");
    assert_eq!(gear_rows[2], gear_rows[1] + 1, "grouped: {body}");
}

/// The resume flow through the REAL runtime: SessionLoaded + advisory
/// arrive through the seam (as main emits them at startup, or commands.rs
/// on /resume), and the final frame carries the full backscroll — the T10
/// acceptance shape for the TUI.
#[test]
fn headless_resume_backscroll_renders_in_final_frame() {
    let mut script: Vec<Event> = Vec::new();
    script.extend("exit".chars().map(|c| Event::Key(key(KeyCode::Char(c)))));
    script.push(Event::Key(key(KeyCode::Enter)));

    let (mut ui, snapshot) = TuiUi::headless(
        SessionInfo {
            model: "claude-sonnet-5".into(),
            thinking: false,
            cwd: "/test".into(),
            version: "test".into(),
            profiles: vec![],
            provider: "anthropic".into(),
        },
        90,
        24,
        script,
        temur::provider::CancelToken::new(),
    );

    ui.event(&AgentEvent::SessionLoaded {
        items: vec![
            ReplayItem::User("resume me".into()),
            ReplayItem::Assistant("previous answer".into()),
            ReplayItem::Tool { name: "write".into() },
        ],
        notice: "resumed session: 3 messages, ~70 tokens in / 30 out".into(),
    });
    ui.event(&AgentEvent::Notice(
        "resumed session ended with a prompt the model never answered; it was dropped".into(),
    ));
    while let Some(line) = ui.read_input() {
        panic!("script only exits: {line}");
    }
    drop(ui);

    let rows = snapshot.lock().unwrap().clone();
    let body = rows.join("\n");
    assert!(rows[0].contains("resume me"), "header title: {}", rows[0]);
    assert!(body.contains("▌ resume me"), "user block:\n{body}");
    assert!(body.contains("   previous answer"), "assistant text:\n{body}");
    assert!(body.contains("   ⚙ write"), "tool one-liner:\n{body}");
    assert!(body.contains("▌ [!] resumed session: 3 messages"), "summary:\n{body}");
    assert!(body.contains("never answered"), "advisory follows the rebuild:\n{body}");
}

// ------------------------------------------------- T21: bash approval prompt

/// Force the sandbox probe to fail for this process (the one-way test
/// seam), so guarded sessions take the Ask arm. Safe here: no other test
/// in this binary consults `sandbox_available`, so the cached answer is
/// always set AFTER the env var.
fn force_probe_fail() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| std::env::set_var("TEMUR_TEST_SANDBOX_UNAVAILABLE", "1"));
    assert!(!temur::tools::sandbox_available());
}

/// T46: the T21 prompt these tests exercise, now a structured value.
///
/// Construction updated for the richer approval type; every asserted string
/// below is byte-identical. `no_key_sandbox` is the composed form, which is
/// what a guarded bash command produces and what the T21 wording and the
/// y/N answer set belong to.
fn t21_prompt(command: &str) -> temur::ui::tui::app::ApprovalPrompt {
    temur::ui::tui::app::ApprovalPrompt {
        tool: "bash".to_string(),
        summary: command.to_string(),
        no_key_sandbox: true,
        danger: None,
    }
}

#[test]
fn approval_prompt_restricts_keys_to_y_n_esc() {
    let mut a = app();
    a.busy = true;
    a.approval = Some(t21_prompt("echo hi"));
    // Anything that is not an answer is consumed and ignored: no typing,
    // no interrupt, no quit while the prompt is open.
    assert_eq!(a.handle_key(key(KeyCode::Char('x'))), Action::None);
    assert_eq!(a.handle_key(key(KeyCode::Enter)), Action::None);
    assert_eq!(a.handle_key(ctrl('c')), Action::None);
    assert!(a.input.is_empty(), "no key may leak into the input line");
    assert!(a.approval.is_some(), "prompt still open");
    // y approves and closes.
    assert_eq!(a.handle_key(key(KeyCode::Char('y'))), Action::Approval(true));
    assert!(a.approval.is_none());
    // n and Esc deny and close.
    a.approval = Some(t21_prompt("echo hi"));
    assert_eq!(a.handle_key(key(KeyCode::Char('n'))), Action::Approval(false));
    a.approval = Some(t21_prompt("echo hi"));
    assert_eq!(a.handle_key(key(KeyCode::Esc)), Action::Approval(false));
    // With the prompt closed, Esc while busy is an interrupt again.
    assert_eq!(a.handle_key(key(KeyCode::Esc)), Action::Interrupt);
}

#[test]
fn approval_prompt_renders_question_command_and_hint() {
    let mut a = app();
    a.busy = true;
    a.approval = Some(t21_prompt("cat /etc/hosts && echo done"));
    let rows = render(&mut a, 100, 14);
    let body = rows.join("\n");
    assert!(
        body.contains("run this bash command WITHOUT it? [y/N]"),
        "question line:\n{body}"
    );
    assert!(body.contains("cat /etc/hosts && echo done"), "exact command:\n{body}");
    assert!(
        body.contains("y approve this one command · n or esc deny"),
        "status hint:\n{body}"
    );
}

/// Build a guarded session over the approval fixture plus a second text
/// response, a gated step script, and the TUI approver; run one turn.
/// Returns (tempdir, final frame rows).
fn approval_turn(answer: KeyCode) -> (tempfile::TempDir, Vec<String>) {
    force_probe_fail();
    let dir = tempfile::tempdir().unwrap();
    let key_file = dir.path().join("api.key");
    std::fs::write(&key_file, "placeholder-not-a-real-key\n").unwrap();
    let provider = AnthropicProvider::new(
        "https://mock.invalid",
        "mock-key".into(),
        Box::new(ReplayTransport::new(vec![
            format!("{}/tests/fixtures/bash_approval.sse", env!("CARGO_MANIFEST_DIR")).into(),
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
        max_tokens_source: None,
        prose_tool_calls: true,
        cost_rates: None,
        cost_advisory_step_usd: temur::config::DEFAULT_COST_ADVISORY_STEP_USD,
        auto_compact: false,
    };
    let mut session = Session::new(Box::new(provider), Registry::standard(), cfg);
    session.set_key_guard(
        temur::tools::KeyGuard::from_paths(vec![key_file]),
        false,
    );

    let steps = vec![
        temur::ui::tui::ScriptStep::Line("do the approval task".into()),
        temur::ui::tui::ScriptStep::ApprovalKey(answer),
    ];
    let (mut ui, snapshot) = TuiUi::headless_steps(
        SessionInfo {
            model: "claude-sonnet-5".into(),
            thinking: false,
            cwd: dir.path().display().to_string(),
            version: "test".into(),
            profiles: vec![],
            provider: "anthropic".into(),
        },
        100,
        30,
        steps,
        session.cancel_token(),
    );
    session.set_approver(ui.approver());

    let line = ui.read_input().expect("scripted submit reaches read_input");
    assert_eq!(line, "do the approval task");
    session.turn(&line, &mut |ev| ui.event(&ev)).unwrap();
    drop(ui);
    let rows = snapshot.lock().unwrap().clone();
    (dir, rows)
}

#[test]
fn headless_approval_yes_runs_the_command_and_the_turn_completes() {
    let (dir, rows) = approval_turn(KeyCode::Char('y'));
    let body = rows.join("\n");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("approval-marker.txt")).unwrap(),
        "approval-ran\n",
        "the approved command must actually run:\n{body}"
    );
    assert!(body.contains("▌ # bash"), "bash block tool:\n{body}");
    assert!(!body.contains("# bash ✗"), "approved bash is not an error:\n{body}");
    assert!(body.contains("Hello, world!"), "second round text:\n{body}");
    assert!(
        !body.contains("[y/N]"),
        "the prompt must be gone from the final frame:\n{body}"
    );
}

#[test]
fn headless_approval_no_denies_and_the_session_continues() {
    let (dir, rows) = approval_turn(KeyCode::Char('n'));
    let body = rows.join("\n");
    assert!(
        !dir.path().join("approval-marker.txt").exists(),
        "a denied command must not run:\n{body}"
    );
    assert!(body.contains("# bash ✗"), "denied bash is an error result:\n{body}");
    assert!(
        body.contains("Hello, world!"),
        "the turn continues after a denial:\n{body}"
    );
}

/// T13 queued item: "/models listing renders two ids on one line in some
/// widths", observed once in the operator's terminal. T27 could not
/// reproduce it offline. This is the reproduction attempt kept as a pin,
/// not a fix: each id is one repeated letter, so a row carrying two
/// distinct letters proves content from two ids merged, fragments
/// included. The plain REPL cannot merge either, structurally: it prints
/// one line per id (src/ui/repl.rs).
#[test]
fn models_listing_never_puts_two_ids_on_one_row() {
    // Uppercase: none of these appear in the count line's own text.
    let letters = ["A", "B", "C", "D", "E", "F", "G"];
    // Realistic anthropic id lengths (22-28 columns).
    let ids: Vec<String> = letters
        .iter()
        .enumerate()
        .map(|(i, l)| l.repeat(22 + i))
        .collect();
    for w in 4u16..=200 {
        let mut a = app();
        a.fold(&AgentEvent::ModelsListed(ids.clone()));
        let rows = render(&mut a, w, 200);
        for (y, row) in rows.iter().enumerate() {
            let seen: std::collections::BTreeSet<char> = row
                .chars()
                .filter(|c| letters.contains(&c.to_string().as_str()))
                .collect();
            assert!(seen.len() < 2, "two ids merged at width={w}, row {y}: {row:?}");
        }
    }
}

// ---------------------------------------------------------------------------
// T43/P1: drain-and-draw-once
// ---------------------------------------------------------------------------

/// An `EventSource` that answers BOTH the render loop's blocking poll and its
/// zero-timeout drain polls, which is how a real terminal behaves under a
/// paste: the bytes are already in the input queue. It stamps every delivered
/// event with the iteration ("generation") that asked for it. The render loop
/// draws exactly once per iteration, so counting distinct generations counts
/// draws: a burst that lands in one generation is a burst that cost one frame.
struct BurstSource {
    queue: std::collections::VecDeque<Event>,
    generation: usize,
    stamps: std::sync::Arc<std::sync::Mutex<Vec<usize>>>,
}

impl BurstSource {
    fn new(events: Vec<Event>) -> (Self, std::sync::Arc<std::sync::Mutex<Vec<usize>>>) {
        let stamps = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        (
            BurstSource {
                queue: events.into(),
                generation: 0,
                stamps: std::sync::Arc::clone(&stamps),
            },
            stamps,
        )
    }
}

impl temur::ui::tui::EventSource for BurstSource {
    fn next(
        &mut self,
        timeout: std::time::Duration,
        _ready: temur::ui::tui::Readiness,
    ) -> std::io::Result<Option<Event>> {
        // A non-zero timeout is the once-per-iteration blocking poll, which
        // happens right after that iteration's draw.
        if !timeout.is_zero() {
            self.generation += 1;
        }
        match self.queue.pop_front() {
            Some(ev) => {
                self.stamps.lock().unwrap().push(self.generation);
                Ok(Some(ev))
            }
            None => Ok(None),
        }
    }
}

fn burst_info(cwd: String) -> SessionInfo {
    SessionInfo {
        model: "claude-sonnet-5".into(),
        thinking: false,
        cwd,
        version: "test".into(),
        profiles: vec![],
        provider: "anthropic".into(),
    }
}

#[test]
fn burst_of_keys_lands_in_one_iteration_and_one_draw() {
    let text = "x".repeat(200);
    let mut script: Vec<Event> = text
        .chars()
        .map(|c| Event::Key(key(KeyCode::Char(c))))
        .collect();
    script.push(Event::Key(key(KeyCode::Enter)));
    let total = script.len();
    let (source, stamps) = BurstSource::new(script);

    let cancel = temur::provider::CancelToken::new();
    let (mut ui, _snapshot) =
        TuiUi::headless_with_source(burst_info("/tmp".into()), 100, 30, source, cancel);
    let line = ui.read_input().expect("the burst submits");
    drop(ui);

    assert_eq!(line, text, "every pasted char reached the input");
    let stamps = stamps.lock().unwrap().clone();
    assert_eq!(stamps.len(), total, "every event was delivered");
    assert!(
        stamps.iter().all(|g| *g == stamps[0]),
        "the whole burst landed in ONE iteration (one draw); generations seen: {:?}",
        stamps.iter().collect::<std::collections::BTreeSet<_>>()
    );
}

#[test]
fn oversized_burst_is_capped_per_iteration_and_nothing_is_lost() {
    // One event past the cap has to wait for the next iteration, and the
    // remainder must still arrive: the cap defers, it never drops.
    const CAP: usize = 4096;
    let text = "x".repeat(CAP + 904);
    let mut script: Vec<Event> = text
        .chars()
        .map(|c| Event::Key(key(KeyCode::Char(c))))
        .collect();
    script.push(Event::Key(key(KeyCode::Enter)));
    let total = script.len();
    let (source, stamps) = BurstSource::new(script);

    let cancel = temur::provider::CancelToken::new();
    let (mut ui, _snapshot) =
        TuiUi::headless_with_source(burst_info("/tmp".into()), 100, 30, source, cancel);
    let line = ui.read_input().expect("the oversized burst still submits");
    drop(ui);

    assert_eq!(line.len(), text.len(), "no pasted char was dropped");
    let stamps = stamps.lock().unwrap().clone();
    assert_eq!(stamps.len(), total, "every event was delivered");
    let first = stamps[0];
    let in_first = stamps.iter().filter(|g| **g == first).count();
    assert_eq!(in_first, CAP, "the first iteration took exactly the cap");
    assert!(
        stamps[CAP] > first,
        "the event past the cap waited for a later iteration"
    );
}

// ---------------------------------------------------------------------------
// T43/P2: interrupt priority
// ---------------------------------------------------------------------------

use temur::ui::tui::interrupt_priority;

/// Hundreds of pasted chars with an interrupt key queued behind them: the
/// exact shape the operator reported, where Esc landed but the queued text
/// then started a fresh turn.
fn burst_then(tail: KeyEvent) -> Vec<Event> {
    let mut batch: Vec<Event> = "a very long pasted line of text"
        .repeat(20)
        .chars()
        .map(|c| Event::Key(key(KeyCode::Char(c))))
        .collect();
    batch.push(Event::Key(tail));
    batch
}

#[test]
fn busy_ctrl_c_behind_a_burst_wins_and_the_rest_is_discarded() {
    let batch = burst_then(ctrl('c'));
    let out = interrupt_priority(batch, true, false);
    assert_eq!(out.len(), 1, "exactly one event survives");
    assert_eq!(out[0], Event::Key(ctrl('c')), "and it is the interrupt");
}

#[test]
fn busy_esc_behind_a_burst_wins_and_the_input_is_not_polluted() {
    let batch = burst_then(key(KeyCode::Esc));
    let out = interrupt_priority(batch, true, false);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0], Event::Key(key(KeyCode::Esc)));
    assert!(
        !out.iter().any(|ev| matches!(ev, Event::Key(k) if matches!(k.code, KeyCode::Char(_)))),
        "not one pasted char survives to pollute the input line"
    );
}

#[test]
fn only_the_first_interrupt_in_a_batch_is_honored() {
    let mut batch = burst_then(key(KeyCode::Esc));
    batch.push(Event::Key(ctrl('c')));
    batch.push(Event::Key(key(KeyCode::Esc)));
    let out = interrupt_priority(batch, true, false);
    assert_eq!(out.len(), 1, "three interrupts collapse to one");
    assert_eq!(out[0], Event::Key(key(KeyCode::Esc)), "the FIRST one");
}

#[test]
fn idle_batches_are_untouched_and_process_in_order() {
    let batch = burst_then(key(KeyCode::Enter));
    let before = batch.clone();
    let out = interrupt_priority(batch, false, false);
    assert_eq!(out, before, "an idle batch is returned verbatim");
}

#[test]
fn a_busy_batch_without_an_interrupt_is_untouched() {
    let batch = burst_then(key(KeyCode::Enter));
    let before = batch.clone();
    let out = interrupt_priority(batch, true, false);
    assert_eq!(out, before, "nothing to prioritize, nothing discarded");
}

#[test]
fn an_open_approval_prompt_is_excluded_from_interrupt_priority() {
    // Esc there means "deny this command", and a y/N answer is a modal
    // answer rather than input, so the batch must not be rewritten.
    let batch = vec![
        Event::Key(key(KeyCode::Char('y'))),
        Event::Key(key(KeyCode::Esc)),
    ];
    let before = batch.clone();
    let out = interrupt_priority(batch, true, true);
    assert_eq!(out, before);
}

#[test]
fn key_releases_never_count_as_interrupts() {
    use ratatui::crossterm::event::KeyEventKind;
    let mut released = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    released.kind = KeyEventKind::Release;
    let batch = vec![Event::Key(key(KeyCode::Char('a'))), Event::Key(released)];
    let before = batch.clone();
    let out = interrupt_priority(batch, true, false);
    assert_eq!(out, before, "a release is not a press");
}

// ---------------------------------------------------------------------------
// T43/P3: the force-quit window
// ---------------------------------------------------------------------------

#[test]
fn double_ctrl_c_force_quits_with_a_whole_paste_in_between() {
    // The operator's actual sequence: Ctrl+C, hundreds of pasted characters
    // still draining, Ctrl+C. Under the old any-key disarm this could never
    // fire; inside the window it now does.
    let mut a = app();
    a.submit("go");
    assert_eq!(a.handle_key(ctrl('c')), Action::None, "first press arms");
    type_str(&mut a, &"pasted text ".repeat(30));
    a.now_ms = temur::ui::tui::app::FORCE_QUIT_WINDOW_MS - 1;
    assert!(a.force_quit_armed(), "still armed inside the window");
    assert_eq!(a.handle_key(ctrl('c')), Action::ForceQuit);
}

#[test]
fn a_stale_first_ctrl_c_past_the_window_does_not_quit() {
    let mut a = app();
    a.submit("go");
    assert_eq!(a.handle_key(ctrl('c')), Action::None);
    a.now_ms = temur::ui::tui::app::FORCE_QUIT_WINDOW_MS + 1;
    assert!(!a.force_quit_armed(), "the window expired");
    assert_eq!(
        a.handle_key(ctrl('c')),
        Action::None,
        "a stale press re-arms rather than quitting"
    );
    assert!(a.force_quit_armed(), "and the re-arm is live");
}

#[test]
fn the_window_is_measured_from_the_press_not_from_the_last_key() {
    // Keys arriving through the window must not extend it.
    let mut a = app();
    a.submit("go");
    assert_eq!(a.handle_key(ctrl('c')), Action::None); // armed at now_ms = 0
    for t in [500u64, 1000, 1500, 2000] {
        a.now_ms = t;
        a.handle_key(key(KeyCode::Char('x')));
        assert!(a.force_quit_armed(), "still armed at {t}ms");
    }
    a.now_ms = temur::ui::tui::app::FORCE_QUIT_WINDOW_MS + 1;
    assert!(!a.force_quit_armed(), "typing did not extend the window");
}

#[test]
fn submit_and_turn_end_still_clear_the_latch() {
    let mut a = app();
    a.submit("go");
    assert_eq!(a.handle_key(ctrl('c')), Action::None);
    assert!(a.force_quit_armed());
    a.prompt_open();
    assert!(!a.force_quit_armed(), "turn end clears the latch");

    a.submit("again");
    assert_eq!(a.handle_key(ctrl('c')), Action::None);
    assert!(a.force_quit_armed());
    a.submit("and again");
    assert!(!a.force_quit_armed(), "submit clears the latch");
}

#[test]
fn the_force_quit_hint_follows_the_window() {
    let mut a = app();
    a.submit("go");
    a.handle_key(ctrl('c'));
    let armed = render(&mut a, 80, 12).join("\n");
    assert!(armed.contains("force-quit"), "hint while armed:\n{armed}");

    a.now_ms = temur::ui::tui::app::FORCE_QUIT_WINDOW_MS + 1;
    let expired = render(&mut a, 80, 12).join("\n");
    assert!(
        !expired.contains("force-quit"),
        "hint is gone once the window expires:\n{expired}"
    );
}

// ---------------------------------------------------------------------------
// T43/P4: bracketed paste
// ---------------------------------------------------------------------------

#[test]
fn paste_inserts_at_the_cursor_and_keeps_its_newlines() {
    let mut a = app();
    type_str(&mut a, "ab");
    a.handle_key(key(KeyCode::Left)); // cursor between a and b
    a.paste("X\nY");
    assert_eq!(a.input, "aX\nYb", "inserted at the cursor, newline kept");
    assert_eq!(a.cursor, "aX\nY".len(), "cursor sits after the paste");
}

#[test]
fn paste_never_submits_whatever_it_contains() {
    let mut a = app();
    a.paste("first line\nsecond line\n");
    assert!(!a.busy, "no turn started");
    assert!(a.cells.is_empty(), "no user cell recorded");
    assert_eq!(a.input, "first line\nsecond line\n", "it is just input");
}

#[test]
fn paste_normalizes_crlf_and_lone_cr() {
    let mut a = app();
    a.paste("one\r\ntwo\rthree");
    assert_eq!(a.input, "one\ntwo\nthree");
}

#[test]
fn paste_strips_control_characters_but_keeps_newline_and_tab() {
    let mut a = app();
    a.paste("a\u{1b}[31mb\u{0}c\td\ne");
    assert_eq!(a.input, "a[31mbc\td\ne", "escape and NUL gone, tab and LF kept");
}

#[test]
fn an_all_control_paste_is_a_no_op() {
    let mut a = app();
    type_str(&mut a, "keep");
    a.paste("\u{0}\u{1b}\u{7}");
    assert_eq!(a.input, "keep");
    assert_eq!(a.cursor, 4);
}

#[test]
fn enter_submits_a_multi_line_paste_as_one_prompt() {
    let mut a = app();
    a.paste("line one\nline two\n");
    let action = a.handle_key(key(KeyCode::Enter));
    assert_eq!(
        action,
        Action::Submit("line one\nline two".into()),
        "one prompt, trailing newline trimmed, interior newline kept"
    );
}

#[test]
fn submit_end_trims_only_so_pasted_indentation_survives() {
    let mut a = app();
    a.paste("    def f():\n        return 1\n\n");
    assert_eq!(
        a.handle_key(key(KeyCode::Enter)),
        Action::Submit("    def f():\n        return 1".into()),
        "leading indentation is part of the prompt"
    );
}

#[test]
fn a_whitespace_only_paste_still_refuses_to_submit() {
    let mut a = app();
    a.paste("   \n\t\n");
    assert_eq!(a.handle_key(key(KeyCode::Enter)), Action::None);
}

#[test]
fn single_line_slash_is_a_command_but_a_multi_line_paste_is_not() {
    assert!(App::is_command_line("/model sonnet-next"));
    assert!(!App::is_command_line("/model sonnet-next\nand more"));
    assert!(!App::is_command_line("not a command"));
    assert!(
        !App::is_command_line("/usr/bin/env\nls"),
        "a pasted path block opening with / is a prompt"
    );
}

#[test]
fn a_multi_line_paste_opening_with_slash_does_not_style_as_a_command() {
    let mut a = app();
    a.paste("/etc/hosts\nsecond line");
    let rows = render(&mut a, 80, 12).join("\n");
    assert!(
        !rows.contains("unknown command") && !rows.contains("/etc/hosts ·"),
        "no command hint for a multi-line paste:\n{rows}"
    );
}

#[test]
fn the_input_line_shows_a_return_glyph_for_each_pasted_newline() {
    let mut a = app();
    a.paste("alpha\nbeta");
    let rows = render(&mut a, 80, 12);
    let input_row = rows
        .iter()
        .find(|r| r.contains("alpha"))
        .expect("the input row is drawn");
    assert!(
        input_row.contains('\u{21b5}'),
        "the newline is visible as a glyph: {input_row:?}"
    );
    assert!(
        input_row.contains("alpha") && input_row.contains("beta"),
        "both sides of the break are drawn: {input_row:?}"
    );
}

#[test]
fn a_multi_line_user_cell_renders_one_transcript_line_per_paste_line() {
    let mut a = app();
    a.submit("line one\nline two");
    let rows = render(&mut a, 80, 12);
    // The header claims the title too, flattened to one line; only the
    // transcript rows (the ones carrying the user bar) are under test here.
    let bars: Vec<&String> = rows
        .iter()
        .filter(|r| r.contains('\u{258c}') && r.contains("line "))
        .collect();
    assert_eq!(bars.len(), 2, "two transcript lines, not one run-on: {bars:?}");
    assert!(bars[0].contains("line one") && !bars[0].contains("line two"));
    assert!(bars[1].contains("line two"));
}

#[test]
fn history_recall_round_trips_a_multi_line_prompt() {
    let mut a = app();
    a.submit("first\nsecond");
    a.prompt_open();
    a.handle_key(key(KeyCode::Up));
    assert_eq!(a.input, "first\nsecond", "recalled with its newline intact");
    assert_eq!(a.cursor, a.input.len(), "cursor at the end");
    assert_eq!(
        a.handle_key(key(KeyCode::Enter)),
        Action::Submit("first\nsecond".into()),
        "and it resubmits as one prompt"
    );
}

#[test]
fn headless_a_pasted_block_reaches_read_input_as_one_prompt() {
    let script = vec![
        Event::Paste("do the smoke task\nwith a second line".into()),
        Event::Key(key(KeyCode::Enter)),
    ];
    let cancel = temur::provider::CancelToken::new();
    let (mut ui, snapshot) =
        TuiUi::headless(burst_info("/tmp".into()), 100, 30, script, cancel);
    let line = ui.read_input().expect("the paste submits on Enter");
    drop(ui);
    assert_eq!(line, "do the smoke task\nwith a second line");
    let rows = snapshot.lock().unwrap().clone().join("\n");
    assert!(rows.contains("do the smoke task"), "user cell line 1:\n{rows}");
    assert!(rows.contains("with a second line"), "user cell line 2:\n{rows}");
}

#[test]
fn headless_a_paste_alone_starts_no_turn() {
    // A character typed AFTER the paste is the proof: if the paste had
    // submitted on its own, read_input would yield the pasted text without
    // the trailing "!", and it would yield it before this Enter. (Asserting
    // on a final frame with no submit in the script would race the shutdown
    // that captures it, so the assertion is on what reaches the agent.)
    let script = vec![
        Event::Paste("nothing should submit this".into()),
        Event::Key(key(KeyCode::Char('!'))),
        Event::Key(key(KeyCode::Enter)),
    ];
    let cancel = temur::provider::CancelToken::new();
    let (mut ui, _snapshot) =
        TuiUi::headless(burst_info("/tmp".into()), 100, 30, script, cancel);
    let line = ui.read_input().expect("the Enter submits");
    drop(ui);
    assert_eq!(
        line, "nothing should submit this!",
        "the paste waited for the Enter instead of submitting itself"
    );
}

// ---------------------------------------------------------------------------
// T43/P5: the Esc regression pin
// ---------------------------------------------------------------------------

/// The operator's exact shape. The first line is delivered while the app is
/// idle, so it starts a turn. Everything after it, the Esc and the text
/// queued behind it, is held back until the app is BUSY and then delivered as
/// one drained batch, which is what a real terminal does when someone is
/// still pasting into a running turn and reaches for Esc.
struct BusyBurst {
    idle_wave: std::collections::VecDeque<Event>,
    busy_wave: std::collections::VecDeque<Event>,
}

impl temur::ui::tui::EventSource for BusyBurst {
    fn next(
        &mut self,
        _timeout: std::time::Duration,
        ready: temur::ui::tui::Readiness,
    ) -> std::io::Result<Option<Event>> {
        if !self.idle_wave.is_empty() {
            return Ok(self.idle_wave.pop_front());
        }
        if !ready.idle {
            return Ok(self.busy_wave.pop_front());
        }
        Ok(None)
    }
}

#[test]
fn esc_mid_turn_interrupts_once_and_the_queued_input_starts_no_new_turn() {
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
        max_tokens_source: None,
        prose_tool_calls: true,
        cost_rates: None,
        cost_advisory_step_usd: temur::config::DEFAULT_COST_ADVISORY_STEP_USD,
        auto_compact: false,
    };
    let mut session = Session::new(Box::new(BlockUntilCancelled), Registry::standard(), cfg);

    let mut idle_wave: std::collections::VecDeque<Event> = "interrupt me"
        .chars()
        .map(|c| Event::Key(key(KeyCode::Char(c))))
        .collect();
    idle_wave.push_back(Event::Key(key(KeyCode::Enter)));

    // Esc first, then the rest of the paste still draining behind it,
    // ending in the Enter that used to start the unstoppable next turn.
    let mut busy_wave: std::collections::VecDeque<Event> =
        std::collections::VecDeque::from(vec![Event::Key(key(KeyCode::Esc))]);
    busy_wave.extend(
        "queued chunk two"
            .chars()
            .map(|c| Event::Key(key(KeyCode::Char(c)))),
    );
    busy_wave.push_back(Event::Key(key(KeyCode::Enter)));

    let source = BusyBurst {
        idle_wave,
        busy_wave,
    };
    let (mut ui, snapshot) = TuiUi::headless_with_source(
        SessionInfo {
            model: "claude-sonnet-5".into(),
            thinking: false,
            cwd: dir.path().display().to_string(),
            version: "test".into(),
            profiles: vec![],
            provider: "anthropic".into(),
        },
        100,
        30,
        source,
        session.cancel_token(),
    );

    let line = ui.read_input().expect("the first line starts a turn");
    assert_eq!(line, "interrupt me");
    session.turn(&line, &mut |ev| ui.event(&ev)).unwrap();
    drop(ui);

    let body = snapshot.lock().unwrap().clone().join("\n");
    assert!(body.contains("turn interrupted"), "Esc landed:\n{body}");
    assert!(body.contains("interrupt me"), "the one prompt is there:\n{body}");
    assert!(
        !body.contains("queued chunk two"),
        "the input queued behind the Esc was discarded, not submitted and \
         not left in the input line:\n{body}"
    );
    assert_eq!(
        body.matches('\u{25a3}').count(),
        1,
        "exactly one turn ran, not a second one started by the queued Enter:\n{body}"
    );
}
