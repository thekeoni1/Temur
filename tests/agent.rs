//! M4 agent-loop tests against a scripted MockProvider. Real tools run in a
//! temp dir; the provider is fully scripted — no network.

use temur::agent::events::AgentEvent;
use temur::agent::{CompactOutcome, Session, SessionConfig, INTERRUPT_MARKER};
use temur::provider::*;
use temur::tools::Registry;
use std::cell::RefCell;
use std::rc::Rc;

struct MockProvider {
    responses: RefCell<Vec<ResponseMessage>>,
    requests: Rc<RefCell<Vec<ChatRequest>>>,
}

impl Provider for MockProvider {
    fn stream(
        &self,
        req: &ChatRequest,
        _on_event: &mut dyn FnMut(StreamEvent),
        _cancel: &CancelToken,
    ) -> Result<ResponseMessage, ProviderError> {
        self.requests.borrow_mut().push(req.clone());
        Ok(self.responses.borrow_mut().remove(0))
    }
}

fn msg(content: Vec<ContentBlock>, stop: StopReason) -> ResponseMessage {
    // Build via JSON to exercise the public Deserialize path (fields are
    // read-only from the crate's perspective).
    let value = serde_json::json!({
        "id": "msg_test",
        "model": "claude-sonnet-5",
        "role": "assistant",
        "content": [],
        "usage": {"input_tokens": 10, "output_tokens": 5}
    });
    let mut m: ResponseMessage = serde_json::from_value(value).unwrap();
    m.content = content;
    m.stop_reason = Some(stop);
    m
}

fn text(t: &str) -> ContentBlock {
    ContentBlock::Text { text: t.into() }
}

fn tool_use(id: &str, name: &str, input: serde_json::Value) -> ContentBlock {
    ContentBlock::ToolUse {
        id: id.into(),
        name: name.into(),
        input,
        input_raw: None,
        provider_state: None,
    }
}

fn session_with(
    dir: &std::path::Path,
    responses: Vec<ResponseMessage>,
) -> (Session, Rc<RefCell<Vec<ChatRequest>>>) {
    let requests = Rc::new(RefCell::new(vec![]));
    let provider = MockProvider {
        responses: RefCell::new(responses),
        requests: requests.clone(),
    };
    let cfg = SessionConfig {
        model: "claude-sonnet-5".into(),
        max_tokens: 32_000,
        system: Some("test system".into()),
        thinking: false,
        cwd: dir.to_path_buf(),
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
    (
        Session::new(Box::new(provider), Registry::standard(), cfg),
        requests,
    )
}

fn collect_events(session: &mut Session, input: &str) -> Vec<AgentEvent> {
    let mut events = vec![];
    session.turn(input, &mut |e| events.push(e)).unwrap();
    events
}

#[test]
fn simple_text_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![msg(vec![text("Hi there")], StopReason::EndTurn)],
    );
    let events = collect_events(&mut session, "hello");

    assert_eq!(requests.borrow().len(), 1);
    assert_eq!(requests.borrow()[0].tools.len(), 8); // full registry advertised
    assert_eq!(session.history().len(), 2); // user + assistant
    match &events[..] {
        [AgentEvent::TurnComplete {
            turn_usage,
            session_usage,
        }] => {
            assert_eq!(turn_usage.input_tokens, Some(10));
            assert_eq!(session_usage.output_tokens, Some(5));
        }
        other => panic!("unexpected events: {other:?}"),
    }
}

#[test]
fn tool_round_trip_all_results_in_one_user_message() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("note.txt");
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(
                vec![
                    text("Writing then reading."),
                    tool_use(
                        "tu_1",
                        "write",
                        serde_json::json!({"filePath": file.to_str().unwrap(), "content": "payload"}),
                    ),
                    tool_use(
                        "tu_2",
                        "read",
                        serde_json::json!({"filePath": file.to_str().unwrap()}),
                    ),
                ],
                StopReason::ToolUse,
            ),
            msg(vec![text("Done.")], StopReason::EndTurn),
        ],
    );
    let events = collect_events(&mut session, "write then read");

    // Two provider calls; second carries the tool results.
    assert_eq!(requests.borrow().len(), 2);
    let second = &requests.borrow()[1];
    let last = second.messages.last().unwrap();
    assert!(matches!(last.role, Role::User));
    assert_eq!(last.content.len(), 2, "ALL results in ONE user message");
    match (&last.content[0], &last.content[1]) {
        (
            ContentBlock::ToolResult {
                tool_use_id: id1,
                is_error: e1,
                ..
            },
            ContentBlock::ToolResult {
                tool_use_id: id2,
                content: c2,
                is_error: e2,
            },
        ) => {
            assert_eq!(id1, "tu_1");
            assert_eq!(id2, "tu_2");
            assert!(!e1 && !e2);
            assert!(c2.contains("payload"), "read saw write's output");
        }
        other => panic!("expected two tool_results, got {other:?}"),
    }
    // The write actually happened on disk.
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "payload");
    // ToolEnd events for both, then TurnComplete last.
    assert!(events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolEnd { .. }))
        .count() == 2);
    assert!(matches!(events.last(), Some(AgentEvent::TurnComplete { .. })));
}

#[test]
fn tool_failure_becomes_is_error_result_not_crash() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(
                vec![tool_use("tu_1", "read", serde_json::json!({"wrongParam": true}))],
                StopReason::ToolUse,
            ),
            msg(vec![text("I'll fix my input.")], StopReason::EndTurn),
        ],
    );
    let events = collect_events(&mut session, "go");

    let second = &requests.borrow()[1];
    match &second.messages.last().unwrap().content[0] {
        ContentBlock::ToolResult {
            is_error, content, ..
        } => {
            assert!(is_error);
            assert!(content.contains("invalid arguments"));
        }
        other => panic!("expected tool_result, got {other:?}"),
    }
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolEnd { is_error: true, .. })));
}

#[test]
fn pause_turn_resends_without_user_message() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(vec![text("part one...")], StopReason::PauseTurn),
            msg(vec![text("...part two")], StopReason::EndTurn),
        ],
    );
    collect_events(&mut session, "long task");

    assert_eq!(requests.borrow().len(), 2);
    let second = &requests.borrow()[1];
    // Last message before resume is the ASSISTANT partial, no injected user msg.
    assert!(matches!(second.messages.last().unwrap().role, Role::Assistant));
    assert_eq!(session.history().len(), 3); // user + 2 assistant parts
}

#[test]
fn refusal_discards_output_and_notifies() {
    let dir = tempfile::tempdir().unwrap();
    let mut refusal = msg(vec![text("partial to discard")], StopReason::Refusal);
    refusal.stop_details = Some(StopDetails {
        kind: "refusal".into(),
        category: Some("cyber".into()),
        explanation: Some("policy".into()),
    });
    let (mut session, _requests) = session_with(dir.path(), vec![refusal]);
    let events = collect_events(&mut session, "do the thing");

    // Refused output is NOT in history (only the user message remains).
    assert_eq!(session.history().len(), 1);
    let notice = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Notice(n) => Some(n.clone()),
            _ => None,
        })
        .expect("refusal notice");
    assert!(notice.contains("refused"));
    assert!(notice.contains("cyber"));
}

/// A provider that replays scripted stream EVENTS before returning its
/// scripted response. MockProvider ignores the event callback, so this is
/// what the cell-pairing seam (ToolStart from the stream, ToolEnd from the
/// loop) has to be tested through.
struct StreamingProvider {
    events: RefCell<Vec<StreamEvent>>,
    responses: RefCell<Vec<ResponseMessage>>,
}

impl Provider for StreamingProvider {
    fn stream(
        &self,
        _req: &ChatRequest,
        on_event: &mut dyn FnMut(StreamEvent),
        _cancel: &CancelToken,
    ) -> Result<ResponseMessage, ProviderError> {
        for ev in self.events.borrow_mut().drain(..) {
            on_event(ev);
        }
        Ok(self.responses.borrow_mut().remove(0))
    }
}

/// T13: a refusal that lands after the stream already announced a tool call
/// must close that cell, or the TUI shows a spinner forever. The call itself
/// never runs and nothing is synthesized into history: the refused output is
/// discarded whole, which is what separates this from the interrupt path.
#[test]
fn refusal_closes_the_tool_cells_it_opened() {
    let dir = tempfile::tempdir().unwrap();
    let side_effect = dir.path().join("never-written.txt");
    let (mut session, _) = session_with(dir.path(), vec![]);
    let refusal = msg(
        vec![
            text("sure, writing that"),
            tool_use(
                "tu_1",
                "write",
                serde_json::json!({
                    "filePath": side_effect.to_str().unwrap(),
                    "content": "boom"
                }),
            ),
        ],
        StopReason::Refusal,
    );
    session.switch_provider(
        Box::new(StreamingProvider {
            events: RefCell::new(vec![StreamEvent::ToolUseStarted {
                name: "write".into(),
            }]),
            responses: RefCell::new(vec![refusal]),
        }),
        &selection("claude-sonnet-5", 32_000, None),
        None,
    );
    let events = collect_events(&mut session, "do the thing");

    let start = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolStart { name } if name == "write"))
        .expect("the stream opened a write cell");
    let end = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolEnd { name, is_error: true, .. } if name == "write"))
        .expect("the refusal closed it");
    let notice = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Notice(n) if n.contains("refused")))
        .expect("refusal notice");
    assert!(start < end && end < notice, "cell closes before the notice: {events:?}");
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolEnd { .. }))
            .count(),
        1,
        "exactly one close for the one open cell"
    );
    assert!(!side_effect.exists(), "the refused call must never execute");
    assert_eq!(session.history().len(), 1, "only the user message remains");
}

/// The same path with a nameless tool_use block: no cell was ever opened
/// (ToolStart needs a name), so closing one would break the FIFO pairing.
#[test]
fn refusal_does_not_close_a_cell_an_unnamed_block_never_opened() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let refusal = msg(
        vec![tool_use("tu_1", "", serde_json::json!({}))],
        StopReason::Refusal,
    );
    session.switch_provider(
        Box::new(StreamingProvider {
            events: RefCell::new(vec![]),
            responses: RefCell::new(vec![refusal]),
        }),
        &selection("claude-sonnet-5", 32_000, None),
        None,
    );
    let events = collect_events(&mut session, "do the thing");
    assert!(
        !events.iter().any(|e| matches!(e, AgentEvent::ToolEnd { .. })),
        "no cell was opened, so none may be closed: {events:?}"
    );
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::Notice(n) if n.contains("refused"))));
}

#[test]
fn doom_loop_guard_stops_identical_calls() {
    let dir = tempfile::tempdir().unwrap();
    let repeat = || {
        msg(
            vec![tool_use("tu_x", "bash", serde_json::json!({"command": "true"}))],
            StopReason::ToolUse,
        )
    };
    let (mut session, requests) = session_with(
        dir.path(),
        vec![repeat(), repeat(), repeat(), repeat(), repeat()],
    );
    let events = collect_events(&mut session, "loop forever");

    assert_eq!(requests.borrow().len(), 3, "stopped at the third identical call");
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Notice(n) if n.contains("repeated 3 times")
    )));
}

#[test]
fn alternating_pair_doom_loop_fires() {
    // T4 DELIBERATE INVERSION: this spot previously asserted that
    // alternating two calls escaped the doom-loop guard (only the iteration
    // limit could stop such a turn). The 6-deep alternating-pair window now
    // catches A,B,A,B,A,B — the guard fires on the 6th request, before
    // executing its batch.
    let dir = tempfile::tempdir().unwrap();
    let call = |cmd: &str| {
        msg(
            vec![tool_use("tu_x", "bash", serde_json::json!({"command": cmd}))],
            StopReason::ToolUse,
        )
    };
    let responses = vec![
        call("echo a"),
        call("echo b"),
        call("echo a"),
        call("echo b"),
        call("echo a"),
        call("echo b"),
    ];
    let (mut session, requests) = session_with(dir.path(), responses);
    let events = collect_events(&mut session, "loop");
    assert_eq!(requests.borrow().len(), 6, "guard fires on the 6th request");
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Notice(n) if n.contains("alternated")
    )));
}

#[test]
fn iteration_limit_stops_runaway_turns() {
    let dir = tempfile::tempdir().unwrap();
    // Ten DISTINCT calls so neither doom-loop guard (identical or
    // alternating) fires; only the iteration limit stops the turn.
    let mk = |i: u32| {
        msg(
            vec![tool_use(
                "tu_i",
                "bash",
                serde_json::json!({"command": format!("echo {i}")}),
            )],
            StopReason::ToolUse,
        )
    };
    let responses: Vec<_> = (0..10).map(mk).collect();
    let requests = Rc::new(RefCell::new(vec![]));
    let provider = MockProvider {
        responses: RefCell::new(responses),
        requests: requests.clone(),
    };
    let cfg = SessionConfig {
        model: "m".into(),
        max_tokens: 1000,
        system: None,
        thinking: false,
        cwd: dir.path().to_path_buf(),
        max_iterations: 4,
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
    let mut events = vec![];
    session.turn("go", &mut |e| events.push(e)).unwrap();

    assert_eq!(requests.borrow().len(), 4);
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Notice(n) if n.contains("iteration limit")
    )));
}

#[test]
fn iteration_limit_flows_from_config_to_session() {
    let dir = tempfile::tempdir().unwrap();
    // Custom value from config.json is respected (deliberately != the 400
    // default so a silent fall-back to the default would fail here)...
    let cfg: temur::config::Config =
        serde_json::from_str(r#"{"max_turn_iterations":7}"#).unwrap();
    let scfg = SessionConfig::from_config(&cfg, dir.path().to_path_buf());
    assert_eq!(scfg.max_iterations, 7);
    // ...and the built-in default applies when the field is absent.
    let cfg: temur::config::Config = serde_json::from_str("{}").unwrap();
    let scfg = SessionConfig::from_config(&cfg, dir.path().to_path_buf());
    assert_eq!(scfg.max_iterations, temur::config::DEFAULT_MAX_TURN_ITERATIONS);
}

// --- T3 context-window awareness (advisory only) ---------------------------

fn msg_with_usage(
    content: Vec<ContentBlock>,
    stop: StopReason,
    usage: serde_json::Value,
) -> ResponseMessage {
    let value = serde_json::json!({
        "id": "msg_test",
        "model": "local-test",
        "role": "assistant",
        "content": [],
        "usage": usage
    });
    let mut m: ResponseMessage = serde_json::from_value(value).unwrap();
    m.content = content;
    m.stop_reason = Some(stop);
    m
}

fn session_with_window(
    dir: &std::path::Path,
    responses: Vec<ResponseMessage>,
    context_window: Option<u64>,
    max_tokens: u32,
) -> Session {
    let provider = MockProvider {
        responses: RefCell::new(responses),
        requests: Rc::new(RefCell::new(vec![])),
    };
    let cfg = SessionConfig {
        model: "local-test".into(),
        max_tokens,
        system: None,
        thinking: false,
        cwd: dir.to_path_buf(),
        max_iterations: 50,
        temperature: None,
        top_p: None,
        context_window,
        max_tokens_source: None,
        prose_tool_calls: true,
        cost_rates: None,
        cost_advisory_step_usd: temur::config::DEFAULT_COST_ADVISORY_STEP_USD,
        auto_compact: false,
    };
    Session::new(Box::new(provider), Registry::standard(), cfg)
}

fn notices(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Notice(n) => Some(n.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn context_prewarn_fires_once_per_session() {
    let dir = tempfile::tempdir().unwrap();
    // window 1000, max_tokens 800: usage 150+100=250 leaves 750 < 800.
    let mut session = session_with_window(
        dir.path(),
        vec![
            msg_with_usage(
                vec![text("a")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 150, "output_tokens": 100}),
            ),
            msg_with_usage(
                vec![text("b")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 300, "output_tokens": 100}),
            ),
        ],
        Some(1000),
        800,
    );
    let n1 = notices(&collect_events(&mut session, "one"));
    assert_eq!(n1.len(), 1, "exactly one pre-warn: {n1:?}");
    // T20 unified wording: both remedies, /compact first.
    assert!(n1[0].contains("context: ~250 of 1000 tokens used"));
    assert!(n1[0].contains("/compact frees the window"));
    assert!(n1[0].contains("or start a new session"));
    // Turn two re-satisfies the condition; the warning is once per SESSION.
    let n2 = notices(&collect_events(&mut session, "two"));
    assert!(n2.is_empty(), "no repeat warning: {n2:?}");
}

#[test]
fn eighty_percent_arm_fires_independently_of_max_tokens() {
    let dir = tempfile::tempdir().unwrap();
    // max_tokens 100: the remaining-window arm needs used > 900, so these
    // firings can only come from the 80% arm.
    let mut session = session_with_window(
        dir.path(),
        vec![
            // used 799: one below the 80% threshold, silent.
            msg_with_usage(
                vec![text("a")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 700, "output_tokens": 99}),
            ),
            // used exactly 800 = 80% of 1000: fires.
            msg_with_usage(
                vec![text("b")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 700, "output_tokens": 100}),
            ),
        ],
        Some(1000),
        100,
    );
    let n1 = notices(&collect_events(&mut session, "one"));
    assert!(n1.is_empty(), "799 of 1000 is below 80%: {n1:?}");
    let n2 = notices(&collect_events(&mut session, "two"));
    assert_eq!(n2.len(), 1, "800 of 1000 crosses 80%: {n2:?}");
    assert!(n2[0].contains("context: ~800 of 1000 tokens used"));
    assert!(n2[0].contains("/compact frees the window"));
}

#[test]
fn no_context_window_means_no_advisory_ever() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = session_with_window(
        dir.path(),
        vec![msg_with_usage(
            vec![text("a")],
            StopReason::EndTurn,
            serde_json::json!({"input_tokens": 900_000, "output_tokens": 100}),
        )],
        None,
        100,
    );
    let n = notices(&collect_events(&mut session, "one"));
    assert!(n.is_empty(), "no window, no advisory: {n:?}");
}

// The resume-time trigger (T20): a session rebuilt from a seed whose
// RESTORED estimate already crosses the threshold advises immediately.

fn resumed_with_window(
    dir: &std::path::Path,
    last_context_used: Option<u64>,
    responses: Vec<ResponseMessage>,
    context_window: Option<u64>,
    max_tokens: u32,
) -> Session {
    let provider = MockProvider {
        responses: RefCell::new(responses),
        requests: Rc::new(RefCell::new(vec![])),
    };
    let cfg = SessionConfig {
        model: "claude-sonnet-5".into(),
        max_tokens,
        system: None,
        thinking: false,
        cwd: dir.to_path_buf(),
        max_iterations: 50,
        temperature: None,
        top_p: None,
        context_window,
        max_tokens_source: None,
        prose_tool_calls: true,
        cost_rates: None,
        cost_advisory_step_usd: temur::config::DEFAULT_COST_ADVISORY_STEP_USD,
        auto_compact: false,
    };
    let mut file = saved(
        vec![user_msg("old prompt"), assistant_msg(vec![text("old answer")])],
        vec![],
    );
    file.last_context_used = last_context_used;
    let (seed, _) = store::prepare_seed(file);
    Session::resume(Box::new(provider), Registry::standard(), cfg, seed)
}

/// The seam is what main.rs and `/resume` actually call, so it is what a
/// test of the resume-time advisory should drive. F7 (v0.29.1) removed the
/// `Session::context_advisory()` accessor these tests used: it had no
/// production callers left after the T40 rider and was drifting away from
/// the path that really runs.
fn seam_notices(session: &mut Session) -> Vec<String> {
    let mut events = vec![];
    session.resume_seam_context_action(&mut |e| events.push(e));
    notices(&events)
}

#[test]
fn resume_time_advisory_fires_once_and_latches_across_trigger_paths() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = resumed_with_window(
        dir.path(),
        Some(900),
        vec![msg_with_usage(
            vec![text("a")],
            StopReason::EndTurn,
            serde_json::json!({"input_tokens": 850, "output_tokens": 60}),
        )],
        Some(1000),
        100,
    );
    let fired = seam_notices(&mut session);
    assert_eq!(fired.len(), 1, "restored 900 of 1000 must advise: {fired:?}");
    assert!(fired[0].contains("context: ~900 of 1000 tokens used"));
    assert!(fired[0].contains("/compact frees the window"));
    assert!(fired[0].contains("or start a new session"));
    // Latch: the seam itself does not re-fire...
    assert!(seam_notices(&mut session).is_empty());
    // ...and the OTHER trigger path (the turn loop) honors the same latch
    // even though this turn's usage (910 of 1000) crosses again.
    let n = notices(&collect_events(&mut session, "next"));
    assert!(n.is_empty(), "latched across trigger paths: {n:?}");
}

#[test]
fn resume_time_advisory_stays_silent_below_threshold_or_without_window() {
    let dir = tempfile::tempdir().unwrap();
    // Below both arms: 100 of 1000 with max_tokens 100.
    let mut session = resumed_with_window(dir.path(), Some(100), vec![], Some(1000), 100);
    assert!(seam_notices(&mut session).is_empty());
    // No window configured: never advises, whatever the estimate says.
    let mut session = resumed_with_window(dir.path(), Some(900), vec![], None, 100);
    assert!(seam_notices(&mut session).is_empty());
    // No restored estimate: nothing to judge.
    let mut session = resumed_with_window(dir.path(), None, vec![], Some(1000), 100);
    assert!(seam_notices(&mut session).is_empty());
}

#[test]
fn resume_command_emits_the_advisory_when_the_restored_estimate_is_hot() {
    let dir = tempfile::tempdir().unwrap();
    let sdir = tempfile::tempdir().unwrap();
    // The LIVE session has a window; the file being resumed restores a hot
    // estimate: /resume itself must carry the advisory notice.
    let mut session = session_with_window(dir.path(), vec![], Some(1000), 100);
    let mut h = CmdHarness::new();
    h.sessions_dir = sdir.path().to_path_buf();

    let history = vec![user_msg("old prompt"), assistant_msg(vec![text("old answer")])];
    let r = store::SessionFileRef {
        version: FORMAT_VERSION,
        provider: "anthropic",
        model: "claude-sonnet-5",
        cwd: "/test",
        history: &history,
        session_usage: Usage::default(),
        todos: &[],
        last_context_used: Some(900),
        name: Some("hot"),
    };
    store::save(
        &sdir.path().join("test-9999-hot.json"),
        &r,
        temur::config::DEFAULT_SESSION_MAX_BYTES,
        &mut |_| {},
    )
    .unwrap();

    let events = commands::run(commands::parse("/resume hot"), &mut h.ctx(&mut session, &no_build));
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::SessionLoaded { .. })),
        "{events:?}"
    );
    let ns = notices(&events);
    assert!(
        ns.iter().any(|n| n.contains("context: ~900 of 1000 tokens used")
            && n.contains("/compact frees the window")),
        "resume-time advisory rides the /resume events: {ns:?}"
    );
}

#[test]
fn max_tokens_near_window_gets_context_overflow_wording() {
    let dir = tempfile::tempdir().unwrap();
    // used 250 + max_tokens 800 >= window 1000 → overflow wording.
    let mut session = session_with_window(
        dir.path(),
        vec![msg_with_usage(
            vec![text("truncated tex")],
            StopReason::MaxTokens,
            serde_json::json!({"input_tokens": 150, "output_tokens": 100}),
        )],
        Some(1000),
        800,
    );
    let events = collect_events(&mut session, "hi");
    let notice = notices(&events)
        .into_iter()
        .find(|n| n.contains("response truncated"))
        .expect("truncation notice");
    assert!(notice.contains("max_tokens reached near the context window"));
    assert!(notice.contains("~250 of 1000 tokens"));
    assert!(notice.contains("likely context overflow"));
    assert!(notice.contains("consider starting a new session"));
}

#[test]
fn max_tokens_without_window_names_the_limit_and_its_source() {
    let dir = tempfile::tempdir().unwrap();
    // T16: the plain (not near-window) notice names the limit and where it
    // came from; no active profile → "from config".
    let mut session = session_with_window(
        dir.path(),
        vec![msg_with_usage(
            vec![text("t")],
            StopReason::MaxTokens,
            serde_json::json!({"input_tokens": 150, "output_tokens": 100}),
        )],
        None,
        800,
    );
    let events = collect_events(&mut session, "hi");
    assert!(notices(&events)
        .iter()
        .any(|n| n == "response truncated: max_tokens (800, from config) reached; raise max_tokens in config.json"),
        "{events:?}");
}

#[test]
fn max_tokens_notice_names_the_profile_after_a_switch() {
    let dir = tempfile::tempdir().unwrap();
    // A profile-supplied limit: the notice names the profile.
    let mut session = session_with_window(dir.path(), vec![], None, 800);
    let truncated = MockProvider {
        responses: RefCell::new(vec![msg_with_usage(
            vec![text("t")],
            StopReason::MaxTokens,
            serde_json::json!({"input_tokens": 150, "output_tokens": 100}),
        )]),
        requests: Rc::new(RefCell::new(vec![])),
    };
    session.switch_provider(
        Box::new(truncated),
        &selection("qwen3-1.7b", 1024, None),
        Some("local".into()),
    );
    let events = collect_events(&mut session, "hi");
    assert!(notices(&events)
        .iter()
        .any(|n| n == "response truncated: max_tokens (1024, from profile \"local\") reached; raise max_tokens in config.json"),
        "{events:?}");
}

#[test]
fn truncation_is_reported_when_the_response_also_made_tool_calls() {
    // T13 F10: a response can both assemble tool calls and hit the limit.
    // The provider says ToolUse (the calls must run) and carries the
    // truncation in stop_details; the agent runs the tools AND reports the
    // truncation, in that order, with the usual wording.
    let dir = tempfile::tempdir().unwrap();
    let mut truncated = msg_with_usage(
        vec![tool_use("call_1", "read", serde_json::json!({"filePath": "nope.txt"}))],
        StopReason::ToolUse,
        serde_json::json!({"input_tokens": 150, "output_tokens": 100}),
    );
    truncated.stop_details = Some(StopDetails {
        kind: "max_tokens".into(),
        category: None,
        explanation: None,
    });
    let mut session = session_with_window(
        dir.path(),
        vec![truncated, msg(vec![text("done")], StopReason::EndTurn)],
        None,
        800,
    );
    let events = collect_events(&mut session, "hi");
    assert!(notices(&events)
        .iter()
        .any(|n| n == "response truncated: max_tokens (800, from config) reached; raise max_tokens in config.json"),
        "{events:?}");
    // The tool still ran, and the notice preceded it.
    let notice_at = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Notice(n) if n.contains("response truncated")))
        .expect("truncation notice");
    let tool_at = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolEnd { .. }))
        .expect("the tool call was dispatched");
    assert!(notice_at < tool_at, "{events:?}");
}

#[test]
fn no_usage_reported_stays_silent_despite_window() {
    let dir = tempfile::tempdir().unwrap();
    // Quirk server: never reports usage. A tiny window would certainly warn
    // if an estimate existed; without one the heuristic stays silent
    // instead of inventing numbers.
    let mut session = session_with_window(
        dir.path(),
        vec![msg_with_usage(
            vec![text("ok")],
            StopReason::EndTurn,
            serde_json::json!({}),
        )],
        Some(100),
        32_000,
    );
    let events = collect_events(&mut session, "hi");
    assert!(notices(&events).is_empty());
}

#[test]
fn max_tokens_notice_and_history_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(
        dir.path(),
        vec![msg(vec![text("truncated tex")], StopReason::MaxTokens)],
    );
    let events = collect_events(&mut session, "hi");
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Notice(n) if n.contains("max_tokens")
    )));
    assert_eq!(session.history().len(), 2); // truncated output IS kept
}

// ------------------------------------------------------- T5 resume seam

use temur::session_store::{self as store, SessionFile, FORMAT_VERSION};

/// Same harness as `session_with`, but the session is rebuilt from a saved
/// seed instead of started empty.
fn resumed_with(
    dir: &std::path::Path,
    file: SessionFile,
    responses: Vec<ResponseMessage>,
) -> (Session, Rc<RefCell<Vec<ChatRequest>>>, Vec<String>) {
    let requests = Rc::new(RefCell::new(vec![]));
    let provider = MockProvider {
        responses: RefCell::new(responses),
        requests: requests.clone(),
    };
    let cfg = SessionConfig {
        model: "claude-sonnet-5".into(),
        max_tokens: 32_000,
        system: Some("test system".into()),
        thinking: false,
        cwd: dir.to_path_buf(),
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
    let (seed, notices) = store::prepare_seed(file);
    (
        Session::resume(Box::new(provider), Registry::standard(), cfg, seed),
        requests,
        notices,
    )
}

fn saved(history: Vec<RequestMessage>, todos: Vec<temur::tools::TodoItem>) -> SessionFile {
    SessionFile {
        version: FORMAT_VERSION,
        provider: "anthropic".into(),
        model: "claude-sonnet-5".into(),
        cwd: "/work".into(),
        history,
        session_usage: Usage {
            input_tokens: Some(1000),
            output_tokens: Some(200),
            ..Default::default()
        },
        todos,
        last_context_used: Some(1200),
        name: None,
    }
}

fn user_msg(t: &str) -> RequestMessage {
    RequestMessage {
        role: Role::User,
        content: vec![text(t)],
    }
}

fn assistant_msg(content: Vec<ContentBlock>) -> RequestMessage {
    RequestMessage {
        role: Role::Assistant,
        content,
    }
}

#[test]
fn resumed_history_is_replayed_ahead_of_the_new_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let file = saved(
        vec![
            user_msg("what is in a.txt?"),
            assistant_msg(vec![text("it says hello")]),
        ],
        vec![],
    );
    let (mut session, requests, _) = resumed_with(
        dir.path(),
        file,
        vec![msg(vec![text("and b.txt says world")], StopReason::EndTurn)],
    );
    collect_events(&mut session, "and b.txt?");

    let reqs = requests.borrow();
    assert_eq!(reqs.len(), 1);
    let sent = &reqs[0].messages;
    // The seeded exchange goes out ahead of the new prompt, in order.
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0], user_msg("what is in a.txt?"));
    assert_eq!(sent[1], assistant_msg(vec![text("it says hello")]));
    assert_eq!(sent[2], user_msg("and b.txt?"));

    // Accumulated usage continues from the saved totals rather than restarting.
    assert_eq!(session.snapshot().session_usage.input_tokens, Some(1010));
    assert_eq!(session.snapshot().last_context_used, Some(15));
}

#[test]
fn resume_drops_a_trailing_unanswered_prompt() {
    let dir = tempfile::tempdir().unwrap();
    // The provider-error shape: the prompt was saved, the answer never came.
    let file = saved(
        vec![
            user_msg("first"),
            assistant_msg(vec![text("ok")]),
            user_msg("this one errored out"),
        ],
        vec![],
    );
    let (mut session, requests, notices) = resumed_with(
        dir.path(),
        file,
        vec![msg(vec![text("fine")], StopReason::EndTurn)],
    );
    assert!(notices.iter().any(|n| n.contains("never answered")), "{notices:?}");
    collect_events(&mut session, "try again");

    let reqs = requests.borrow();
    let sent = &reqs[0].messages;
    assert_eq!(sent.len(), 3, "stale prompt must not be replayed: {sent:?}");
    assert_eq!(sent[2], user_msg("try again"));
    assert!(!sent.iter().any(|m| m == &user_msg("this one errored out")));
}

#[test]
fn resume_keeps_a_trailing_tool_result_message() {
    let dir = tempfile::tempdir().unwrap();
    // A guard-stopped turn ends exactly like this: results delivered, no
    // assistant reply. It is factual and wire-valid, so it is kept.
    let file = saved(
        vec![
            user_msg("read it"),
            assistant_msg(vec![tool_use("t1", "read", serde_json::json!({}))]),
            RequestMessage {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: "file body".into(),
                    is_error: false,
                }],
            },
        ],
        vec![],
    );
    let (mut session, requests, notices) = resumed_with(
        dir.path(),
        file,
        vec![msg(vec![text("got it")], StopReason::EndTurn)],
    );
    assert!(!notices.iter().any(|n| n.contains("never answered")), "{notices:?}");
    collect_events(&mut session, "continue");

    let reqs = requests.borrow();
    let sent = &reqs[0].messages;
    assert_eq!(sent.len(), 4);
    assert!(matches!(
        sent[2].content[0],
        ContentBlock::ToolResult { .. }
    ));
}

#[test]
fn seeded_todos_are_visible_to_the_todoread_tool() {
    let dir = tempfile::tempdir().unwrap();
    let file = saved(
        vec![user_msg("plan the work"), assistant_msg(vec![text("planned")])],
        vec![temur::tools::TodoItem {
            id: Some("1".into()),
            content: "finish the migration".into(),
            status: "in_progress".into(),
        }],
    );
    let (mut session, _requests, _) = resumed_with(
        dir.path(),
        file,
        vec![
            msg(
                vec![tool_use("t1", "todoread", serde_json::json!({}))],
                StopReason::ToolUse,
            ),
            msg(vec![text("one task still open")], StopReason::EndTurn),
        ],
    );
    collect_events(&mut session, "what is left?");

    // The tool result the model saw must contain the restored todo.
    let result = session
        .history()
        .iter()
        .flat_map(|m| &m.content)
        .find_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .expect("a todoread tool_result");
    assert!(
        result.contains("finish the migration"),
        "seeded todos lost: {result}"
    );
}

#[test]
fn snapshot_reports_exactly_what_gets_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(
        dir.path(),
        vec![
            msg(
                vec![tool_use(
                    "t1",
                    "todowrite",
                    serde_json::json!({"todos": [{"content": "a task", "status": "pending"}]}),
                )],
                StopReason::ToolUse,
            ),
            msg(vec![text("noted")], StopReason::EndTurn),
        ],
    );
    collect_events(&mut session, "add a task");

    let snap = session.snapshot();
    assert_eq!(snap.history.len(), session.history().len());
    assert_eq!(snap.todos.len(), 1);
    assert_eq!(snap.todos[0].content, "a task");
    assert_eq!(snap.session_usage.input_tokens, Some(20)); // two round-trips
    assert_eq!(snap.last_context_used, Some(15));
}

// ---------------------------------------------------------------------------
// T6 interruption (I2): the turn landing policy. A scripted provider sets
// the cancel token as it returns — modeling a mid-stream Esc with zero
// timing — and every case is checked against the wire rule that makes the
// landed history resumable.
// ---------------------------------------------------------------------------

/// Scripted provider for interruption cases: each entry is (set_cancel,
/// stream outcome). `set_cancel: true` models the token being set while
/// that response streamed.
struct InterruptingProvider {
    responses: RefCell<Vec<(bool, Result<ResponseMessage, ProviderError>)>>,
}

impl Provider for InterruptingProvider {
    fn stream(
        &self,
        _req: &ChatRequest,
        _on_event: &mut dyn FnMut(StreamEvent),
        cancel: &CancelToken,
    ) -> Result<ResponseMessage, ProviderError> {
        let (set_cancel, resp) = self.responses.borrow_mut().remove(0);
        if set_cancel {
            cancel.set();
        }
        resp
    }
}

fn interrupt_session(
    dir: &std::path::Path,
    responses: Vec<(bool, Result<ResponseMessage, ProviderError>)>,
) -> Session {
    let cfg = SessionConfig {
        model: "claude-sonnet-5".into(),
        max_tokens: 32_000,
        system: Some("test system".into()),
        thinking: false,
        cwd: dir.to_path_buf(),
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
    Session::new(
        Box::new(InterruptingProvider {
            responses: RefCell::new(responses),
        }),
        Registry::standard(),
        cfg,
    )
}

/// A cancelled stream's partial message: same shape as `msg` but with no
/// stop reason (the message_delta never arrived).
fn partial(content: Vec<ContentBlock>) -> ResponseMessage {
    let mut m = msg(content, StopReason::EndTurn);
    m.stop_reason = None;
    m
}

/// A tool_use whose streamed arguments never completed (`input_raw` set) —
/// exactly what the T4 accumulators deliver for a cancel mid tool-JSON.
fn tool_use_incomplete(id: &str, name: &str, raw: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: id.into(),
        name: name.into(),
        input: serde_json::json!({}),
        input_raw: Some(raw.into()),
        provider_state: None,
    }
}

/// WIRE RULE (the invariant that makes interrupted sessions resumable):
/// every assistant tool_use id must be answered by a tool_result in the
/// immediately following user message.
fn assert_history_wire_valid(history: &[RequestMessage]) {
    for (i, m) in history.iter().enumerate() {
        if m.role != Role::Assistant {
            continue;
        }
        let ids: Vec<&str> = m
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        if ids.is_empty() {
            continue;
        }
        let next = history
            .get(i + 1)
            .unwrap_or_else(|| panic!("history ends on a tool_use message (index {i})"));
        assert_eq!(next.role, Role::User, "tool_use not followed by user msg");
        for id in ids {
            assert!(
                next.content.iter().any(|b| matches!(
                    b,
                    ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == id
                )),
                "tool_use {id} has no tool_result in the next message"
            );
        }
    }
}

/// Runs one interrupted turn and applies the assertions every case shares:
/// turn returns Ok, the "turn interrupted" notice fires (possibly with an
/// F5 "request had failed" suffix), TurnComplete is last, and the landed
/// history is wire-valid.
fn run_interrupted(session: &mut Session, input: &str) -> Vec<AgentEvent> {
    let mut events = vec![];
    session.turn(input, &mut |e| events.push(e)).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Notice(n) if n.starts_with("turn interrupted"))),
        "missing interrupt notice: {events:?}"
    );
    assert!(
        matches!(events.last(), Some(AgentEvent::TurnComplete { .. })),
        "TurnComplete must always be emitted: {events:?}"
    );
    assert_history_wire_valid(session.history());
    events
}

#[test]
fn interrupt_with_partial_text_keeps_the_text() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = interrupt_session(
        dir.path(),
        vec![(true, Ok(partial(vec![text("partial tail")])))],
    );
    run_interrupted(&mut session, "go");

    assert_eq!(session.history().len(), 2);
    assert_eq!(session.history()[1].role, Role::Assistant);
    assert_eq!(
        session.history()[1].content,
        vec![text("partial tail")],
        "completed text must be kept"
    );
}

#[test]
fn interrupt_drops_incomplete_tool_use_and_synthesizes_result_for_kept_one() {
    let dir = tempfile::tempdir().unwrap();
    let side_effect = dir.path().join("side.txt");
    let mut session = interrupt_session(
        dir.path(),
        vec![(
            true,
            Ok(partial(vec![
                tool_use(
                    "t1",
                    "write",
                    serde_json::json!({
                        "filePath": side_effect.to_str().unwrap(),
                        "content": "must never be written"
                    }),
                ),
                tool_use_incomplete("t2", "bash", "{\"comm"),
            ])),
        )],
    );
    let events = run_interrupted(&mut session, "go");

    // Kept complete call, dropped incomplete one, ONE synthesized result.
    assert_eq!(session.history().len(), 3);
    match &session.history()[1].content[..] {
        [ContentBlock::ToolUse { id, input_raw, .. }] => {
            assert_eq!(id, "t1");
            assert!(input_raw.is_none());
        }
        other => panic!("incomplete tool_use must be dropped: {other:?}"),
    }
    match &session.history()[2].content[..] {
        [ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        }] => {
            assert_eq!(tool_use_id, "t1");
            assert_eq!(content, INTERRUPT_MARKER);
            assert!(is_error);
        }
        other => panic!("expected one synthesized result: {other:?}"),
    }
    // The kept call was NEVER executed.
    assert!(!side_effect.exists(), "interrupted tool must not run");
    // Both streamed cells were closed (kept and dropped), preserving FIFO.
    let ends: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolEnd { name, is_error: true, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ends, vec!["write", "bash"]);
}

#[test]
fn interrupt_between_stream_end_and_tool_exec_synthesizes_all() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = interrupt_session(
        dir.path(),
        vec![(
            true,
            Ok(msg(
                vec![
                    tool_use("t1", "read", serde_json::json!({"filePath": "/nope"})),
                    tool_use("t2", "bash", serde_json::json!({"command": "true"})),
                ],
                StopReason::ToolUse,
            )),
        )],
    );
    run_interrupted(&mut session, "go");

    assert_eq!(session.history().len(), 3);
    let results: Vec<(&str, &str)> = session.history()[2]
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error: true,
            } => Some((tool_use_id.as_str(), content.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        vec![
            ("t1", INTERRUPT_MARKER),
            ("t2", INTERRUPT_MARKER),
        ],
        "every kept call gets a synthesized result in ONE message"
    );
}

#[test]
fn interrupt_before_first_byte_lands_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut session =
        interrupt_session(dir.path(), vec![(true, Err(ProviderError::Incomplete))]);
    run_interrupted(&mut session, "go");

    // History ends with the plain user prompt; the resume seam's
    // dangling-prompt rule handles it on --continue.
    assert_eq!(session.history().len(), 1);
    assert_eq!(session.history()[0].role, Role::User);
}

#[test]
fn interrupt_treats_transport_error_under_cancel_as_interruption() {
    // D4 + F5: Err + token set is still the user's interrupt (turn returns
    // Ok, no fatal "provider error") — but the real failure is no longer
    // swallowed: the interrupt notice carries it.
    let dir = tempfile::tempdir().unwrap();
    let mut session = interrupt_session(
        dir.path(),
        vec![(true, Err(ProviderError::Network("reset by peer".into())))],
    );
    let events = run_interrupted(&mut session, "go");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Notice(n) if n.contains("provider error"))),
        "{events:?}"
    );
    let notice = notices(&events)
        .into_iter()
        .find(|n| n.starts_with("turn interrupted"))
        .unwrap();
    assert!(
        notice.contains("request had failed") && notice.contains("reset by peer"),
        "real failure must surface in the notice: {notice}"
    );
    assert_eq!(session.history().len(), 1);
}

#[test]
fn interrupt_with_api_error_surfaces_the_error_text() {
    // F5(b): the user pressed Esc while the request had ALREADY failed with
    // a real API error (e.g. 401) — the notice must include the API error
    // text, not swallow it into a bare "turn interrupted".
    let dir = tempfile::tempdir().unwrap();
    let mut session = interrupt_session(
        dir.path(),
        vec![(
            true,
            Err(ProviderError::Api {
                status: 401,
                kind: "authentication_error".into(),
                message: "invalid x-api-key".into(),
            }),
        )],
    );
    let events = run_interrupted(&mut session, "go");
    let notice = notices(&events)
        .into_iter()
        .find(|n| n.starts_with("turn interrupted"))
        .unwrap();
    assert!(
        notice.contains("request had failed") && notice.contains("invalid x-api-key"),
        "API error text must reach the notice: {notice}"
    );
    assert_eq!(session.history().len(), 1, "nothing to land");
}

#[test]
fn interrupt_before_first_byte_keeps_the_plain_notice() {
    // F5 boundary: Incomplete is the provider's own "cancelled before
    // anything happened", not a failure — the notice stays exactly
    // "turn interrupted" with no suffix.
    let dir = tempfile::tempdir().unwrap();
    let mut session =
        interrupt_session(dir.path(), vec![(true, Err(ProviderError::Incomplete))]);
    let events = run_interrupted(&mut session, "go");
    assert!(
        notices(&events).iter().any(|n| n == "turn interrupted"),
        "no suffix for the self-inflicted Incomplete: {events:?}"
    );
}

#[test]
fn interrupt_with_signed_thinking_only_pushes_nothing() {
    // F6: a landing that keeps only thinking blocks (no text, no tool_use)
    // must push NOTHING — a thinking-only assistant message is rejected on
    // replay (400), which would brick the saved session.
    let dir = tempfile::tempdir().unwrap();
    let signed = ContentBlock::Thinking {
        thinking: "a full thought".into(),
        signature: Some("sig".into()),
    };
    let mut session = interrupt_session(dir.path(), vec![(true, Ok(partial(vec![signed])))]);
    run_interrupted(&mut session, "go");
    assert_eq!(
        session.history().len(),
        1,
        "history must end at the user prompt"
    );
}

#[test]
fn interrupt_with_redacted_thinking_only_pushes_nothing() {
    // F6, redacted variant: kept-but-not-substantive content lands nothing.
    let dir = tempfile::tempdir().unwrap();
    let redacted = ContentBlock::RedactedThinking {
        data: "opaque".into(),
    };
    let mut session = interrupt_session(dir.path(), vec![(true, Ok(partial(vec![redacted])))]);
    run_interrupted(&mut session, "go");
    assert_eq!(session.history().len(), 1);
}

#[test]
fn interrupt_drops_unsigned_thinking_keeps_signed() {
    let dir = tempfile::tempdir().unwrap();
    let unsigned = ContentBlock::Thinking {
        thinking: "half a thought".into(),
        signature: None,
    };
    let signed = ContentBlock::Thinking {
        thinking: "a full thought".into(),
        signature: Some("sig".into()),
    };
    let mut session = interrupt_session(
        dir.path(),
        vec![(
            true,
            Ok(partial(vec![unsigned, signed.clone(), text("tail")])),
        )],
    );
    run_interrupted(&mut session, "go");

    assert_eq!(session.history().len(), 2);
    assert_eq!(
        session.history()[1].content,
        vec![signed, text("tail")],
        "unsigned thinking is rejected on replay and must be dropped"
    );
}

#[test]
fn interrupt_with_only_droppable_content_lands_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = interrupt_session(
        dir.path(),
        vec![(
            true,
            Ok(partial(vec![ContentBlock::Thinking {
                thinking: "half".into(),
                signature: None,
            }])),
        )],
    );
    run_interrupted(&mut session, "go");
    assert_eq!(session.history().len(), 1, "empty landing pushes nothing");
}

#[test]
fn turn_no_longer_clears_the_token_callers_clear_at_submission() {
    // F7 INVARIANT INVERSION: `Session::turn` used to clear the token at
    // entry as a stale-flag defense, but that clear raced a real Esc landing
    // between submission and turn entry and silently dropped the interrupt.
    // The clear now belongs to the submitting component (TUI Submit arm /
    // plain REPL after read_input — see tests/tui.rs for the seam test);
    // a token that is set when turn runs IS an interrupt.
    let dir = tempfile::tempdir().unwrap();
    let mut session = interrupt_session(
        dir.path(),
        vec![(false, Ok(msg(vec![text("Hi there")], StopReason::EndTurn)))],
    );
    session.cancel_token().set();
    let events = run_interrupted(&mut session, "hello");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Notice(n) if n.starts_with("turn interrupted"))),
        "a set token at turn time is an interrupt now: {events:?}"
    );
}

#[test]
fn interrupt_mid_batch_aborts_running_bash_and_synthesizes_the_rest() {
    // First call: bash sleeping 30 s. Second call: a write whose side
    // effect must never appear. The token is set while bash runs — the
    // running call aborts with the marker, the pending call is synthesized
    // without executing, and both land in ONE results message.
    let dir = tempfile::tempdir().unwrap();
    let side_effect = dir.path().join("must-not-exist.txt");
    let mut session = interrupt_session(
        dir.path(),
        vec![(
            false,
            Ok(msg(
                vec![
                    tool_use("t1", "bash", serde_json::json!({"command": "sleep 30"})),
                    tool_use(
                        "t2",
                        "write",
                        serde_json::json!({
                            "filePath": side_effect.to_str().unwrap(),
                            "content": "boom"
                        }),
                    ),
                ],
                StopReason::ToolUse,
            )),
        )],
    );

    let token = session.cancel_token();
    let setter = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        token.set();
    });
    let start = std::time::Instant::now();
    let events = run_interrupted(&mut session, "go");
    setter.join().unwrap();

    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "turn must land promptly (took {:?})",
        start.elapsed()
    );
    assert_eq!(session.history().len(), 3);
    match &session.history()[2].content[..] {
        [ContentBlock::ToolResult {
            tool_use_id: id1,
            content: c1,
            is_error: true,
        }, ContentBlock::ToolResult {
            tool_use_id: id2,
            content: c2,
            is_error: true,
        }] => {
            assert_eq!(id1, "t1");
            assert!(
                c1.contains("(interrupted by user)"),
                "aborted bash carries the marker: {c1}"
            );
            assert_eq!(id2, "t2");
            assert_eq!(c2, INTERRUPT_MARKER, "pending call synthesized");
        }
        other => panic!("expected two error results in one message: {other:?}"),
    }
    assert!(!side_effect.exists(), "pending write must never execute");
    // Both cells closed: the executed-then-aborted bash and the synthesized
    // write each got a ToolEnd.
    let ends = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolEnd { is_error: true, .. }))
        .count();
    assert_eq!(ends, 2);
}
// ---- to append to tests/agent.rs (T6 E3) ----

// ------------------------------------------------------------ T6 (E3): fuzzy

/// A weak model reproduced oldString with mangled indentation (spaces for
/// the file's tab): the edit must land via the whitespace-tolerant
/// fallback, the file must be corrected on disk, and the tool_result the
/// model reads back must carry the fuzzy marker with is_error false.
#[test]
fn fuzzy_edit_lands_end_to_end_through_the_agent_loop() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("code.rs");
    std::fs::write(&file, "fn main() {\n\tlet x = 1;\n}\n").unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(
                vec![tool_use(
                    "t1",
                    "edit",
                    serde_json::json!({
                        "filePath": file.to_str().unwrap(),
                        "oldString": "    let x = 1;",
                        "newString": "    let y = 2;"
                    }),
                )],
                StopReason::ToolUse,
            ),
            msg(vec![text("done")], StopReason::EndTurn),
        ],
    );
    let events = collect_events(&mut session, "rename x to y");

    // F3: the file's tab indentation survives the splice — the model's
    // 4-space prefix is swapped for the matched line's tab.
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn main() {\n\tlet y = 2;\n}\n"
    );
    let second = &requests.borrow()[1];
    match &second.messages.last().unwrap().content[0] {
        ContentBlock::ToolResult {
            content, is_error, ..
        } => {
            assert!(!is_error, "fuzzy success is not an error: {content}");
            assert!(
                content.contains("whitespace-tolerant match"),
                "marker must round-trip to the model: {content}"
            );
        }
        other => panic!("expected tool_result, got {other:?}"),
    }
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolEnd { is_error: false, .. })));
}

/// Ambiguous fuzzy oldString: the "more surrounding lines" error feeds back
/// as a normal is_error tool_result — non-fatal, turn completes cleanly.
#[test]
fn ambiguous_fuzzy_edit_round_trips_as_nonfatal_error_result() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.txt");
    std::fs::write(&file, "a\nx\na\n").unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(
                vec![tool_use(
                    "t1",
                    "edit",
                    serde_json::json!({
                        "filePath": file.to_str().unwrap(),
                        "oldString": " a",
                        "newString": "b"
                    }),
                )],
                StopReason::ToolUse,
            ),
            msg(vec![text("I need more context.")], StopReason::EndTurn),
        ],
    );
    collect_events(&mut session, "edit the file");

    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "a\nx\na\n",
        "ambiguity must not touch the file"
    );
    let second = &requests.borrow()[1];
    match &second.messages.last().unwrap().content[0] {
        ContentBlock::ToolResult {
            content, is_error, ..
        } => {
            assert!(is_error);
            assert!(
                content.contains("more surrounding lines"),
                "ambiguity guidance must reach the model: {content}"
            );
        }
        other => panic!("expected tool_result, got {other:?}"),
    }
    assert_eq!(session.history().len(), 4, "turn completed normally");
}

// ---------------------------------------------------- T8: between-turns seam

/// Minimal recording transport over the openai fixture set, for proving
/// what actually goes on the wire after a `/model`-style switch.
struct RecordingTransport {
    fixture: &'static str,
    urls: Rc<RefCell<Vec<String>>>,
    bodies: Rc<RefCell<Vec<String>>>,
}

impl temur::provider::transport::Transport for RecordingTransport {
    fn post_stream(
        &self,
        url: &str,
        _api_key: &str,
        body: &str,
    ) -> Result<Box<dyn std::io::Read>, temur::provider::transport::TransportError> {
        self.urls.borrow_mut().push(url.to_string());
        self.bodies.borrow_mut().push(body.to_string());
        let path = format!(
            "{}/tests/fixtures/openai/{}.sse",
            env!("CARGO_MANIFEST_DIR"),
            self.fixture
        );
        Ok(Box::new(std::fs::File::open(path).unwrap()))
    }
}

#[test]
fn switch_provider_next_turn_hits_new_provider_with_full_history() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests_a) = session_with(
        dir.path(),
        vec![msg(vec![text("first answer")], StopReason::EndTurn)],
    );
    collect_events(&mut session, "first question");
    assert_eq!(requests_a.borrow().len(), 1);
    assert_eq!(session.model(), "claude-sonnet-5");

    let requests_b = Rc::new(RefCell::new(vec![]));
    let provider_b = MockProvider {
        responses: RefCell::new(vec![msg(vec![text("second answer")], StopReason::EndTurn)]),
        requests: requests_b.clone(),
    };
    session.switch_provider(Box::new(provider_b), &selection("model-b", 512, Some(9_999)), None);
    assert_eq!(session.model(), "model-b");
    assert_eq!(session.max_tokens(), 512);
    assert_eq!(session.context_window(), Some(9_999));

    collect_events(&mut session, "second question");
    assert_eq!(
        requests_a.borrow().len(),
        1,
        "the old provider must never be called again"
    );
    let reqs = requests_b.borrow();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].model, "model-b");
    assert_eq!(reqs[0].max_tokens, 512);
    // The FULL pre-switch history rides along: user1, assistant1, user2.
    assert_eq!(reqs[0].messages.len(), 3);
    match &reqs[0].messages[0].content[0] {
        ContentBlock::Text { text } => assert_eq!(text, "first question"),
        other => panic!("expected text, got {other:?}"),
    }
    assert_eq!(session.history().len(), 4, "both exchanges kept");
}

#[test]
fn switch_to_compat_hits_new_base_url_and_drops_thinking_blocks() {
    // Turn 1 (anthropic-flavored mock) leaves a SIGNED thinking block in
    // history; after switching to a real OpenAiCompatProvider the next
    // request must go to the new base_url with the new model, carry the
    // full history, and drop the thinking block at the wire boundary —
    // pre-switch behavior, regression-asserted across a switch.
    let dir = tempfile::tempdir().unwrap();
    let thinking = ContentBlock::Thinking {
        thinking: "private reasoning".into(),
        signature: Some("sig".into()),
    };
    let (mut session, _requests_a) = session_with(
        dir.path(),
        vec![msg(vec![thinking, text("first answer")], StopReason::EndTurn)],
    );
    collect_events(&mut session, "first question");

    let urls = Rc::new(RefCell::new(vec![]));
    let bodies = Rc::new(RefCell::new(vec![]));
    let compat = temur::provider::openai_compat::OpenAiCompatProvider::new(
        "http://switched.test/v1",
        None,
        temur::provider::MaxTokensParam::default(),
        Box::new(RecordingTransport {
            fixture: "text_simple",
            urls: urls.clone(),
            bodies: bodies.clone(),
        }),
    );
    session.switch_provider(Box::new(compat), &selection("qwen-sw", 1024, None), None);
    collect_events(&mut session, "second question");

    assert_eq!(urls.borrow().len(), 1);
    assert!(
        urls.borrow()[0].starts_with("http://switched.test/v1"),
        "request went to the switched endpoint: {}",
        urls.borrow()[0]
    );
    let body: serde_json::Value = serde_json::from_str(&bodies.borrow()[0]).unwrap();
    assert_eq!(body["model"], "qwen-sw");
    assert!(
        !bodies.borrow()[0].contains("private reasoning"),
        "thinking must be dropped at the compat wire boundary"
    );
    // Full history still crosses: system + user1 + assistant1 + user2.
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 4);
}

#[test]
fn failed_switch_is_never_partial_by_construction() {
    // Atomicity lives at the call site: the provider is built BEFORE
    // switch_provider is called. This asserts the session side — nothing
    // about a session changes until switch_provider actually runs.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(
        dir.path(),
        vec![msg(vec![text("answer")], StopReason::EndTurn)],
    );
    collect_events(&mut session, "question");
    let history_before = session.history().len();
    // A failed build (e.g. unreadable key file) simply never reaches
    // switch_provider; the command layer test proves that path end-to-end.
    assert_eq!(session.model(), "claude-sonnet-5");
    assert_eq!(session.history().len(), history_before);
}

#[test]
fn clear_history_wipes_state_and_next_turn_starts_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(
                vec![tool_use(
                    "t1",
                    "todowrite",
                    serde_json::json!({"todos": [{"content": "a task", "status": "pending"}]}),
                )],
                StopReason::ToolUse,
            ),
            msg(vec![text("done")], StopReason::EndTurn),
            msg(vec![text("fresh answer")], StopReason::EndTurn),
        ],
    );
    collect_events(&mut session, "make a todo");
    assert!(!session.history().is_empty());
    assert_eq!(session.snapshot().todos.len(), 1);
    assert!(session.session_usage().input_tokens.is_some());
    assert!(session.last_context_used().is_some());

    session.clear_history();
    assert!(session.history().is_empty());
    let snap = session.snapshot();
    assert!(snap.todos.is_empty(), "todos cleared via ToolCtx");
    assert_eq!(snap.session_usage, Usage::default());
    assert!(snap.last_context_used.is_none());

    // A fresh turn after /clear starts from scratch: exactly one message.
    collect_events(&mut session, "fresh question");
    let reqs = requests.borrow();
    assert_eq!(reqs.last().unwrap().messages.len(), 1);
}

#[test]
fn set_thinking_flips_the_next_request_and_getters_track() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(vec![text("a")], StopReason::EndTurn),
            msg(vec![text("b")], StopReason::EndTurn),
        ],
    );
    assert!(!session.thinking());
    collect_events(&mut session, "one");
    assert!(!requests.borrow()[0].thinking);

    session.set_thinking(true);
    assert!(session.thinking());
    collect_events(&mut session, "two");
    assert!(requests.borrow()[1].thinking);
}

// ------------------------------------------------------- T10: load_seed seam

#[test]
fn load_seed_swaps_state_between_turns_and_the_next_request_replays_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(vec![text("first answer")], StopReason::EndTurn),
            msg(vec![text("post-load answer")], StopReason::EndTurn),
        ],
    );
    collect_events(&mut session, "original conversation");
    assert_eq!(session.history().len(), 2);

    // A saved session from elsewhere, through the same prepare_seed gate
    // the commands layer uses.
    let file = saved(
        vec![
            user_msg("older prompt"),
            RequestMessage {
                role: Role::Assistant,
                content: vec![text("older answer")],
            },
        ],
        vec![temur::tools::TodoItem {
            id: Some("1".into()),
            content: "carried todo".into(),
            status: "in_progress".into(),
        }],
    );
    let (seed, _) = store::prepare_seed(file);
    session.load_seed(seed);

    // The snapshot now describes the LOADED session — history, usage
    // totals, todos, and the context estimate all replaced.
    let snap = session.snapshot();
    assert_eq!(snap.history.len(), 2);
    assert_eq!(snap.session_usage.input_tokens, Some(1000));
    assert_eq!(snap.session_usage.output_tokens, Some(200));
    assert_eq!(snap.todos.len(), 1);
    assert_eq!(snap.last_context_used, Some(1200));

    // And the next turn goes out on the loaded history + the new prompt.
    collect_events(&mut session, "next");
    let req = requests.borrow()[1].clone();
    assert_eq!(req.messages.len(), 3);
    assert!(matches!(
        &req.messages[0].content[0],
        ContentBlock::Text { text } if text == "older prompt"
    ));
    assert!(matches!(
        &req.messages[2].content[0],
        ContentBlock::Text { text } if text == "next"
    ));
    // Session totals continue FROM the seeded usage (1000+10 in, 200+5 out).
    assert_eq!(session.session_usage().input_tokens, Some(1010));
    assert_eq!(session.session_usage().output_tokens, Some(205));
}

// ------------------------------------------------------- T8: command layer

use temur::commands::{self, CommandCtx};
use temur::config::ResolvedProfile;
use temur::tools::PromptProfile;
use std::collections::BTreeMap;

fn two_profiles() -> BTreeMap<String, ResolvedProfile> {
    let mut m = BTreeMap::new();
    m.insert(
        "a".to_string(),
        ResolvedProfile {
            provider: "anthropic".into(),
            model: "model-a".into(),
            base_url: "https://a.test".into(),
            api_key_file: None,
            max_tokens: 111,
            context_window: None,
            prompt_profile: PromptProfile::Full,
            prompt_profile_source: Default::default(),
            price_input_per_mtok: None,
            price_output_per_mtok: None,
            max_tokens_parameter: Default::default(),
        },
    );
    m.insert(
        "b".to_string(),
        ResolvedProfile {
            provider: "openai-compat".into(),
            model: "model-b".into(),
            base_url: "http://b.test/v1".into(),
            api_key_file: None,
            max_tokens: 222,
            context_window: Some(4_096),
            prompt_profile: PromptProfile::Full,
            prompt_profile_source: Default::default(),
            price_input_per_mtok: None,
            price_output_per_mtok: None,
            max_tokens_parameter: Default::default(),
        },
    );
    m
}

/// Owns every loop-local the driver would; hands out a `CommandCtx` the same
/// way main.rs builds one per command line.
struct CmdHarness {
    profiles: BTreeMap<String, ResolvedProfile>,
    active: Option<String>,
    provider_name: String,
    model: String,
    persist: Option<std::path::PathBuf>,
    /// Mirrors main's sessions_dir local (T10). Defaults to a path that
    /// does not exist — session commands then see an empty listing.
    sessions_dir: std::path::PathBuf,
    /// Mirrors main's real-cwd local (T10; named-filename hashing).
    cwd: std::path::PathBuf,
    cwd_display: String,
    /// Mirrors main's session_name local (T10).
    session_name: Option<String>,
    replay: bool,
    prompt_profile: PromptProfile,
    /// Mirrors main's rebuild_system closure; tests swap it to model the
    /// config-override rule (a constant string regardless of profile).
    rebuild: Box<dyn Fn(PromptProfile) -> String>,
    /// Mirrors main's active_resolved local: the full active selection.
    active_resolved: ResolvedProfile,
    /// Mirrors main's cfg_path local (T15): the file `--save` edits.
    config_path: std::path::PathBuf,
    /// Mirrors main's list_models injection; tests swap in fakes.
    #[allow(clippy::type_complexity)]
    list: Box<
        dyn Fn(&ResolvedProfile) -> Result<Vec<temur::provider::ModelEntry>, temur::error::Error>,
    >,
    /// Mirrors main's cached_models local (T16; T22 windows): the last
    /// `/models` listing, empty until a test (or a listing) fills it.
    cached_models: Vec<temur::provider::ModelEntry>,
}

/// Listing entries with no windows, the pre-T22 shape most tests drive.
fn entries(ids: &[&str]) -> Vec<temur::provider::ModelEntry> {
    ids.iter()
        .map(|id| temur::provider::ModelEntry { id: id.to_string(), context_window: None })
        .collect()
}

/// The base (non-profile) selection the harness starts on, mirroring what
/// main's resolve_base produces for a default config.
fn base_resolved() -> ResolvedProfile {
    ResolvedProfile {
        provider: "anthropic".into(),
        model: "claude-sonnet-5".into(),
        base_url: "https://api.anthropic.com".into(),
        api_key_file: None,
        max_tokens: 32_000,
        context_window: None,
        prompt_profile: PromptProfile::Full,
        prompt_profile_source: Default::default(),
        price_input_per_mtok: None,
        price_output_per_mtok: None,
        max_tokens_parameter: Default::default(),
    }
}

/// A selection carrying only what `switch_provider` reads: the model and
/// its limits, on the unpriced anthropic base. The tests that switch for
/// a reason other than cost build theirs from this.
fn selection(model: &str, max_tokens: u32, context_window: Option<u64>) -> ResolvedProfile {
    ResolvedProfile {
        model: model.into(),
        max_tokens,
        context_window,
        ..base_resolved()
    }
}

/// What the default harness rebuild closure returns per profile — the
/// per-profile analogue of main's DEFAULT_SYSTEM / DEFAULT_SYSTEM_COMPACT.
fn test_system_for(p: PromptProfile) -> String {
    match p {
        PromptProfile::Full => "full test system".into(),
        PromptProfile::Compact => "compact test system".into(),
    }
}

impl CmdHarness {
    fn new() -> Self {
        CmdHarness {
            profiles: two_profiles(),
            active: None,
            provider_name: "anthropic".into(),
            model: "claude-sonnet-5".into(),
            persist: None,
            sessions_dir: "/nonexistent/temur-test-sessions".into(),
            cwd: "/test".into(),
            cwd_display: "/test".into(),
            session_name: None,
            replay: false,
            prompt_profile: PromptProfile::Full,
            rebuild: Box::new(test_system_for),
            active_resolved: base_resolved(),
            config_path: "/nonexistent/temur-test-config.json".into(),
            list: Box::new(|_| unreachable!("no list_models injected")),
            cached_models: Vec::new(),
        }
    }

    fn ctx<'a>(
        &'a mut self,
        session: &'a mut Session,
        build: &'a dyn Fn(
            &ResolvedProfile,
        ) -> Result<Box<dyn Provider>, temur::error::Error>,
    ) -> CommandCtx<'a> {
        CommandCtx {
            session,
            profiles: &self.profiles,
            active_profile: &mut self.active,
            provider_name: &mut self.provider_name,
            model: &mut self.model,
            persist_path: &mut self.persist,
            session_max_bytes: temur::config::DEFAULT_SESSION_MAX_BYTES,
            sessions_dir: &self.sessions_dir,
            cwd: &self.cwd,
            cwd_display: &self.cwd_display,
            session_name: &mut self.session_name,
            replay_mode: self.replay,
            prompt_profile: &mut self.prompt_profile,
            active_resolved: &mut self.active_resolved,
            config_path: &self.config_path,
            cached_models: &mut self.cached_models,
            build_provider: build,
            list_models: &*self.list,
            rebuild_system: &*self.rebuild,
        }
    }
}

#[test]
fn model_switch_updates_everything_and_next_turn_uses_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();

    let requests_b = Rc::new(RefCell::new(vec![]));
    let rb = requests_b.clone();
    let build = move |p: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        assert_eq!(p.model, "model-b");
        assert_eq!(p.base_url, "http://b.test/v1");
        Ok(Box::new(MockProvider {
            responses: RefCell::new(vec![msg(vec![text("hi from b")], StopReason::EndTurn)]),
            requests: rb.clone(),
        }))
    };
    let events = commands::run(
        commands::parse("/model b"),
        &mut h.ctx(&mut session, &build),
    );
    assert!(
        events.contains(&AgentEvent::ModelSwitched { model: "model-b".into(), provider: "openai-compat".into() }),
        "chrome signal present: {events:?}"
    );
    assert!(
        notices(&events).iter().any(|n| n == "switched to b (openai-compat · model-b)"),
        "confirmation notice: {events:?}"
    );
    assert_eq!(h.active.as_deref(), Some("b"));
    assert_eq!(h.provider_name, "openai-compat");
    assert_eq!(h.model, "model-b");
    assert_eq!(session.model(), "model-b");
    assert_eq!(session.max_tokens(), 222);
    assert_eq!(session.context_window(), Some(4_096));

    collect_events(&mut session, "hello");
    assert_eq!(requests_b.borrow()[0].model, "model-b");
    assert_eq!(requests_b.borrow()[0].max_tokens, 222);
    // T9: the full active selection tracked the switch too.
    assert_eq!(h.active_resolved, h.profiles["b"]);
}

#[test]
fn failed_switch_is_atomic_via_the_real_build_path() {
    // The REAL construction path (provider::build_live) with a key file
    // that does not exist: the switch must fail with a notice and leave
    // every observable session/loop fact untouched.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(
        dir.path(),
        vec![msg(vec![text("answer")], StopReason::EndTurn)],
    );
    collect_events(&mut session, "question");
    let history_before = session.history().len();

    let mut h = CmdHarness::new();
    h.profiles.get_mut("b").unwrap().api_key_file =
        Some("/nonexistent/temur-test/keyfile".into());
    let build = |p: &ResolvedProfile| temur::provider::build_live(p);
    let events = commands::run(
        commands::parse("/model b"),
        &mut h.ctx(&mut session, &build),
    );
    let ns = notices(&events);
    assert!(
        ns.iter().any(|n| n.contains("switch to \"b\" failed") && n.contains("session unchanged")),
        "failure notice: {ns:?}"
    );
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::ModelSwitched { .. })));
    assert_eq!(h.active, None, "active profile unchanged");
    assert_eq!(h.provider_name, "anthropic");
    assert_eq!(h.model, "claude-sonnet-5");
    assert_eq!(session.model(), "claude-sonnet-5");
    assert_eq!(session.history().len(), history_before);
}

#[test]
fn anthropic_profile_without_key_sources_is_a_notice_not_a_crash() {
    // Profile "a" has no api_key_file; with APP_SECRET_FILE unset the real
    // build path must surface a secret error as a switch-failed notice.
    if std::env::var_os("APP_SECRET_FILE").is_some() {
        // The build environment never sets this (CLAUDE.md); if some outer
        // harness does, this specific failure shape cannot be asserted.
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    let build = |p: &ResolvedProfile| temur::provider::build_live(p);
    let events = commands::run(
        commands::parse("/model a"),
        &mut h.ctx(&mut session, &build),
    );
    let ns = notices(&events);
    assert!(
        ns.iter().any(|n| n.contains("failed") && n.contains("session unchanged")),
        "notice, not crash: {ns:?}"
    );
    assert_eq!(session.model(), "claude-sonnet-5");
}

#[test]
fn same_profile_switch_is_a_friendly_noop() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    h.active = Some("b".into());
    let calls = Rc::new(RefCell::new(0u32));
    let c = calls.clone();
    let build = move |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        *c.borrow_mut() += 1;
        unreachable!("builder must not run for a same-profile switch")
    };
    let events = commands::run(
        commands::parse("/model b"),
        &mut h.ctx(&mut session, &build),
    );
    assert!(notices(&events).iter().any(|n| n.contains("already on profile")));
    assert_eq!(*calls.borrow(), 0);
}

#[test]
fn unknown_profile_unknown_command_and_bare_slash_touch_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!("no command here builds a provider")
    };
    // NOTE (T9): "/model nope" no longer belongs here — an unknown argument
    // is a raw-id switch attempt now (covered by the raw-id tests), not a
    // "no profile named" rejection.
    for (line, needle) in [
        ("/frobnicate", "unknown command"),
        ("/", "unknown command"),
        ("/thinking maybe", "usage: /thinking"),
    ] {
        let events = commands::run(commands::parse(line), &mut h.ctx(&mut session, &build));
        assert!(
            notices(&events).iter().any(|n| n.contains(needle)),
            "{line}: {events:?}"
        );
    }
    assert_eq!(session.history().len(), 0, "nothing reached history");
    assert_eq!(h.active, None);
}

#[test]
fn clear_persists_the_empty_session_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(
        dir.path(),
        vec![msg(vec![text("answer")], StopReason::EndTurn)],
    );
    collect_events(&mut session, "question");

    let path = dir.path().join("session.json");
    // Mimic the driver-loop save that would have happened after the turn.
    let snap = session.snapshot();
    let file = temur::session_store::SessionFileRef {
        version: temur::session_store::FORMAT_VERSION,
        provider: "anthropic",
        model: "claude-sonnet-5",
        cwd: "/test",
        history: snap.history,
        session_usage: snap.session_usage,
        todos: snap.todos,
        last_context_used: snap.last_context_used,
        name: None,
    };
    temur::session_store::save(&path, &file, temur::config::DEFAULT_SESSION_MAX_BYTES, &mut |_| {})
        .unwrap();
    assert!(!temur::session_store::load(&path).unwrap().history.is_empty());

    let mut h = CmdHarness::new();
    h.persist = Some(path.clone());
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!("clear builds nothing")
    };
    let events = commands::run(commands::parse("/clear"), &mut h.ctx(&mut session, &build));
    assert!(events.contains(&AgentEvent::SessionCleared));
    assert!(notices(&events).iter().any(|n| n == "session cleared"));
    assert!(session.history().is_empty());

    // The file on disk is already empty — quit + --continue resumes empty.
    let loaded = temur::session_store::load(&path).unwrap();
    assert!(loaded.history.is_empty());
    let (seed, _) = temur::session_store::prepare_seed(loaded);
    assert!(seed.history.is_empty());
}

// ------------------------------------------------------------ T20: /compact

/// Every call fails: the fail-closed arm of `/compact`.
struct FailingProvider;

impl Provider for FailingProvider {
    fn stream(
        &self,
        _req: &ChatRequest,
        _on_event: &mut dyn FnMut(StreamEvent),
        _cancel: &CancelToken,
    ) -> Result<ResponseMessage, ProviderError> {
        Err(ProviderError::Api {
            status: 500,
            kind: "server_error".into(),
            message: "boom".into(),
        })
    }
}

#[test]
fn compact_replaces_history_and_next_request_carries_summary_not_old_messages() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(vec![text("answer one")], StopReason::EndTurn),
            msg(vec![text("answer two")], StopReason::EndTurn),
            // The summary call's response.
            msg(vec![text("Goal: test compaction\nState: two turns done")], StopReason::EndTurn),
            msg(vec![text("continued")], StopReason::EndTurn),
        ],
    );
    collect_events(&mut session, "first question");
    collect_events(&mut session, "second question");
    assert_eq!(session.history().len(), 4);

    let out = session.compact();
    assert_eq!(out, CompactOutcome::Compacted { before: 4, after: 2 });
    // The estimate described the old conversation; it must reset.
    assert_eq!(session.last_context_used(), None);
    // Session usage keeps accumulating: 3 responses at 10 in / 5 out each.
    assert_eq!(session.session_usage().input_tokens, Some(30));
    assert_eq!(session.session_usage().output_tokens, Some(15));

    {
        // The summary request itself: tools omitted entirely, the whole
        // history present, and the final message carries the instruction.
        let reqs = requests.borrow();
        let sreq = &reqs[2];
        assert!(sreq.tools.is_empty(), "summary call must omit tools");
        assert_eq!(sreq.messages.len(), 5); // 4 history + 1 instruction
        let last = sreq.messages.last().unwrap();
        assert_eq!(last.role, Role::User);
        assert!(matches!(
            &last.content[0],
            ContentBlock::Text { text } if text.contains("Summarize this conversation")
        ));
        assert_eq!(sreq.system.as_deref(), Some("test system"));
    }

    // The next turn's request: summary + verbatim tail + new prompt, and
    // NONE of the summarized messages.
    collect_events(&mut session, "third question");
    let reqs = requests.borrow();
    let next = &reqs[3];
    assert_eq!(next.messages.len(), 3); // merged tail head, assistant, new prompt
    match &next.messages[0].content[..] {
        [ContentBlock::Text { text: summary }, ContentBlock::Text { text: orig }] => {
            assert!(summary.starts_with("[conversation summary (compacted)]"));
            assert!(summary.contains("Goal: test compaction"));
            assert_eq!(orig, "second question");
        }
        other => panic!("merged first message: {other:?}"),
    }
    let all_text: Vec<&str> = next
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(all_text.iter().any(|t| *t == "answer two"), "verbatim tail kept");
    assert!(
        !all_text.iter().any(|t| t.contains("first question") || t.contains("answer one")),
        "summarized messages must not reach the wire: {all_text:?}"
    );
}

#[test]
fn compact_provider_error_is_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(
        dir.path(),
        vec![msg(vec![text("answer")], StopReason::EndTurn)],
    );
    collect_events(&mut session, "question");
    let before = session.history().to_vec();
    session.switch_provider(
        Box::new(FailingProvider),
        &selection("claude-sonnet-5", 32_000, None),
        None,
    );

    match session.compact() {
        CompactOutcome::Failed(reason) => assert!(reason.contains("boom"), "{reason}"),
        other => panic!("expected Failed: {other:?}"),
    }
    assert_eq!(session.history(), &before[..], "history untouched on failure");
    assert_eq!(session.last_context_used(), Some(15));
}

#[test]
fn compact_empty_summary_is_fail_closed_but_usage_counts() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(
        dir.path(),
        vec![
            msg(vec![text("answer")], StopReason::EndTurn),
            // Whitespace-only summary: fail-closed.
            msg(vec![text("  \n ")], StopReason::EndTurn),
        ],
    );
    collect_events(&mut session, "question");
    let before = session.history().to_vec();

    match session.compact() {
        CompactOutcome::Failed(reason) => {
            assert!(reason.contains("empty summary"), "{reason}")
        }
        other => panic!("expected Failed: {other:?}"),
    }
    assert_eq!(session.history(), &before[..]);
    // The failed attempt was still real spend: 2 responses accumulated.
    assert_eq!(session.session_usage().input_tokens, Some(20));
}

#[test]
fn compact_on_empty_history_makes_no_call() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(dir.path(), vec![]);
    assert_eq!(session.compact(), CompactOutcome::Nothing);
    assert!(requests.borrow().is_empty(), "no provider call for an empty history");
}

#[test]
fn compact_cancelled_leaves_history_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(
        dir.path(),
        vec![
            msg(vec![text("answer")], StopReason::EndTurn),
            msg(vec![text("a summary that must not land")], StopReason::EndTurn),
        ],
    );
    collect_events(&mut session, "question");
    let before = session.history().to_vec();
    // Ctrl+C landed during the summary call (the mock returns a completed
    // message anyway, mirroring an Ok(partial) landing after a cancel).
    session.cancel_token().set();
    assert_eq!(session.compact(), CompactOutcome::Cancelled);
    assert_eq!(session.history(), &before[..]);
    session.cancel_token().clear();
}

#[test]
fn compact_command_reports_and_persists_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(
        dir.path(),
        vec![
            msg(vec![text("answer")], StopReason::EndTurn),
            msg(vec![text("Goal: persist test")], StopReason::EndTurn),
        ],
    );
    collect_events(&mut session, "question");

    let path = dir.path().join("session.json");
    let mut h = CmdHarness::new();
    h.persist = Some(path.clone());
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!("/compact builds no provider")
    };
    let events = commands::run(commands::parse("/compact"), &mut h.ctx(&mut session, &build));
    let ns = notices(&events);
    assert!(
        ns.iter().any(|n| n.contains("compacted: 2 message(s) summarized into 2")
            && n.contains("rebuilds the provider's cached prefix")),
        "compact notice: {ns:?}"
    );
    // The compacted state is on disk NOW, like /clear.
    let loaded = temur::session_store::load(&path).unwrap();
    assert_eq!(loaded.history.len(), 2);
    assert!(matches!(
        &loaded.history[0].content[0],
        ContentBlock::Text { text } if text.starts_with("[conversation summary (compacted)]")
    ));
    assert_eq!(loaded.last_context_used, None);
}

#[test]
fn compact_command_on_empty_history_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!()
    };
    let events = commands::run(commands::parse("/compact"), &mut h.ctx(&mut session, &build));
    assert!(
        notices(&events).iter().any(|n| n.contains("nothing to compact")),
        "{events:?}"
    );
}

#[test]
fn status_before_any_turn_renders_placeholders_and_no_key_material() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!()
    };
    let events = commands::run(commands::parse("/status"), &mut h.ctx(&mut session, &build));
    let ns = notices(&events);
    assert!(ns.iter().any(|n| n.contains("(none — base config)")), "{ns:?}");
    assert!(ns.iter().any(|n| n.contains("anthropic") && n.contains("claude-sonnet-5")));
    assert!(
        ns.iter().any(|n| n == "thinking: off · max_tokens: 32000 · prompt: full"),
        "T9 prompt field on the thinking line: {ns:?}"
    );
    assert!(ns.iter().any(|n| n.contains("no usage reported yet")));
    assert!(ns.iter().any(|n| n.contains("persistence disabled (--mock)")));
    assert!(
        !ns.iter().any(|n| n.contains("key")),
        "no key-related output at all: {ns:?}"
    );
}

// ------------------------------------------- T24: /status cost estimate

/// A session seeded with real token totals, the only state the cost
/// estimate reads. 1M input + 200k output, no cache fields reported.
fn session_with_usage(dir: &std::path::Path) -> Session {
    let (mut session, _) = session_with(dir, vec![]);
    let file = SessionFile {
        version: FORMAT_VERSION,
        provider: "anthropic".into(),
        model: "claude-sonnet-5".into(),
        cwd: "/work".into(),
        history: vec![],
        session_usage: Usage {
            input_tokens: Some(1_000_000),
            output_tokens: Some(200_000),
            ..Default::default()
        },
        todos: vec![],
        last_context_used: Some(1200),
        name: None,
    };
    let (seed, _) = store::prepare_seed(file);
    session.load_seed(seed);
    session
}

#[test]
fn status_shows_the_cost_estimate_when_keyed_priced_and_used() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = session_with_usage(dir.path());
    let mut h = CmdHarness::new();
    // Illustrative list-style rates, per million tokens: the arithmetic
    // is the subject here, not which tier they belong to.
    h.active_resolved.price_input_per_mtok = Some(3.0);
    h.active_resolved.price_output_per_mtok = Some(15.0);
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!()
    };
    let events = commands::run(commands::parse("/status"), &mut h.ctx(&mut session, &build));
    let ns = notices(&events);
    // 1M in at $3 + 200k out at $15 = $3.00 + $3.00.
    assert!(
        ns.iter()
            .any(|n| n == "cost: ~$6.00 this session (estimate, configured list rates)"),
        "{ns:?}"
    );
    // Between the context line and the session-file line.
    let cost = ns.iter().position(|n| n.starts_with("cost:")).unwrap();
    let context = ns.iter().position(|n| n.starts_with("context:")).unwrap();
    let file = ns.iter().position(|n| n.starts_with("session file:")).unwrap();
    assert!(context < cost && cost < file, "{ns:?}");
}

#[test]
fn status_cost_estimate_shows_four_decimals_below_a_cent() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let file = SessionFile {
        version: FORMAT_VERSION,
        provider: "anthropic".into(),
        model: "claude-sonnet-5".into(),
        cwd: "/work".into(),
        history: vec![],
        session_usage: Usage { input_tokens: Some(1_000), ..Default::default() },
        todos: vec![],
        last_context_used: Some(1_000),
        name: None,
    };
    let (seed, _) = store::prepare_seed(file);
    session.load_seed(seed);
    let mut h = CmdHarness::new();
    h.active_resolved.price_input_per_mtok = Some(3.0);
    h.active_resolved.price_output_per_mtok = Some(15.0);
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!()
    };
    let events = commands::run(commands::parse("/status"), &mut h.ctx(&mut session, &build));
    let ns = notices(&events);
    assert!(
        ns.iter()
            .any(|n| n == "cost: ~$0.0030 this session (estimate, configured list rates)"),
        "{ns:?}"
    );
}

#[test]
fn status_omits_the_cost_estimate_when_keyless_unpriced_or_unused() {
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!()
    };
    let has_cost = |ns: &[String]| ns.iter().any(|n| n.starts_with("cost:"));

    // Keyless openai-compat, fully priced and used: never billed, so no line.
    let dir = tempfile::tempdir().unwrap();
    let mut session = session_with_usage(dir.path());
    let mut h = CmdHarness::new();
    h.active_resolved.provider = "openai-compat".into();
    h.active_resolved.api_key_file = None;
    h.active_resolved.price_input_per_mtok = Some(3.0);
    h.active_resolved.price_output_per_mtok = Some(15.0);
    let events = commands::run(commands::parse("/status"), &mut h.ctx(&mut session, &build));
    assert!(!has_cost(&notices(&events)), "keyless: {:?}", notices(&events));

    // Keyed and used but unpriced: no nag, the docs point at the fields.
    let mut session = session_with_usage(dir.path());
    let mut h = CmdHarness::new();
    let events = commands::run(commands::parse("/status"), &mut h.ctx(&mut session, &build));
    assert!(!has_cost(&notices(&events)), "unpriced: {:?}", notices(&events));

    // Keyed and priced but nothing reported yet: nothing to estimate.
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    h.active_resolved.price_input_per_mtok = Some(3.0);
    h.active_resolved.price_output_per_mtok = Some(15.0);
    let events = commands::run(commands::parse("/status"), &mut h.ctx(&mut session, &build));
    assert!(!has_cost(&notices(&events)), "no usage: {:?}", notices(&events));
}

#[test]
fn thinking_set_under_compat_notes_anthropic_only() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    h.provider_name = "openai-compat".into();
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!()
    };
    let events = commands::run(
        commands::parse("/thinking on"),
        &mut h.ctx(&mut session, &build),
    );
    assert!(events.contains(&AgentEvent::ThinkingChanged(true)));
    assert!(notices(&events).iter().any(|n| n.contains("only used by the anthropic provider")));
    assert!(session.thinking());
    // Show reflects it.
    let events = commands::run(
        commands::parse("/thinking"),
        &mut h.ctx(&mut session, &build),
    );
    assert!(notices(&events).iter().any(|n| n == "thinking: on"));
}

#[test]
fn replay_mode_disables_mutating_commands_only() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    h.replay = true;
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!("mutating commands are disabled under replay")
    };
    for line in [
        "/model b",
        "/model raw-id-9",
        "/models",
        "/clear",
        "/compact",
        "/thinking on",
        "/sessions",
        "/resume alpha",
        "/new alpha",
    ] {
        let events = commands::run(commands::parse(line), &mut h.ctx(&mut session, &build));
        assert!(
            notices(&events).iter().any(|n| n.contains("unavailable in replay/capture mode")),
            "{line}: {events:?}"
        );
    }
    assert_eq!(session.model(), "claude-sonnet-5");
    assert!(!session.thinking());
    assert_eq!(h.active, None);
    // Read-only commands still work.
    for line in ["/help", "/status", "/model", "/thinking"] {
        let events = commands::run(commands::parse(line), &mut h.ctx(&mut session, &build));
        assert!(!notices(&events).is_empty(), "{line} still answers");
    }
}

// ------------------------------------------- T9: per-profile prompt profiles

/// Descriptions the registry serves for a profile, for request assertions.
fn descriptions(profile: PromptProfile) -> Vec<String> {
    Registry::standard()
        .with_profile(profile)
        .definitions()
        .into_iter()
        .map(|d| d.description)
        .collect()
}

fn req_descriptions(req: &ChatRequest) -> Vec<String> {
    req.tools.iter().map(|t| t.description.clone()).collect()
}

/// full → compact switch swaps the NEXT request's system string AND tool
/// descriptions; switching back restores both. Asserted on the recorded
/// `ChatRequest`s of mock providers installed by the switches themselves.
#[test]
fn switch_swaps_system_and_tool_descriptions_and_back() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    h.profiles.get_mut("b").unwrap().prompt_profile = PromptProfile::Compact;

    // Switch onto the compact profile "b".
    let requests_b = Rc::new(RefCell::new(vec![]));
    let rb = requests_b.clone();
    let build_b = move |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        Ok(Box::new(MockProvider {
            responses: RefCell::new(vec![msg(vec![text("from b")], StopReason::EndTurn)]),
            requests: rb.clone(),
        }))
    };
    commands::run(commands::parse("/model b"), &mut h.ctx(&mut session, &build_b));
    assert_eq!(h.prompt_profile, PromptProfile::Compact);
    collect_events(&mut session, "hello");
    let req = requests_b.borrow()[0].clone();
    assert_eq!(req.system.as_deref(), Some("compact test system"));
    assert_eq!(req_descriptions(&req), descriptions(PromptProfile::Compact));

    // Switch back to the full profile "a": both restored.
    let requests_a = Rc::new(RefCell::new(vec![]));
    let ra = requests_a.clone();
    let build_a = move |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        Ok(Box::new(MockProvider {
            responses: RefCell::new(vec![msg(vec![text("from a")], StopReason::EndTurn)]),
            requests: ra.clone(),
        }))
    };
    commands::run(commands::parse("/model a"), &mut h.ctx(&mut session, &build_a));
    assert_eq!(h.prompt_profile, PromptProfile::Full);
    collect_events(&mut session, "again");
    let req = requests_a.borrow()[0].clone();
    assert_eq!(req.system.as_deref(), Some("full test system"));
    assert_eq!(req_descriptions(&req), descriptions(PromptProfile::Full));

    // /status reflects the live profile.
    let build_none = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!("/status builds nothing")
    };
    let events = commands::run(commands::parse("/status"), &mut h.ctx(&mut session, &build_none));
    assert!(notices(&events).iter().any(|n| n.ends_with("prompt: full")), "{events:?}");
}

/// T41: a `/model` switch onto a profile whose SMALL window auto-selected
/// compact says so, in the same words startup uses, and `/status` marks it
/// `(auto)`. A switch onto a profile that auto-selected full says nothing.
#[test]
fn a_switch_onto_an_auto_compact_profile_explains_itself() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    {
        let b = h.profiles.get_mut("b").unwrap();
        b.context_window = Some(12288);
        b.prompt_profile = PromptProfile::Compact;
        b.prompt_profile_source = temur::config::PromptProfileSource::Auto;
    }
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        Ok(Box::new(MockProvider {
            responses: RefCell::new(vec![]),
            requests: Rc::new(RefCell::new(vec![])),
        }))
    };
    let events = commands::run(commands::parse("/model b"), &mut h.ctx(&mut session, &build));
    assert!(
        notices(&events)
            .iter()
            .any(|n| n == &temur::config::auto_compact_notice(12288)),
        "the switch prints the startup line verbatim: {events:?}"
    );
    let events = commands::run(commands::parse("/status"), &mut h.ctx(&mut session, &build));
    assert!(
        notices(&events).iter().any(|n| n.ends_with("prompt: compact (auto)")),
        "{events:?}"
    );

    // Profile "a" resolves full: nothing to say, either way.
    let events = commands::run(commands::parse("/model a"), &mut h.ctx(&mut session, &build));
    assert!(
        !notices(&events).iter().any(|n| n.contains("prompt profile:")),
        "auto-full is silent: {events:?}"
    );
}

/// The config system_prompt override rule, as main's rebuild_system closure
/// implements it: the SAME string comes back for either profile, so a
/// prompt-profile switch changes nothing about the system string (tool
/// descriptions still swap — that contract is independent).
#[test]
fn system_prompt_override_wins_in_both_profiles_across_switches() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    h.profiles.get_mut("b").unwrap().prompt_profile = PromptProfile::Compact;
    h.rebuild = Box::new(|_| "override system".into());
    // Startup under the override (what main would have assembled).
    session.set_prompt("override system".into(), PromptProfile::Full);

    let requests = Rc::new(RefCell::new(vec![]));
    for (cmd, expected_descs) in [
        ("/model b", descriptions(PromptProfile::Compact)),
        ("/model a", descriptions(PromptProfile::Full)),
    ] {
        let r = requests.clone();
        let build = move |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
            Ok(Box::new(MockProvider {
                responses: RefCell::new(vec![msg(vec![text("ok")], StopReason::EndTurn)]),
                requests: r.clone(),
            }))
        };
        commands::run(commands::parse(cmd), &mut h.ctx(&mut session, &build));
        collect_events(&mut session, "turn");
        let req = requests.borrow().last().unwrap().clone();
        assert_eq!(
            req.system.as_deref(),
            Some("override system"),
            "{cmd}: override wins in both profiles"
        );
        assert_eq!(req_descriptions(&req), expected_descs, "{cmd}: descriptions still swap");
    }
}

/// Extended atomicity (T9): a FAILED switch whose target has a different
/// prompt_profile leaves the system string and the registry untouched too.
#[test]
fn failed_switch_leaves_system_and_registry_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![msg(vec![text("still here")], StopReason::EndTurn)],
    );
    let mut h = CmdHarness::new();
    {
        let b = h.profiles.get_mut("b").unwrap();
        b.prompt_profile = PromptProfile::Compact;
        b.api_key_file = Some("/nonexistent/temur-test/keyfile".into());
    }
    let build = |p: &ResolvedProfile| temur::provider::build_live(p);
    let events = commands::run(commands::parse("/model b"), &mut h.ctx(&mut session, &build));
    assert!(
        notices(&events).iter().any(|n| n.contains("failed") && n.contains("session unchanged")),
        "{events:?}"
    );
    assert_eq!(h.prompt_profile, PromptProfile::Full, "loop-local profile unchanged");

    // The next request proves session internals: original system string and
    // FULL descriptions, through the still-installed mock provider.
    collect_events(&mut session, "probe");
    let req = requests.borrow()[0].clone();
    assert_eq!(req.system.as_deref(), Some("test system"));
    assert_eq!(req_descriptions(&req), descriptions(PromptProfile::Full));
}

// ------------------------------------------ T9: /models + raw-id switching

#[test]
fn models_list_renders_ids_empty_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!("/models builds no provider")
    };

    // Non-empty listing → the typed event, verbatim ids. The listing fn
    // sees the ACTIVE resolved selection.
    let mut h = CmdHarness::new();
    h.list = Box::new(|p| {
        assert_eq!(p.model, "claude-sonnet-5", "listing sees the active selection");
        Ok(entries(&["m-1", "m-2"]))
    });
    let events = commands::run(commands::parse("/models"), &mut h.ctx(&mut session, &build));
    assert_eq!(
        events,
        vec![AgentEvent::ModelsListed(vec!["m-1".into(), "m-2".into()])]
    );
    // T22: the listing refreshed the cache, windows included (none here).
    assert_eq!(h.cached_models, entries(&["m-1", "m-2"]));

    // Empty listing → a notice, not an empty listing event.
    let mut h = CmdHarness::new();
    h.list = Box::new(|_| Ok(vec![]));
    let events = commands::run(commands::parse("/models"), &mut h.ctx(&mut session, &build));
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::ModelsListed(_))));
    assert!(notices(&events).iter().any(|n| n.contains("no models")), "{events:?}");

    // Network/HTTP error → error notice carrying the message; no key
    // material anywhere in any /models output by construction (ids only).
    let mut h = CmdHarness::new();
    h.list = Box::new(|_| {
        Err(temur::error::Error::Models(
            "model listing GET https://x.test/v1/models: HTTP 500".into(),
        ))
    });
    let events = commands::run(commands::parse("/models"), &mut h.ctx(&mut session, &build));
    assert!(
        notices(&events).iter().any(|n| n.starts_with("/models failed:") && n.contains("HTTP 500")),
        "{events:?}"
    );
}

// -------------------------------- T22: /models context enrichment notices

/// One listing entry with a reported window.
fn wentry(id: &str, window: u64) -> temur::provider::ModelEntry {
    temur::provider::ModelEntry { id: id.to_string(), context_window: Some(window) }
}

#[test]
fn models_hints_the_exact_config_line_when_context_window_is_unset() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!("/models builds no provider")
    };
    // base_resolved: anthropic, claude-sonnet-5, context_window None.
    let mut h = CmdHarness::new();
    h.list = Box::new(|_| {
        Ok(vec![wentry("claude-sonnet-5", 200_000), wentry("claude-opus-5", 200_000)])
    });
    let events = commands::run(commands::parse("/models"), &mut h.ctx(&mut session, &build));
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::ModelsListed(_))),
        "{events:?}"
    );
    let ns = notices(&events);
    assert_eq!(
        ns,
        vec![
            "hint: the API reports max_input_tokens 200000 for claude-sonnet-5; add \"context_window\": 200000 to the profile to enable the context advisory"
        ],
        "{events:?}"
    );
    // The cache carries the windows: no second request would be needed.
    assert_eq!(h.cached_models[0], wentry("claude-sonnet-5", 200_000));
}

#[test]
fn models_warns_when_configured_context_window_exceeds_the_reported_one() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!("/models builds no provider")
    };
    let mut h = CmdHarness::new();
    h.active_resolved.context_window = Some(300_000);
    h.list = Box::new(|_| Ok(vec![wentry("claude-sonnet-5", 200_000)]));
    let events = commands::run(commands::parse("/models"), &mut h.ctx(&mut session, &build));
    let ns = notices(&events);
    assert_eq!(ns.len(), 1, "{events:?}");
    assert!(
        ns[0].starts_with("warning: configured context_window 300000 is larger than the max_input_tokens 200000"),
        "{ns:?}"
    );
    assert!(ns[0].contains("claude-sonnet-5"), "{ns:?}");
    assert!(
        ns[0].contains("fires too late") && ns[0].contains("requests can fail"),
        "{ns:?}"
    );
}

#[test]
fn models_stays_silent_when_equal_unknown_or_not_anthropic() {
    let dir = tempfile::tempdir().unwrap();
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!("/models builds no provider")
    };
    let quiet = |h: &mut CmdHarness| {
        let dir2 = tempfile::tempdir().unwrap();
        let (mut session, _) = session_with(dir2.path(), vec![]);
        let events = commands::run(commands::parse("/models"), &mut h.ctx(&mut session, &build));
        assert!(
            notices(&events).is_empty(),
            "no context notice expected: {events:?}"
        );
        drop(dir2);
    };
    let _ = &dir;
    // Equal: silence.
    let mut h = CmdHarness::new();
    h.active_resolved.context_window = Some(200_000);
    h.list = Box::new(|_| Ok(vec![wentry("claude-sonnet-5", 200_000)]));
    quiet(&mut h);
    // Window unknown on the wire (0 or absent parses to None): silence
    // even with context_window unset.
    let mut h = CmdHarness::new();
    h.list = Box::new(|_| Ok(entries(&["claude-sonnet-5"])));
    quiet(&mut h);
    // Active model not in the listing, and no dated alias of it either:
    // silence.
    let mut h = CmdHarness::new();
    h.list = Box::new(|_| Ok(vec![wentry("some-other-model", 200_000)]));
    quiet(&mut h);
    // A dated entry for a DIFFERENT model must not be read as this one's.
    let mut h = CmdHarness::new();
    h.list = Box::new(|_| Ok(vec![wentry("claude-opus-5-20251001", 200_000)]));
    quiet(&mut h);
    // Suffix present but not eight digits: not a dated id.
    let mut h = CmdHarness::new();
    h.list = Box::new(|_| Ok(vec![wentry("claude-sonnet-5-preview", 200_000)]));
    quiet(&mut h);
    // Dated entries that DISAGREE about the window: the inference is not
    // unambiguous, so it is not made.
    let mut h = CmdHarness::new();
    h.model = "claude-haiku-4-5".into();
    h.active_resolved.model = "claude-haiku-4-5".into();
    h.list = Box::new(|_| {
        Ok(vec![
            wentry("claude-haiku-4-5-20251001", 200_000),
            wentry("claude-haiku-4-5-20260210", 400_000),
        ])
    });
    quiet(&mut h);
    // Not the anthropic provider: silence even if a proxy reports windows.
    let mut h = CmdHarness::new();
    h.active = Some("b".into());
    h.provider_name = "openai-compat".into();
    h.model = "model-b".into();
    h.active_resolved = h.profiles["b"].clone(); // context_window 4096
    h.list = Box::new(|_| Ok(vec![wentry("model-b", 1_000)]));
    quiet(&mut h);
}

/// T13: under-configuring is safe, but silence about it was not helpful.
#[test]
fn models_hints_when_the_configured_window_is_under_the_reported_one() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!("/models builds no provider")
    };
    let mut h = CmdHarness::new();
    h.active_resolved.context_window = Some(100_000);
    h.list = Box::new(|_| Ok(vec![wentry("claude-sonnet-5", 200_000)]));
    let events = commands::run(commands::parse("/models"), &mut h.ctx(&mut session, &build));
    let ns = notices(&events);
    assert_eq!(ns.len(), 1, "{events:?}");
    assert_eq!(
        ns[0],
        "hint: configured context_window 100000 is smaller than the max_input_tokens 200000 the API reports for claude-sonnet-5: safe, but the context advisory fires earlier than it needs to; raise context_window to 200000 to use the whole window",
        "{ns:?}"
    );
}

/// T13: the bare `claude-haiku-4-5` alias is absent from /v1/models, which
/// lists dated ids only, so /models could never judge a haiku profile. It
/// judges it through the dated entry now, and says so.
#[test]
fn models_judges_a_bare_alias_through_its_dated_listing_entry() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!("/models builds no provider")
    };
    // Unset window: the hint names the dated entry and the alias it came
    // from, and still spells the exact config line.
    let mut h = CmdHarness::new();
    h.model = "claude-haiku-4-5".into();
    h.active_resolved.model = "claude-haiku-4-5".into();
    h.list = Box::new(|_| {
        Ok(vec![
            wentry("claude-sonnet-5", 1_000_000),
            wentry("claude-haiku-4-5-20251001", 200_000),
        ])
    });
    let events = commands::run(commands::parse("/models"), &mut h.ctx(&mut session, &build));
    let ns = notices(&events);
    assert_eq!(
        ns,
        vec![
            "hint: the API reports max_input_tokens 200000 for claude-haiku-4-5-20251001 (matched from claude-haiku-4-5); add \"context_window\": 200000 to the profile to enable the context advisory"
        ],
        "{events:?}"
    );

    // Several dated entries that AGREE: still unambiguous, and the newest
    // date is the one named.
    let mut h = CmdHarness::new();
    h.model = "claude-haiku-4-5".into();
    h.active_resolved.model = "claude-haiku-4-5".into();
    h.active_resolved.context_window = Some(500_000);
    h.list = Box::new(|_| {
        Ok(vec![
            wentry("claude-haiku-4-5-20260210", 200_000),
            wentry("claude-haiku-4-5-20251001", 200_000),
        ])
    });
    let events = commands::run(commands::parse("/models"), &mut h.ctx(&mut session, &build));
    let ns = notices(&events);
    assert_eq!(ns.len(), 1, "{events:?}");
    assert!(
        ns[0].starts_with("warning: configured context_window 500000 is larger than the max_input_tokens 200000")
            && ns[0].contains("for claude-haiku-4-5-20260210 (matched from claude-haiku-4-5)"),
        "{ns:?}"
    );
}

#[test]
fn raw_id_switch_keeps_profile_settings_and_the_save_records_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();

    // Start ON profile "b" (as a prior /model b would have left things).
    h.active = Some("b".into());
    h.provider_name = "openai-compat".into();
    h.model = "model-b".into();
    h.active_resolved = h.profiles["b"].clone();

    let requests = Rc::new(RefCell::new(vec![]));
    let r = requests.clone();
    let build = move |p: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        // The target is the active selection with ONLY the model replaced.
        assert_eq!(p.model, "raw-model-x");
        assert_eq!(p.provider, "openai-compat");
        assert_eq!(p.base_url, "http://b.test/v1");
        assert_eq!(p.max_tokens, 222);
        Ok(Box::new(MockProvider {
            responses: RefCell::new(vec![msg(vec![text("hi")], StopReason::EndTurn)]),
            requests: r.clone(),
        }))
    };
    let events = commands::run(
        commands::parse("/model raw-model-x"),
        &mut h.ctx(&mut session, &build),
    );
    assert!(events.contains(&AgentEvent::ModelSwitched { model: "raw-model-x".into(), provider: "openai-compat".into() }));
    assert!(
        notices(&events)
            .iter()
            .any(|n| n == "switched model to raw-model-x (openai-compat · profile settings kept)"),
        "{events:?}"
    );
    // Profile NAME kept; model bookkeeping updated; limits stay b's.
    assert_eq!(h.active.as_deref(), Some("b"));
    assert_eq!(h.model, "raw-model-x");
    assert_eq!(h.active_resolved.model, "raw-model-x");
    assert_eq!(session.model(), "raw-model-x");
    assert_eq!(session.max_tokens(), 222);
    assert_eq!(session.context_window(), Some(4_096));
    assert_eq!(h.prompt_profile, PromptProfile::Full, "raw-id switch never touches the prompt profile");

    // /status: profile line unchanged, model line new.
    let build_none = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!()
    };
    let events = commands::run(commands::parse("/status"), &mut h.ctx(&mut session, &build_none));
    let ns = notices(&events);
    assert!(ns.iter().any(|n| n == "profile: b"), "{ns:?}");
    assert!(ns.iter().any(|n| n == "provider: openai-compat · model: raw-model-x"), "{ns:?}");

    // The next request goes out under the raw id…
    collect_events(&mut session, "hello");
    assert_eq!(requests.borrow()[0].model, "raw-model-x");
    assert_eq!(requests.borrow()[0].max_tokens, 222);

    // …and the driver-loop save (mirrored here field-for-field) records it.
    let path = dir.path().join("session.json");
    let snap = session.snapshot();
    let file = temur::session_store::SessionFileRef {
        version: temur::session_store::FORMAT_VERSION,
        provider: &h.provider_name,
        model: &h.model,
        cwd: &h.cwd_display,
        history: snap.history,
        session_usage: snap.session_usage,
        todos: snap.todos,
        last_context_used: snap.last_context_used,
        name: None,
    };
    temur::session_store::save(&path, &file, temur::config::DEFAULT_SESSION_MAX_BYTES, &mut |_| {})
        .unwrap();
    assert_eq!(temur::session_store::load(&path).unwrap().model, "raw-model-x");
}

#[test]
fn raw_id_switch_with_unreadable_key_file_is_atomic() {
    // Anthropic active selection whose key file cannot be read: the raw-id
    // switch must fail through the REAL build path and change nothing.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    h.active_resolved.api_key_file = Some("/nonexistent/temur-test/keyfile".into());
    let build = |p: &ResolvedProfile| temur::provider::build_live(p);
    let events = commands::run(
        commands::parse("/model raw-model-x"),
        &mut h.ctx(&mut session, &build),
    );
    assert!(
        notices(&events)
            .iter()
            .any(|n| n.contains("switch to model \"raw-model-x\" failed") && n.contains("session unchanged")),
        "{events:?}"
    );
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::ModelSwitched { .. })));
    assert_eq!(h.model, "claude-sonnet-5");
    assert_eq!(h.active_resolved.model, "claude-sonnet-5");
    assert_eq!(session.model(), "claude-sonnet-5");
}

// ------------------------------------------------- T15: /model --save

/// A temp config file + the harness pointed at it.
fn harness_with_config(h: &mut CmdHarness, dir: &std::path::Path, json: &str) {
    let path = dir.join("config.json");
    std::fs::write(&path, json).unwrap();
    h.config_path = path;
}

#[test]
fn model_switch_save_switches_then_persists_to_the_profile_site() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    h.active = Some("b".into());
    h.provider_name = "openai-compat".into();
    h.model = "model-b".into();
    h.active_resolved = h.profiles["b"].clone();
    harness_with_config(
        &mut h,
        dir.path(),
        r#"{"profiles":{"b":{"provider":"openai-compat","model":"model-b","keep_me":1}},"future_field":true}"#,
    );

    let build = |p: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        assert_eq!(p.model, "raw-x");
        Ok(Box::new(MockProvider {
            responses: RefCell::new(vec![]),
            requests: Rc::new(RefCell::new(vec![])),
        }))
    };
    let events = commands::run(
        commands::parse("/model raw-x --save"),
        &mut h.ctx(&mut session, &build),
    );
    assert!(events.contains(&AgentEvent::ModelSwitched { model: "raw-x".into(), provider: "openai-compat".into() }));
    assert!(
        notices(&events).iter().any(|n| n.starts_with("saved model raw-x to ")),
        "{events:?}"
    );
    let saved = std::fs::read_to_string(&h.config_path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(v["profiles"]["b"]["model"], "raw-x");
    assert_eq!(v["profiles"]["b"]["keep_me"], 1, "unknown fields survive: {saved}");
    assert_eq!(v["future_field"], true, "{saved}");
}

#[test]
fn model_save_current_persists_without_switching() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    harness_with_config(&mut h, dir.path(), r#"{"model":"old-model"}"#);
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!("/model --save switches nothing")
    };
    let events = commands::run(
        commands::parse("/model --save"),
        &mut h.ctx(&mut session, &build),
    );
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::ModelSwitched { .. })));
    assert!(
        notices(&events).iter().any(|n| n.starts_with("saved model claude-sonnet-5 to ")),
        "{events:?}"
    );
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&h.config_path).unwrap()).unwrap();
    assert_eq!(v["model"], "claude-sonnet-5", "anthropic base site is the top-level key");
}

#[test]
fn model_save_with_a_profile_name_is_a_clean_error_and_switches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!("a profile name with --save must not build")
    };
    let events = commands::run(
        commands::parse("/model b --save"),
        &mut h.ctx(&mut session, &build),
    );
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::ModelSwitched { .. })));
    assert!(
        notices(&events)
            .iter()
            .any(|n| n.contains("\"b\" is a profile") && n.contains("\"profile\" key")),
        "{events:?}"
    );
    assert_eq!(h.model, "claude-sonnet-5", "no switch happened");
}

#[test]
fn model_switch_save_persist_failure_keeps_the_switch() {
    // config_path points nowhere: the switch succeeds, persistence fails,
    // and the notice says both.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        Ok(Box::new(MockProvider {
            responses: RefCell::new(vec![]),
            requests: Rc::new(RefCell::new(vec![])),
        }))
    };
    let events = commands::run(
        commands::parse("/model raw-x --save"),
        &mut h.ctx(&mut session, &build),
    );
    assert!(events.contains(&AgentEvent::ModelSwitched { model: "raw-x".into(), provider: "anthropic".into() }));
    assert!(
        notices(&events)
            .iter()
            .any(|n| n.contains("NOT saved") && n.contains("no config file")),
        "{events:?}"
    );
    assert_eq!(h.model, "raw-x", "the switch stands");
    assert_eq!(session.model(), "raw-x");
}

#[test]
fn model_switch_save_failed_switch_never_touches_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    harness_with_config(&mut h, dir.path(), r#"{"model":"keep"}"#);
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        Err(temur::error::Error::Secret("cannot read key".into()))
    };
    let events = commands::run(
        commands::parse("/model raw-x --save"),
        &mut h.ctx(&mut session, &build),
    );
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::ModelSwitched { .. })));
    assert!(
        notices(&events).iter().any(|n| n.contains("failed") && n.contains("session unchanged")),
        "{events:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&h.config_path).unwrap(),
        r#"{"model":"keep"}"#,
        "a failed switch persists nothing"
    );
}

#[test]
fn model_save_forms_are_replay_guarded() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    h.replay = true;
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!("replay mode builds nothing")
    };
    for line in ["/model --save", "/model raw-x --save"] {
        let events = commands::run(commands::parse(line), &mut h.ctx(&mut session, &build));
        assert!(
            notices(&events).iter().any(|n| n.contains("unavailable in replay/capture mode")),
            "{line}: {events:?}"
        );
    }
}

// ---------------------------------- T16: /model hints + cached-id advisory

/// A provider builder for switches that must succeed; nothing is asserted
/// about the requests it records.
fn build_ok(_: &ResolvedProfile) -> Result<Box<dyn Provider>, temur::error::Error> {
    Ok(Box::new(MockProvider {
        responses: RefCell::new(vec![]),
        requests: Rc::new(RefCell::new(vec![])),
    }))
}

#[test]
fn model_list_appends_the_raw_id_hints_after_the_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!("/model with no argument builds nothing")
    };
    let events = commands::run(commands::parse("/model"), &mut h.ctx(&mut session, &build));
    let ns = notices(&events);
    // Profiles first, hints last, in order.
    assert!(ns[0].starts_with("a — "), "{ns:?}");
    assert!(ns[1].starts_with("b — "), "{ns:?}");
    assert_eq!(
        &ns[2..],
        [
            "/model <name> switches profiles; any other argument is a raw model id on the ACTIVE provider",
            "/models lists what the active provider serves; /model <id> --save persists the switch",
        ],
        "{ns:?}"
    );
}

#[test]
fn raw_switch_advises_when_the_id_is_absent_from_the_cached_listing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    h.cached_models = entries(&["served-a", "served-b"]);
    let events = commands::run(
        commands::parse("/model bogus-id"),
        &mut h.ctx(&mut session, &build_ok),
    );
    // The switch STANDS — the advisory follows the confirmation notice.
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::ModelSwitched { .. })));
    assert_eq!(h.model, "bogus-id");
    let ns = notices(&events);
    assert!(ns[0].starts_with("switched model to bogus-id"), "{ns:?}");
    assert_eq!(
        ns[1],
        "note: \"bogus-id\" is not in the last /models listing; the switch stands — a wrong id surfaces as the provider's error on the next turn",
        "{ns:?}"
    );
}

#[test]
fn raw_switch_stays_silent_when_the_id_is_listed_or_no_listing_exists() {
    // Cached listing contains the id: no advisory.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    h.cached_models = entries(&["served-a"]);
    let events = commands::run(
        commands::parse("/model served-a"),
        &mut h.ctx(&mut session, &build_ok),
    );
    assert!(
        !notices(&events).iter().any(|n| n.contains("/models listing")),
        "{events:?}"
    );
    // Empty cache (no listing yet): also silent — nothing to judge against.
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    let events = commands::run(
        commands::parse("/model bogus-id"),
        &mut h.ctx(&mut session, &build_ok),
    );
    assert!(
        !notices(&events).iter().any(|n| n.contains("/models listing")),
        "{events:?}"
    );
    assert_eq!(h.model, "bogus-id", "the switch still happened");
}

// ------------------------------------------- T16: cross-provider hop

/// A harness sitting on the openai-compat profile "b", with two anthropic
/// profiles alongside ("a": model-a, "opus": claude-opus-5) — the crafted
/// map every hop rule is exercised over. Name order: a < b < opus.
fn hop_harness() -> CmdHarness {
    let mut h = CmdHarness::new();
    h.profiles.insert(
        "opus".to_string(),
        ResolvedProfile {
            provider: "anthropic".into(),
            model: "claude-opus-5".into(),
            base_url: "https://a.test".into(),
            api_key_file: None,
            max_tokens: 333,
            context_window: None,
            prompt_profile: PromptProfile::Compact,
            prompt_profile_source: Default::default(),
            price_input_per_mtok: None,
            price_output_per_mtok: None,
            max_tokens_parameter: Default::default(),
        },
    );
    h.active = Some("b".into());
    h.provider_name = "openai-compat".into();
    h.model = "model-b".into();
    h.active_resolved = h.profiles["b"].clone();
    h
}

#[test]
fn hop_rule0_cached_listing_wins_no_hop_no_warning() {
    // A proxy legitimately serving claude-* ids over openai-compat: the id
    // is in the cached listing, so the switch is literal and silent.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = hop_harness();
    h.cached_models = entries(&["claude-opus-5"]);
    let events = commands::run(
        commands::parse("/model claude-opus-5"),
        &mut h.ctx(&mut session, &build_ok),
    );
    assert_eq!(h.active.as_deref(), Some("b"), "no hop: profile b kept");
    assert_eq!(h.provider_name, "openai-compat");
    assert_eq!(h.model, "claude-opus-5");
    let ns = notices(&events);
    assert_eq!(
        ns,
        vec!["switched model to claude-opus-5 (openai-compat · profile settings kept)"],
        "no hop notice, no advisory: {ns:?}"
    );
}

#[test]
fn hop_rule1_exact_model_match_activates_that_profile() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = hop_harness();
    let events = commands::run(
        commands::parse("/model claude-opus-5"),
        &mut h.ctx(&mut session, &build_ok),
    );
    // FULL activation of "opus": profile name, provider, limits, prompt
    // profile all switch — not just the model string.
    assert_eq!(h.active.as_deref(), Some("opus"));
    assert_eq!(h.provider_name, "anthropic");
    assert_eq!(h.model, "claude-opus-5");
    assert_eq!(h.active_resolved, h.profiles["opus"]);
    assert_eq!(h.prompt_profile, PromptProfile::Compact, "prompt swap ran");
    assert_eq!(session.max_tokens(), 333);
    assert!(events.contains(&AgentEvent::ModelSwitched {
        model: "claude-opus-5".into(),
        provider: "anthropic".into(),
    }));
    assert_eq!(
        notices(&events),
        vec!["\"claude-opus-5\" is an anthropic model - switched to profile \"opus\" (anthropic, claude-opus-5)"],
    );
}

#[test]
fn hop_rule1_inexact_takes_first_anthropic_profile_then_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = hop_harness();
    let events = commands::run(
        commands::parse("/model claude-opus-4-8"),
        &mut h.ctx(&mut session, &build_ok),
    );
    // No profile's model matches: first anthropic profile in NAME order is
    // "a" (a < opus), activated in full, then the raw override on top.
    assert_eq!(h.active.as_deref(), Some("a"));
    assert_eq!(h.provider_name, "anthropic");
    assert_eq!(h.model, "claude-opus-4-8");
    assert_eq!(h.active_resolved.max_tokens, h.profiles["a"].max_tokens);
    assert_eq!(h.active_resolved.model, "claude-opus-4-8");
    assert!(events.contains(&AgentEvent::ModelSwitched {
        model: "claude-opus-4-8".into(),
        provider: "anthropic".into(),
    }));
    assert_eq!(
        notices(&events),
        vec!["\"claude-opus-4-8\" looks anthropic - hopped to profile \"a\" (its key file and limits apply), model claude-opus-4-8"],
    );
}

#[test]
fn hop_rule1_exact_beats_name_order() {
    // "opus" matches exactly and must win although "a" precedes it.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = hop_harness();
    commands::run(
        commands::parse("/model claude-opus-5"),
        &mut h.ctx(&mut session, &build_ok),
    );
    assert_eq!(h.active.as_deref(), Some("opus"), "exact match wins over name order");
}

#[test]
fn hop_rule2_no_anthropic_profile_switches_locally_with_a_hint() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = hop_harness();
    h.profiles.retain(|_, p| p.provider != "anthropic");
    let events = commands::run(
        commands::parse("/model claude-opus-5"),
        &mut h.ctx(&mut session, &build_ok),
    );
    // Today's behavior: the raw switch on the ACTIVE provider stands…
    assert_eq!(h.active.as_deref(), Some("b"));
    assert_eq!(h.provider_name, "openai-compat");
    assert_eq!(h.model, "claude-opus-5");
    // …plus the hint naming the enabling config.
    let ns = notices(&events);
    assert_eq!(ns[0], "switched model to claude-opus-5 (openai-compat · profile settings kept)");
    assert_eq!(
        ns[1],
        "note: \"claude-opus-5\" looks anthropic and was set on the ACTIVE provider (openai-compat); an anthropic profile in config.json enables the hop (temur init --add anthropic sets one up)",
    );
}

#[test]
fn hop_rule3_non_claude_id_and_anthropic_active_stay_plain() {
    // A non-claude id on openai-compat: plain switch (advisory covered by
    // its own tests).
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = hop_harness();
    let events = commands::run(
        commands::parse("/model qwen3-4b"),
        &mut h.ctx(&mut session, &build_ok),
    );
    assert_eq!(h.active.as_deref(), Some("b"));
    assert_eq!(h.model, "qwen3-4b");
    assert!(!notices(&events).iter().any(|n| n.contains("anthropic")), "{events:?}");
    // A claude id while the ACTIVE provider IS anthropic: no hop either.
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new(); // base config: anthropic
    let events = commands::run(
        commands::parse("/model claude-opus-4-8"),
        &mut h.ctx(&mut session, &build_ok),
    );
    assert_eq!(h.active, None, "no profile activated");
    assert_eq!(h.model, "claude-opus-4-8");
    assert_eq!(
        notices(&events),
        vec!["switched model to claude-opus-4-8 (anthropic · profile settings kept)"],
    );
}

#[test]
fn hop_failed_activation_is_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = hop_harness();
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        Err(temur::error::Error::Secret("cannot read key".into()))
    };
    let events = commands::run(
        commands::parse("/model claude-opus-5"),
        &mut h.ctx(&mut session, &build),
    );
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::ModelSwitched { .. })));
    assert!(
        notices(&events).iter().any(|n| n.contains("failed") && n.contains("session unchanged")),
        "{events:?}"
    );
    assert_eq!(h.active.as_deref(), Some("b"), "hop failure changes nothing");
    assert_eq!(h.provider_name, "openai-compat");
    assert_eq!(h.model, "model-b");
}

/// T41 (fixed in v0.30.1): the override failed, but the activation
/// STANDS, and an activation swaps the prompt profile. A user left on the
/// compact descriptions by a switch that only half-succeeded is exactly
/// who the auto line exists for, so it survives this path too.
#[test]
fn a_failed_override_after_a_hop_still_explains_the_auto_compact_profile() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = hop_harness();
    {
        let a = h.profiles.get_mut("a").unwrap();
        a.context_window = Some(12288);
        a.prompt_profile = PromptProfile::Compact;
        a.prompt_profile_source = temur::config::PromptProfileSource::Auto;
    }
    // Activating "a" (model-a) succeeds; only the override onto the
    // requested id fails, which is the partial state under test.
    let build = |p: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        if p.model == "claude-opus-4-8" {
            return Err(temur::error::Error::Secret("no route to that model".into()));
        }
        Ok(Box::new(MockProvider {
            responses: RefCell::new(vec![]),
            requests: Rc::new(RefCell::new(vec![])),
        }))
    };
    let events = commands::run(
        commands::parse("/model claude-opus-4-8"),
        &mut h.ctx(&mut session, &build),
    );
    let ns = notices(&events);
    assert!(
        ns.iter().any(|n| n.contains("the model override to")),
        "the failure is still reported: {ns:?}"
    );
    assert!(
        ns.iter().any(|n| n == &temur::config::auto_compact_notice(12288)),
        "and the profile it left the session on is explained: {ns:?}"
    );
    assert_eq!(h.active.as_deref(), Some("a"), "the activation stands");
    assert_eq!(h.prompt_profile, PromptProfile::Compact, "and it swapped the prompts");
}

#[test]
fn hop_then_save_persists_to_the_hop_profiles_site() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = hop_harness();
    harness_with_config(
        &mut h,
        dir.path(),
        r#"{"profiles":{"a":{"provider":"anthropic","model":"model-a"},"b":{"provider":"openai-compat","model":"model-b"},"opus":{"provider":"anthropic","model":"claude-opus-5"}},"profile":"b"}"#,
    );
    let events = commands::run(
        commands::parse("/model claude-opus-4-8 --save"),
        &mut h.ctx(&mut session, &build_ok),
    );
    // Hop to "a" (first anthropic profile), override, THEN persist — the
    // save site is the hop profile, and the notice names it.
    assert_eq!(h.active.as_deref(), Some("a"));
    assert!(
        notices(&events).iter().any(|n| n.starts_with("saved model claude-opus-4-8 to profile \"a\" in ")),
        "{events:?}"
    );
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&h.config_path).unwrap()).unwrap();
    assert_eq!(saved["profiles"]["a"]["model"], "claude-opus-4-8");
    assert_eq!(saved["profiles"]["b"]["model"], "model-b", "other profiles untouched");
    assert_eq!(saved["profile"], "b", "startup profile stays a hand edit");
}

// --------------------------------------------- T10: /sessions /resume /new

use temur::session_store::ReplayItem;

/// Write a session file the way the driver loop would, field for field.
fn write_session(
    dir: &std::path::Path,
    file_name: &str,
    cwd: &str,
    name: Option<&str>,
    history: Vec<RequestMessage>,
) {
    let f = SessionFile {
        version: FORMAT_VERSION,
        provider: "anthropic".into(),
        model: "claude-sonnet-5".into(),
        cwd: cwd.into(),
        history,
        session_usage: Usage {
            input_tokens: Some(70),
            output_tokens: Some(30),
            ..Default::default()
        },
        todos: vec![],
        last_context_used: None,
        name: name.map(String::from),
    };
    let r = store::SessionFileRef {
        version: f.version,
        provider: &f.provider,
        model: &f.model,
        cwd: &f.cwd,
        history: &f.history,
        session_usage: f.session_usage,
        todos: &f.todos,
        last_context_used: f.last_context_used,
        name: f.name.as_deref(),
    };
    store::save(
        &dir.join(file_name),
        &r,
        temur::config::DEFAULT_SESSION_MAX_BYTES,
        &mut |_| {},
    )
    .unwrap();
}

/// A no-build closure for commands that must not construct providers.
fn no_build(_: &ResolvedProfile) -> Result<Box<dyn Provider>, temur::error::Error> {
    unreachable!("session commands never build a provider")
}

#[test]
fn sessions_listing_marks_the_active_file_and_caches_keys() {
    let dir = tempfile::tempdir().unwrap();
    let sdir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    h.sessions_dir = sdir.path().to_path_buf();
    h.persist = Some(sdir.path().join("test-1111.json"));

    write_session(
        sdir.path(),
        "test-1111.json",
        "/test",
        None,
        vec![user_msg("current default work")],
    );
    write_session(
        sdir.path(),
        "other-2222-alpha.json",
        "/other",
        Some("alpha"),
        vec![user_msg("alpha work elsewhere")],
    );

    let events = commands::run(commands::parse("/sessions"), &mut h.ctx(&mut session, &no_build));
    let (lines, keys) = match &events[..] {
        [AgentEvent::SessionsListed { lines, keys }] => (lines.clone(), keys.clone()),
        other => panic!("expected one SessionsListed: {other:?}"),
    };
    assert_eq!(lines.len(), 2);
    let active_line = lines.iter().find(|l| l.starts_with('*')).expect("an active marker");
    assert!(active_line.contains("(default)") && active_line.contains("/test"), "{active_line}");
    assert!(active_line.contains("test-1111.json"), "file name shown: {active_line}");
    assert!(active_line.contains("current default work"), "derived title: {active_line}");
    let other_line = lines.iter().find(|l| l.contains("alpha")).unwrap();
    assert!(!other_line.starts_with('*'), "only the active file is marked: {other_line}");
    assert!(other_line.contains("/other"), "cwd read from inside the file: {other_line}");
    // Keys: the name where one exists, the file name otherwise.
    assert!(keys.contains(&"alpha".to_string()));
    assert!(keys.contains(&"test-1111.json".to_string()));

    // Empty dir: a notice, not an empty listing.
    h.sessions_dir = "/nonexistent/temur-test-sessions".into();
    let events = commands::run(commands::parse("/sessions"), &mut h.ctx(&mut session, &no_build));
    assert!(notices(&events).iter().any(|n| n.contains("no saved sessions")), "{events:?}");
}

#[test]
fn resume_switches_session_and_redirects_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let sdir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![msg(vec![text("post-resume answer")], StopReason::EndTurn)],
    );
    let mut h = CmdHarness::new();
    h.sessions_dir = sdir.path().to_path_buf();
    h.persist = Some(sdir.path().join("test-1111.json"));

    write_session(
        sdir.path(),
        "test-1111-alpha.json",
        "/test",
        Some("alpha"),
        vec![user_msg("older prompt"), assistant_msg(vec![text("older answer")])],
    );

    let events = commands::run(
        commands::parse("/resume alpha"),
        &mut h.ctx(&mut session, &no_build),
    );
    match &events[0] {
        AgentEvent::SessionLoaded { items, notice } => {
            assert_eq!(
                items,
                &vec![
                    ReplayItem::User("older prompt".into()),
                    ReplayItem::Assistant("older answer".into()),
                ]
            );
            assert!(notice.contains("resumed session: 2 messages"), "{notice}");
        }
        other => panic!("first event must be SessionLoaded: {other:?}"),
    }
    // Same-project resume: no cwd advisory.
    assert!(!notices(&events).iter().any(|n| n.contains("recorded in")), "{events:?}");
    // Bookkeeping: saves now target the named file under its name.
    assert_eq!(h.persist.as_deref(), Some(sdir.path().join("test-1111-alpha.json").as_path()));
    assert_eq!(h.session_name.as_deref(), Some("alpha"));
    assert_eq!(session.history().len(), 2);
    // Next turn continues the RESUMED conversation.
    collect_events(&mut session, "next");
    let req = requests.borrow()[0].clone();
    assert_eq!(req.messages.len(), 3);

    // Same-session key again: a friendly no-op, nothing re-loaded.
    let events = commands::run(
        commands::parse("/resume alpha"),
        &mut h.ctx(&mut session, &no_build),
    );
    assert!(
        notices(&events).iter().any(|n| n.contains("already on this session")),
        "{events:?}"
    );
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::SessionLoaded { .. })));
    assert_eq!(session.history().len(), 4, "history untouched by the no-op");
}

#[test]
fn resume_failures_are_atomic_ambiguous_and_missing_are_clean_errors() {
    let dir = tempfile::tempdir().unwrap();
    let sdir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(
        dir.path(),
        vec![msg(vec![text("answer")], StopReason::EndTurn)],
    );
    collect_events(&mut session, "live conversation");
    let history_before = session.history().len();

    let mut h = CmdHarness::new();
    h.sessions_dir = sdir.path().to_path_buf();
    h.persist = Some(sdir.path().join("test-1111.json"));

    // Ambiguous: the same name in two OTHER projects.
    write_session(sdir.path(), "othera-2222-beta.json", "/other-a", Some("beta"), vec![]);
    write_session(sdir.path(), "otherb-3333-beta.json", "/other-b", Some("beta"), vec![]);
    // Corrupt: resolvable by prefix, unloadable.
    std::fs::write(sdir.path().join("broken-4444.json"), "{not json").unwrap();

    for (line, needle) in [
        ("/resume beta", "several projects"),
        ("/resume zzz", "no saved session"),
        ("/resume broken-", "session unchanged"),
    ] {
        let events = commands::run(commands::parse(line), &mut h.ctx(&mut session, &no_build));
        assert!(
            notices(&events).iter().any(|n| n.contains(needle)),
            "{line}: {events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(e, AgentEvent::SessionLoaded { .. })),
            "{line}: nothing loaded"
        );
        // Atomicity: the live session and its save target are untouched.
        assert_eq!(session.history().len(), history_before, "{line}");
        assert_eq!(h.persist.as_deref(), Some(sdir.path().join("test-1111.json").as_path()));
        assert_eq!(h.session_name, None, "{line}");
    }
    // The ambiguous error lists both candidates with their cwds.
    let events = commands::run(commands::parse("/resume beta"), &mut h.ctx(&mut session, &no_build));
    let ns = notices(&events);
    assert!(ns[0].contains("/other-a") && ns[0].contains("/other-b"), "{ns:?}");
}

#[test]
fn resume_across_projects_warns_that_tools_stay_here() {
    let dir = tempfile::tempdir().unwrap();
    let sdir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    h.sessions_dir = sdir.path().to_path_buf();

    write_session(
        sdir.path(),
        "elsewhere-5555-gamma.json",
        "/elsewhere",
        Some("gamma"),
        vec![user_msg("remote work"), assistant_msg(vec![text("done")])],
    );
    let events = commands::run(
        commands::parse("/resume gamma"),
        &mut h.ctx(&mut session, &no_build),
    );
    assert!(matches!(&events[0], AgentEvent::SessionLoaded { .. }));
    assert!(
        notices(&events).iter().any(|n| n
            == "session was recorded in /elsewhere; tools run in the current directory /test"),
        "{events:?}"
    );
    assert_eq!(h.session_name.as_deref(), Some("gamma"));
}

#[test]
fn resume_drops_a_trailing_unanswered_prompt_with_the_existing_notice() {
    let dir = tempfile::tempdir().unwrap();
    let sdir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    h.sessions_dir = sdir.path().to_path_buf();

    write_session(
        sdir.path(),
        "test-1111-delta.json",
        "/test",
        Some("delta"),
        vec![
            user_msg("answered"),
            assistant_msg(vec![text("the answer")]),
            user_msg("never answered"),
        ],
    );
    let events = commands::run(
        commands::parse("/resume delta"),
        &mut h.ctx(&mut session, &no_build),
    );
    match &events[0] {
        AgentEvent::SessionLoaded { items, notice } => {
            assert_eq!(items.len(), 2, "dropped prompt is not replayed: {items:?}");
            assert!(notice.contains("2 messages"), "summary counts the seeded set: {notice}");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        notices(&events).iter().any(|n| n.contains("never answered")),
        "{events:?}"
    );
    assert_eq!(session.history().len(), 2);
}

#[test]
fn new_session_redirects_persistence_without_writing_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let sdir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(
        dir.path(),
        vec![msg(vec![text("answer")], StopReason::EndTurn)],
    );
    collect_events(&mut session, "old conversation");
    let mut h = CmdHarness::new();
    h.sessions_dir = sdir.path().to_path_buf();
    h.cwd = dir.path().to_path_buf(); // a real directory, so the hash is stable
    h.persist = Some(sdir.path().join("test-1111.json"));

    // The name is sanitized: "my*alpha!" -> "myalpha".
    let events = commands::run(
        commands::parse("/new my*alpha!"),
        &mut h.ctx(&mut session, &no_build),
    );
    assert!(events.contains(&AgentEvent::SessionCleared));
    assert!(
        notices(&events)
            .iter()
            .any(|n| n.contains("\"myalpha\"") && n.contains("created on the first turn")),
        "{events:?}"
    );
    assert!(session.history().is_empty(), "in-memory state cleared");
    assert_eq!(h.session_name.as_deref(), Some("myalpha"));
    let new_path = h.persist.clone().unwrap();
    assert!(new_path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .ends_with("-myalpha.json"));
    // Quit before a first turn: NO file exists yet.
    assert!(!new_path.exists(), "no empty-file write");
    // The sessions dir holds only the pre-existing default file.
    assert_eq!(std::fs::read_dir(sdir.path()).unwrap().count(), 0);
}

#[test]
fn new_session_rejects_duplicates_and_unusable_names_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let sdir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(
        dir.path(),
        vec![msg(vec![text("answer")], StopReason::EndTurn)],
    );
    collect_events(&mut session, "keep me");
    let mut h = CmdHarness::new();
    h.sessions_dir = sdir.path().to_path_buf();
    h.cwd = dir.path().to_path_buf();
    let original = sdir.path().join("test-1111.json");
    h.persist = Some(original.clone());

    // A session named "dup" already exists for THIS cwd.
    let dup_file = temur::session_store::named_session_file_name(dir.path(), "dup");
    write_session(sdir.path(), &dup_file, "/test", Some("dup"), vec![]);

    for (line, needle) in [
        // Duplicate name: error points at /resume.
        ("/new dup", "/resume dup"),
        // Sanitize collision: "du*p" sanitizes to the existing "dup".
        ("/new du*p", "/resume dup"),
        // Nothing survives sanitizing.
        ("/new ///", "no usable characters"),
    ] {
        let events = commands::run(commands::parse(line), &mut h.ctx(&mut session, &no_build));
        assert!(
            notices(&events).iter().any(|n| n.contains(needle)),
            "{line}: {events:?}"
        );
        assert!(!events.contains(&AgentEvent::SessionCleared), "{line}");
        assert_eq!(session.history().len(), 2, "{line}: history untouched");
        assert_eq!(h.persist.as_deref(), Some(original.as_path()), "{line}");
        assert_eq!(h.session_name, None, "{line}");
    }
}

#[test]
fn status_reports_the_session_name_or_default() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    h.persist = Some("/state/sessions/test-1111.json".into());

    let events = commands::run(commands::parse("/status"), &mut h.ctx(&mut session, &no_build));
    assert!(
        notices(&events)
            .iter()
            .any(|n| n.contains("session file: /state/sessions/test-1111.json")
                && n.contains("session: (default)")),
        "{events:?}"
    );

    h.persist = Some("/state/sessions/test-1111-alpha.json".into());
    h.session_name = Some("alpha".into());
    let events = commands::run(commands::parse("/status"), &mut h.ctx(&mut session, &no_build));
    assert!(
        notices(&events).iter().any(|n| n.contains("session: alpha")),
        "{events:?}"
    );
}

#[test]
fn help_derives_from_the_command_table() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_with(dir.path(), vec![]);
    let mut h = CmdHarness::new();
    let build = |_: &ResolvedProfile| -> Result<Box<dyn Provider>, temur::error::Error> {
        unreachable!()
    };
    let events = commands::run(commands::parse("/help"), &mut h.ctx(&mut session, &build));
    let ns = notices(&events);
    // One line per table row plus the exit line; /models present.
    assert_eq!(ns.len(), commands::COMMANDS.len() + 1);
    for (name, _, _) in commands::COMMANDS {
        assert!(ns.iter().any(|l| l.starts_with(name)), "{name} missing: {ns:?}");
    }
    assert!(ns.iter().any(|l| l.starts_with("/models ")), "{ns:?}");
    assert_eq!(ns.last().unwrap(), "exit or quit — leave");
}

// --- T26 mid-session cost advisory -----------------------------------------

/// A session on a PRICED, keyed anthropic selection: $3 per Mtok in, $15 per
/// Mtok out, so 1M input tokens is exactly $3 and every assertion below reads
/// straight off the usage numbers. `step` is the advisory step in dollars.
fn priced_session(
    dir: &std::path::Path,
    responses: Vec<ResponseMessage>,
    step: f64,
    seed: Option<temur::session_store::SessionSeed>,
) -> Session {
    let provider = MockProvider {
        responses: RefCell::new(responses),
        requests: Rc::new(RefCell::new(vec![])),
    };
    let cfg = SessionConfig {
        model: "claude-sonnet-5".into(),
        max_tokens: 32_000,
        system: None,
        thinking: false,
        cwd: dir.to_path_buf(),
        max_iterations: 50,
        temperature: None,
        top_p: None,
        context_window: None,
        max_tokens_source: None,
        prose_tool_calls: true,
        cost_rates: Some(temur::cost::CostRates {
            provider: "anthropic".into(),
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
        }),
        cost_advisory_step_usd: step,
        auto_compact: false,
    };
    match seed {
        Some(s) => Session::resume(Box::new(provider), Registry::standard(), cfg, s),
        None => Session::new(Box::new(provider), Registry::standard(), cfg),
    }
}

/// Input-token-only usage, so a dollar figure is `input / 1M * $3`.
fn usage_in(input: u64) -> serde_json::Value {
    serde_json::json!({"input_tokens": input, "output_tokens": 0})
}

fn cost_notices(events: &[AgentEvent]) -> Vec<String> {
    notices(events)
        .into_iter()
        .filter(|n| n.starts_with("cost:"))
        .collect()
}

#[test]
fn the_cost_advisory_fires_inside_a_turn_not_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("note.txt");
    // Round-trip one costs $6 and calls a tool, so the turn continues after
    // the advisory: this is the $26-turn shape, caught while it is running.
    let mut session = priced_session(
        dir.path(),
        vec![
            msg_with_usage(
                vec![tool_use(
                    "tu_1",
                    "write",
                    serde_json::json!({"filePath": file.to_str().unwrap(), "content": "x"}),
                )],
                StopReason::ToolUse,
                usage_in(2_000_000),
            ),
            msg_with_usage(vec![text("done")], StopReason::EndTurn, usage_in(1_000)),
        ],
        5.0,
        None,
    );
    let events = collect_events(&mut session, "go");
    let cost = cost_notices(&events);
    assert_eq!(cost.len(), 1, "exactly one advisory: {cost:?}");
    assert_eq!(
        cost[0],
        "cost: this session has crossed $5.00 (estimate: ~$6.00 at configured list rates); set cost_advisory_step_usd to change the step or 0 to disable"
    );
    // Mid-turn, not at the end: the tool still ran after the advisory landed.
    let advisory_at = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Notice(n) if n.starts_with("cost:")))
        .unwrap();
    let tool_at = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolEnd { .. }))
        .unwrap();
    assert!(advisory_at < tool_at, "advisory came after the tool: {events:?}");
}

#[test]
fn one_response_clearing_two_steps_advises_once_at_the_highest() {
    let dir = tempfile::tempdir().unwrap();
    // $12 in a single response: crosses $5 AND $10, and says $10 once.
    let mut session = priced_session(
        dir.path(),
        vec![msg_with_usage(
            vec![text("a")],
            StopReason::EndTurn,
            usage_in(4_000_000),
        )],
        5.0,
        None,
    );
    let cost = cost_notices(&collect_events(&mut session, "go"));
    assert_eq!(cost.len(), 1, "never a burst: {cost:?}");
    assert!(cost[0].contains("crossed $10.00"), "{cost:?}");
    assert!(cost[0].contains("~$12.00"), "{cost:?}");
}

#[test]
fn resuming_an_expensive_session_never_advises_for_money_already_spent() {
    let dir = tempfile::tempdir().unwrap();
    // The seed already spent $6 (latched at one step). The next response
    // adds $0.003: real spend, but nothing new crossed.
    let seed = temur::session_store::SessionSeed {
        history: vec![],
        session_usage: Usage {
            input_tokens: Some(2_000_000),
            ..Usage::default()
        },
        todos: vec![],
        last_context_used: None,
    };
    let mut session = priced_session(
        dir.path(),
        vec![
            msg_with_usage(vec![text("a")], StopReason::EndTurn, usage_in(1_000)),
            msg_with_usage(vec![text("b")], StopReason::EndTurn, usage_in(2_000_000)),
        ],
        5.0,
        Some(seed),
    );
    assert!(
        cost_notices(&collect_events(&mut session, "one")).is_empty(),
        "resumed spend must not re-advise"
    );
    // New spend that crosses the NEXT multiple still fires: $6.003 -> $12.
    let cost = cost_notices(&collect_events(&mut session, "two"));
    assert_eq!(cost.len(), 1, "{cost:?}");
    assert!(cost[0].contains("crossed $10.00"), "{cost:?}");
}

#[test]
fn clearing_the_session_rearms_the_advisory() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = priced_session(
        dir.path(),
        vec![
            msg_with_usage(vec![text("a")], StopReason::EndTurn, usage_in(2_000_000)),
            msg_with_usage(vec![text("b")], StopReason::EndTurn, usage_in(2_000_000)),
        ],
        5.0,
        None,
    );
    assert_eq!(cost_notices(&collect_events(&mut session, "one")).len(), 1);
    // /clear zeroes the usage totals, so the next $5 is new money again.
    session.clear_history();
    let cost = cost_notices(&collect_events(&mut session, "two"));
    assert_eq!(cost.len(), 1, "{cost:?}");
    assert!(cost[0].contains("crossed $5.00"), "{cost:?}");
}

#[test]
fn a_switch_relatches_against_the_new_rates() {
    let dir = tempfile::tempdir().unwrap();
    // $6 spent at the old rates, advisory already fired at $5.
    let mut session = priced_session(
        dir.path(),
        vec![msg_with_usage(
            vec![text("a")],
            StopReason::EndTurn,
            usage_in(2_000_000),
        )],
        5.0,
        None,
    );
    assert_eq!(cost_notices(&collect_events(&mut session, "one")).len(), 1);
    // Switch onto a 10x pricier selection: the SAME 2M tokens now estimate
    // at $60. That is not new spend, so it must not advise; only spend past
    // the new latch does.
    let next = MockProvider {
        responses: RefCell::new(vec![
            msg_with_usage(vec![text("b")], StopReason::EndTurn, usage_in(1_000)),
            msg_with_usage(vec![text("c")], StopReason::EndTurn, usage_in(1_000_000)),
        ]),
        requests: Rc::new(RefCell::new(vec![])),
    };
    session.switch_provider(
        Box::new(next),
        &ResolvedProfile {
            // Keyed and priced, so the selection itself carries the rates.
            api_key_file: Some("/tmp/k".into()),
            price_input_per_mtok: Some(30.0),
            price_output_per_mtok: Some(150.0),
            ..selection("claude-opus-5", 32_000, None)
        },
        None,
    );
    assert!(
        cost_notices(&collect_events(&mut session, "two")).is_empty(),
        "past spend must not fire under new rates"
    );
    // $60.03 + $30 = $90.03: one advisory, at $90.
    let cost = cost_notices(&collect_events(&mut session, "three"));
    assert_eq!(cost.len(), 1, "{cost:?}");
    assert!(cost[0].contains("crossed $90.00"), "{cost:?}");
}

#[test]
fn the_advisory_is_off_without_rates_and_at_step_zero() {
    let dir = tempfile::tempdir().unwrap();
    // Step 0 is the documented disable, at any spend.
    let mut session = priced_session(
        dir.path(),
        vec![msg_with_usage(
            vec![text("a")],
            StopReason::EndTurn,
            usage_in(100_000_000),
        )],
        0.0,
        None,
    );
    assert!(cost_notices(&collect_events(&mut session, "go")).is_empty());
    // And an unpriced or keyless selection carries no rates at all, so it
    // never sees the advisory whatever the step says.
    let (mut session, _) = session_with(
        dir.path(),
        vec![msg_with_usage(
            vec![text("a")],
            StopReason::EndTurn,
            usage_in(100_000_000),
        )],
    );
    assert!(cost_notices(&collect_events(&mut session, "go")).is_empty());
}

// -------------------------------------------- T28: skill index in the loop

/// A session whose registry carries the skill tool, over a throwaway skill
/// tree. `window` reaches the tool as the context-scaled output cap.
fn skill_session(
    cwd: &std::path::Path,
    skill_root: &std::path::Path,
    window: Option<u64>,
    responses: Vec<ResponseMessage>,
) -> (Session, Rc<RefCell<Vec<ChatRequest>>>) {
    let requests = Rc::new(RefCell::new(vec![]));
    let provider = MockProvider {
        responses: RefCell::new(responses),
        requests: requests.clone(),
    };
    let cfg = SessionConfig {
        model: "claude-sonnet-5".into(),
        max_tokens: 32_000,
        system: Some("test system".into()),
        thinking: false,
        cwd: cwd.to_path_buf(),
        max_iterations: 50,
        temperature: None,
        top_p: None,
        context_window: window,
        max_tokens_source: None,
        prose_tool_calls: true,
        cost_rates: None,
        cost_advisory_step_usd: temur::config::DEFAULT_COST_ADVISORY_STEP_USD,
        auto_compact: false,
    };
    let registry =
        Registry::standard_with_skills(vec![skill_root.join(".temur/skills")]);
    (
        Session::new(Box::new(provider), registry, cfg),
        requests,
    )
}

fn write_big_skill(root: &std::path::Path, name: &str, chapters: usize) -> String {
    let dir = root.join(".temur/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let mut body = String::from("---\nname: demo\ndescription: A demo skill.\n---\n\n");
    body.push_str("Drive the widget CLI with these instructions.\n\n");
    for i in 1..=chapters {
        body.push_str(&format!("## Chapter {i}\n\n"));
        for k in 0..40 {
            body.push_str(&format!(
                "Step {k} of chapter {i}: run the widget command and check its output.\n"
            ));
        }
        body.push('\n');
    }
    std::fs::write(dir.join("SKILL.md"), &body).unwrap();
    body
}

fn tool_results(history: &[RequestMessage]) -> Vec<String> {
    history
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect()
}

/// The whole feature, driven through the agent loop the way a model would
/// use it: load an oversized skill, get an index back, fetch one numbered
/// section, then answer. What comes back for the section must be that
/// section's bytes, not a paraphrase and not a fragment.
#[test]
fn oversized_skill_indexes_then_serves_a_section_through_the_loop() {
    let dir = tempfile::tempdir().unwrap();
    let raw = write_big_skill(dir.path(), "demo", 12);
    let body = temur::skills::minify(&raw);
    let sections = temur::skills::scan_sections(&body);
    // Section 3 is "## Chapter 3" (sections are 1-based in the index).
    let expected = sections[2].text(&body);

    let (mut session, requests) = skill_session(
        dir.path(),
        dir.path(),
        None,
        vec![
            msg(vec![tool_use("t1", "skill", serde_json::json!({"name": "demo"}))], StopReason::ToolUse),
            msg(
                vec![tool_use("t2", "skill", serde_json::json!({"name": "demo", "section": 3}))],
                StopReason::ToolUse,
            ),
            msg(vec![text("Chapter 3 says to run the widget command.")], StopReason::EndTurn),
        ],
    );
    collect_events(&mut session, "use the demo skill");

    let results = tool_results(session.history());
    assert_eq!(results.len(), 2, "two tool results: the index and the section");
    assert!(results[0].starts_with("<skill_index name=\"demo\">"), "{}", results[0]);
    assert!(results[0].contains("3. ## Chapter 3 ("), "{}", results[0]);
    assert!(
        !results[0].contains("Step 39 of chapter 12"),
        "the index lists sections, it does not carry them"
    );

    // The section result carries that section's exact bytes.
    assert!(results[1].starts_with("<skill_section name=\"demo\" number=\"3\""), "{}", results[1]);
    // The payload begins on the line after the opening tag: this skill
    // ships no assets, so T30 emits no base-directory line before it.
    let start = results[1].find('\n').unwrap() + 1;
    assert_eq!(
        &results[1][start..start + expected.len()],
        expected,
        "section payload must be the minified body's own bytes"
    );

    // Three round trips, and every one of them extends the previous
    // messages rather than editing them: the prompt-cache prefix holds.
    let reqs = requests.borrow();
    assert_eq!(reqs.len(), 3);
    for pair in reqs.windows(2) {
        let (prev, next) = (&pair[0], &pair[1]);
        assert!(prev.messages.len() < next.messages.len(), "history only grows");
        assert_eq!(
            prev.messages[..],
            next.messages[..prev.messages.len()],
            "an earlier message was rewritten; the cache prefix is broken"
        );
    }
}

/// The beneficiary claim, pinned: the same skill that fits whole for a
/// big-context model gets indexed for a small-context one. This is the
/// case the feature was built for, and it engages through configuration
/// alone, with no code path of its own.
#[test]
fn a_small_context_window_is_what_turns_a_full_skill_into_an_index() {
    let dir = tempfile::tempdir().unwrap();
    write_big_skill(dir.path(), "demo", 3); // mid-size: ~9k chars

    let load = vec![
        msg(vec![tool_use("t1", "skill", serde_json::json!({"name": "demo"}))], StopReason::ToolUse),
        msg(vec![text("ok")], StopReason::EndTurn),
    ];
    // No window configured: the 30,000-char ceiling, and it fits whole.
    let (mut big, _) = skill_session(dir.path(), dir.path(), None, load.clone());
    collect_events(&mut big, "load it");
    let full = tool_results(big.history()).remove(0);
    assert!(full.starts_with("<skill_content"), "{}", &full[..60]);

    // A local model with an 8k window: the same file, same bytes on disk,
    // now indexed instead of cut off.
    let (mut small, _) = skill_session(dir.path(), dir.path(), Some(8_000), load);
    collect_events(&mut small, "load it");
    let indexed = tool_results(small.history()).remove(0);
    assert!(indexed.starts_with("<skill_index"), "{}", &indexed[..60]);
    assert!(indexed.chars().count() <= 8_000, "the index fits the scaled cap");
    assert!(indexed.contains("over this session's 8000-char tool output limit"), "{indexed}");
}

// ---- T40: auto-compaction of an unattended turn ----

fn session_auto_compact(
    dir: &std::path::Path,
    responses: Vec<ResponseMessage>,
    context_window: Option<u64>,
    max_tokens: u32,
    auto_compact: bool,
) -> (Session, Rc<RefCell<Vec<ChatRequest>>>) {
    let requests = Rc::new(RefCell::new(vec![]));
    let provider = MockProvider {
        responses: RefCell::new(responses),
        requests: requests.clone(),
    };
    let cfg = SessionConfig {
        model: "local-test".into(),
        max_tokens,
        system: None,
        thinking: false,
        cwd: dir.to_path_buf(),
        max_iterations: 50,
        temperature: None,
        top_p: None,
        context_window,
        max_tokens_source: None,
        prose_tool_calls: true,
        cost_rates: None,
        cost_advisory_step_usd: temur::config::DEFAULT_COST_ADVISORY_STEP_USD,
        auto_compact,
    };
    (
        Session::new(Box::new(provider), Registry::standard(), cfg),
        requests,
    )
}

/// A round-trip that calls bash with a distinct command, so the T36 futile
/// guard never sees a repeat.
fn rt(n: u32, used: u64) -> ResponseMessage {
    msg_with_usage(
        vec![tool_use(
            &format!("tu_{n}"),
            "bash",
            serde_json::json!({"command": format!("echo {n}")}),
        )],
        StopReason::ToolUse,
        serde_json::json!({"input_tokens": used, "output_tokens": 0}),
    )
}

fn summary_response(body: &str) -> ResponseMessage {
    msg_with_usage(
        vec![text(body)],
        StopReason::EndTurn,
        serde_json::json!({"input_tokens": 50, "output_tokens": 10}),
    )
}

/// "compacted: N round-trip(s) summarized, K kept, ~B -> ~A bytes" -> (B, A)
fn parse_compaction_bytes(notice: &str) -> (u64, u64) {
    let tail = notice.rsplit(", ~").next().unwrap();
    let nums: Vec<u64> = tail
        .trim_end_matches(" bytes")
        .split(" -> ~")
        .map(|p| p.parse().unwrap())
        .collect();
    (nums[0], nums[1])
}

fn texts_of(m: &RequestMessage) -> Vec<&str> {
    m.content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn auto_compact_folds_the_middle_and_keeps_prompt_and_last_two_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    // window 1000, max_tokens 100: only the 80% arm can fire (the tight arm
    // would need used > 900). Round-trip 4 crosses at exactly 800.
    let (mut session, requests) = session_auto_compact(
        dir.path(),
        vec![
            rt(1, 100),
            rt(2, 100),
            rt(3, 100),
            rt(4, 800), // crosses
            summary_response("WORK SO FAR"),
            rt(5, 100),
            msg_with_usage(
                vec![text("done")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 100, "output_tokens": 0}),
            ),
        ],
        Some(1000),
        100,
        true,
    );
    let n = notices(&collect_events(&mut session, "the task"));

    // The advisory site said what it was about to do; the safe point said
    // what it did.
    assert!(
        n.iter().any(|x| x == "context: ~800 of 1000 tokens used; compacting automatically"),
        "auto-compact notice: {n:?}"
    );
    // The rider's wording: round-trips and bytes. Round-trips 1 and 2 were
    // folded, 3 and 4 kept, and the history actually got smaller.
    let outcome = n
        .iter()
        .find(|x| x.starts_with("compacted: "))
        .expect("outcome notice");
    assert!(
        outcome.starts_with("compacted: 2 round-trip(s) summarized, 2 kept, ~"),
        "outcome notice: {outcome}"
    );
    let (before_b, after_b) = parse_compaction_bytes(outcome);
    assert!(after_b < before_b, "this fold shrank the history: {outcome}");
    assert!(
        !n.iter().any(|x| x.contains("/compact frees the window")),
        "the plain advisory must NOT also fire for one crossing: {n:?}"
    );

    let reqs = requests.borrow();
    // 7 provider calls: 4 round-trips, the summary call, then 2 more.
    assert_eq!(reqs.len(), 7, "one extra call for the summary");

    // The summary call itself: tools omitted entirely, instruction appended.
    let summary_req = &reqs[4];
    assert!(summary_req.tools.is_empty(), "no tool_use possible in a summary call");
    let last = summary_req.messages.last().unwrap();
    assert!(
        texts_of(last).iter().any(|t| t.contains("You are running low on context")),
        "auto-compact instruction, not /compact's: {:?}",
        texts_of(last)
    );

    // The FIRST request built on the compacted history: prompt, summary
    // pair, then round-trips 3 and 4 verbatim.
    let m = &reqs[5].messages;
    assert_eq!(m.len(), 7, "prompt + summary pair + 2 round-trips");
    assert_eq!(texts_of(&m[0]), vec!["the task"], "prompt verbatim");
    assert_eq!(m[1].role, Role::Assistant);
    assert!(texts_of(&m[1])[0].contains("WORK SO FAR"), "the model's summary");
    assert_eq!(m[2].role, Role::User);
    // Round-trips 3 and 4 survived whole; 1 and 2 were folded away.
    let ids: Vec<&str> = m
        .iter()
        .flat_map(|x| x.content.iter())
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec!["tu_3", "tu_4"], "the last two round-trips, verbatim");
    for pair in m.windows(2) {
        assert_ne!(pair[0].role, pair[1].role, "alternation holds on the wire");
    }
    // And the turn ran to completion on the compacted history.
    assert_eq!(reqs[6].messages.len(), 9);
}

#[test]
fn auto_compact_fires_on_the_tight_arm_too() {
    let dir = tempfile::tempdir().unwrap();
    // window 1000, max_tokens 800: used 250 leaves 750 < 800. Far below 80%,
    // so only the remaining-window arm can be firing here.
    let (mut session, requests) = session_auto_compact(
        dir.path(),
        vec![
            rt(1, 100),
            rt(2, 100),
            rt(3, 250), // tight arm
            summary_response("S"),
            msg_with_usage(
                vec![text("done")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 100, "output_tokens": 0}),
            ),
        ],
        Some(1000),
        800,
        true,
    );
    let n = notices(&collect_events(&mut session, "the task"));
    assert!(
        n.iter().any(|x| x == "context: ~250 of 1000 tokens used; compacting automatically"),
        "{n:?}"
    );
    // One round-trip folded, two kept. Worth pinning that the byte figures
    // are REPORTED rather than assumed to improve: folding a single short
    // round-trip into a summary plus its resume message can grow the
    // history, and the notice says so instead of claiming a saving.
    let outcome = n
        .iter()
        .find(|x| x.starts_with("compacted: "))
        .expect("outcome notice");
    assert!(
        outcome.starts_with("compacted: 1 round-trip(s) summarized, 2 kept, ~"),
        "outcome notice: {outcome}"
    );
    assert_eq!(requests.borrow().len(), 5);
}

#[test]
fn auto_compact_is_bounded_at_three_then_advises() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_auto_compact(
        dir.path(),
        vec![
            rt(1, 100),
            rt(2, 100),
            rt(3, 800), // cross 1
            summary_response("S1"),
            rt(4, 800), // cross 2
            summary_response("S2"),
            rt(5, 800), // cross 3
            summary_response("S3"),
            rt(6, 800), // cross 4: over the bound
            msg_with_usage(
                vec![text("done")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 100, "output_tokens": 0}),
            ),
        ],
        Some(1000),
        100,
        true,
    );
    let n = notices(&collect_events(&mut session, "the task"));
    let compacting = n.iter().filter(|x| x.ends_with("compacting automatically")).count();
    let compacted = n.iter().filter(|x| x.starts_with("compacted: ")).count();
    let advised = n.iter().filter(|x| x.contains("/compact frees the window")).count();
    assert_eq!(compacting, 3, "three compactions announced: {n:?}");
    assert_eq!(compacted, 3, "three compactions performed: {n:?}");
    assert_eq!(advised, 1, "the fourth crossing advises instead: {n:?}");
    // The fourth crossing is the ONLY plain advisory, and it is byte-identical
    // to what a session without auto-compaction would have printed.
    assert!(n.iter().any(|x| x
        == "context: ~800 of 1000 tokens used; /compact frees the window by summarizing the conversation, or start a new session"));
    // 10 responses = 7 turn round-trips + 3 summary calls.
    assert_eq!(requests.borrow().len(), 10);
}

#[test]
fn auto_compact_failure_is_named_and_the_turn_continues_uncompacted() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_auto_compact(
        dir.path(),
        vec![
            rt(1, 100),
            rt(2, 100),
            rt(3, 800),               // crosses
            summary_response("   "),  // whitespace only: fail-closed
            msg_with_usage(
                vec![text("done")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 100, "output_tokens": 0}),
            ),
        ],
        Some(1000),
        100,
        true,
    );
    let n = notices(&collect_events(&mut session, "the task"));
    assert!(
        n.iter().any(|x| x
            == "auto-compact failed (the model returned an empty summary); continuing without compacting"),
        "{n:?}"
    );
    assert!(!n.iter().any(|x| x.starts_with("compacted: ")), "nothing was compacted: {n:?}");
    // History untouched by the failed attempt: prompt + 3 whole round-trips,
    // then the final assistant message.
    assert_eq!(session.history().len(), 8);
    let reqs = requests.borrow();
    // The request after the failure went out on the UNCOMPACTED history.
    assert_eq!(reqs[4].messages.len(), 7);
}

#[test]
fn auto_compact_leaves_a_turn_with_too_few_round_trips_alone() {
    let dir = tempfile::tempdir().unwrap();
    // Crossing on round-trip 1: one completed round-trip is nothing to fold.
    let (mut session, requests) = session_auto_compact(
        dir.path(),
        vec![
            rt(1, 800),
            // Still over at turn end, so the latch is the only thing that
            // could suppress an advisory afterwards.
            msg_with_usage(
                vec![text("done")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 800, "output_tokens": 0}),
            ),
        ],
        Some(1000),
        100,
        true,
    );
    let n = notices(&collect_events(&mut session, "the task"));
    // F2 (v0.29.1): the turn ends still over the threshold and nothing ever
    // folded, so the ordinary advisory is what remains - once, at turn end.
    // Rider 2 printed nothing at all here, and in one-shot `-p`, the mode
    // where auto_compact defaults ON, there is no later turn to carry the
    // crossing: "nothing yet" became "nothing ever", and the run died on the
    // next request with no warning ever printed. docs/USAGE.md described
    // this case as advising throughout.
    assert_eq!(
        n,
        vec!["context: ~800 of 1000 tokens used; /compact frees the window by summarizing the conversation, or start a new session".to_string()],
        "exactly one advisory, at turn end: {n:?}"
    );
    // No summary call was spent...
    assert_eq!(requests.borrow().len(), 2);
    // ...and, the rider-2 rule, the once-per-session latch was NOT consumed
    // by a crossing nobody could act on. Before rider 2 this crossing spent
    // the latch and locked auto-compaction out of the whole session, so the
    // F2 line is deliberately printed OUTSIDE the latch: a later turn in a
    // REPL session can still fold.
    assert!(
        session.context_crossing().is_some(),
        "the latch must still be open after a non-actionable crossing"
    );
}

#[test]
fn auto_compact_off_keeps_todays_advisory_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let responses = || {
        vec![
            rt(1, 100),
            rt(2, 100),
            rt(3, 800),
            msg_with_usage(
                vec![text("done")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 100, "output_tokens": 0}),
            ),
        ]
    };
    let (mut session, requests) =
        session_auto_compact(dir.path(), responses(), Some(1000), 100, false);
    let n = notices(&collect_events(&mut session, "the task"));
    assert_eq!(
        n,
        vec![
            "context: ~800 of 1000 tokens used; /compact frees the window by summarizing the conversation, or start a new session"
                .to_string()
        ],
        "the REPL/TUI arm is exactly what it was before T40"
    );
    // No summary call, no compaction: 4 responses, 4 requests.
    assert_eq!(requests.borrow().len(), 4);
    assert_eq!(session.history().len(), 8);
}

// ---- T40 P2: per-round-trip persistence ----

/// A provider that reads the session file on every call, so the test can see
/// what was on disk BETWEEN round-trips rather than only at turn end.
struct WatchingProvider {
    responses: RefCell<Vec<ResponseMessage>>,
    path: std::path::PathBuf,
    /// History length observed on disk at each request, `None` = no file yet.
    seen: Rc<RefCell<Vec<Option<usize>>>>,
}

impl Provider for WatchingProvider {
    fn stream(
        &self,
        _req: &ChatRequest,
        _on_event: &mut dyn FnMut(StreamEvent),
        _cancel: &CancelToken,
    ) -> Result<ResponseMessage, ProviderError> {
        let observed = temur::session_store::load(&self.path)
            .ok()
            .map(|f| f.history.len());
        self.seen.borrow_mut().push(observed);
        Ok(self.responses.borrow_mut().remove(0))
    }
}

fn persist_target(path: &std::path::Path) -> temur::agent::PersistTarget {
    temur::agent::PersistTarget {
        path: path.to_path_buf(),
        provider: "anthropic".into(),
        model: "claude-sonnet-5".into(),
        cwd_display: "/tmp".into(),
        name: None,
        max_bytes: 1_000_000,
    }
}

fn watching_session(
    dir: &std::path::Path,
    path: &std::path::Path,
    responses: Vec<ResponseMessage>,
) -> (Session, Rc<RefCell<Vec<Option<usize>>>>) {
    let seen = Rc::new(RefCell::new(vec![]));
    let provider = WatchingProvider {
        responses: RefCell::new(responses),
        path: path.to_path_buf(),
        seen: seen.clone(),
    };
    let cfg = SessionConfig {
        model: "claude-sonnet-5".into(),
        max_tokens: 32_000,
        system: None,
        thinking: false,
        cwd: dir.to_path_buf(),
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
    session.set_persist_target(Some(persist_target(path)));
    (session, seen)
}

#[test]
fn the_session_file_grows_with_every_round_trip_not_only_at_turn_end() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    let (mut session, seen) = watching_session(
        dir.path(),
        &path,
        vec![
            rt(1, 10),
            rt(2, 10),
            rt(3, 10),
            msg_with_usage(
                vec![text("done")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 10, "output_tokens": 5}),
            ),
        ],
    );
    session.turn("go", &mut |_| {}).unwrap();

    // Request 1 sees the prompt alone; every later request sees the two
    // messages the previous round-trip added. Before T40 P2 this was
    // [None, None, None, None]: nothing reached disk until the turn ended.
    assert_eq!(
        *seen.borrow(),
        vec![Some(1), Some(3), Some(5), Some(7)],
        "history on disk grows 1 -> 3 -> 5 -> 7 across the turn"
    );

    // And the file at turn end is the whole turn.
    let on_disk = temur::session_store::load(&path).unwrap();
    assert_eq!(on_disk.history.len(), 8);
    assert_eq!(on_disk.history, session.history());
}

#[test]
fn the_assistant_half_of_a_round_trip_reaches_disk_before_its_tools_run() {
    // The SIGKILL window T40 P2 closes: a long tool call used to lose the
    // assistant message that asked for it. The tool itself is the observer.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    let marker = dir.path().join("observed.txt");
    let (mut session, _) = watching_session(
        dir.path(),
        &path,
        vec![
            msg_with_usage(
                vec![tool_use(
                    "tu_1",
                    "bash",
                    // Portable on purpose: the container suites have cp,
                    // not python. The copy is parsed back here in Rust.
                    serde_json::json!({
                        "command": format!("cp {} {}", path.display(), marker.display())
                    }),
                )],
                StopReason::ToolUse,
                serde_json::json!({"input_tokens": 10, "output_tokens": 5}),
            ),
            msg_with_usage(
                vec![text("done")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 10, "output_tokens": 5}),
            ),
        ],
    );
    session.turn("go", &mut |_| {}).unwrap();
    let mid_turn = temur::session_store::load(&marker)
        .expect("the session file existed while the tool was still running");
    assert_eq!(
        mid_turn.history.len(),
        2,
        "while the tool ran, disk already held prompt + the assistant message"
    );
}

#[test]
fn no_persist_target_writes_nothing_at_all() {
    // What `--mock` gets: main.rs leaves persist_path None, so the session
    // has no target and a whole turn touches no file.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    let (mut session, seen) = watching_session(
        dir.path(),
        &path,
        vec![
            rt(1, 10),
            msg_with_usage(
                vec![text("done")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 10, "output_tokens": 5}),
            ),
        ],
    );
    session.set_persist_target(None);
    session.turn("go", &mut |_| {}).unwrap();
    assert_eq!(*seen.borrow(), vec![None, None], "no file at any point");
    assert!(!path.exists(), "replay mode never persists");
}

#[test]
fn the_end_of_turn_file_is_what_it_would_have_been_before_p2() {
    // The mid-turn writes leave no residue: the last one wins and says
    // exactly what a single end-of-turn save would have said.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    let (mut session, _) = watching_session(
        dir.path(),
        &path,
        vec![msg_with_usage(
            vec![text("hi")],
            StopReason::EndTurn,
            serde_json::json!({"input_tokens": 10, "output_tokens": 5}),
        )],
    );
    session.turn("go", &mut |_| {}).unwrap();
    let after_turn = std::fs::read(&path).unwrap();

    // Save the same state once, the way turn end always did.
    let reference = dir.path().join("reference.json");
    let snap = session.snapshot();
    let file = temur::session_store::SessionFileRef {
        version: temur::session_store::FORMAT_VERSION,
        provider: "anthropic",
        model: "claude-sonnet-5",
        cwd: "/tmp",
        history: snap.history,
        session_usage: snap.session_usage,
        todos: snap.todos,
        last_context_used: snap.last_context_used,
        name: None,
    };
    temur::session_store::save(&reference, &file, 1_000_000, &mut |_| {}).unwrap();
    assert_eq!(
        after_turn,
        std::fs::read(&reference).unwrap(),
        "byte-identical to a single end-of-turn save"
    );
}

#[test]
fn a_failing_save_is_noticed_once_per_process_not_once_per_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    // A path under a FILE cannot be written, so every save fails.
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"x").unwrap();
    let path = blocker.join("s.json");
    let (mut session, _) = watching_session(
        dir.path(),
        &path,
        vec![
            rt(1, 10),
            rt(2, 10),
            msg_with_usage(
                vec![text("done")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 10, "output_tokens": 5}),
            ),
            msg_with_usage(
                vec![text("done again")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 10, "output_tokens": 5}),
            ),
        ],
    );
    let mut events = vec![];
    session.turn("go", &mut |e| events.push(e)).unwrap();
    let failures = notices(&events)
        .iter()
        .filter(|n| n.starts_with("session save failed:"))
        .count();
    assert_eq!(failures, 1, "one notice for a turn of many failed writes");

    // Still once across a LATER turn: the latch is per process, and the
    // turn completed normally despite every write failing.
    let mut events2 = vec![];
    session.turn("again", &mut |e| events2.push(e)).unwrap();
    let failures2 = notices(&events2)
        .iter()
        .filter(|n| n.starts_with("session save failed:"))
        .count();
    assert_eq!(failures2, 0, "already noticed");
}

#[test]
fn a_trimmed_save_is_noticed_once_per_process_not_once_per_round_trip() {
    // F3 (v0.29.1): the failure notice was latched and the SUCCESS-path trim
    // notice was not. T40 P2 turned one save per turn into two per
    // round-trip, so an over-cap session repeated an identical line up to a
    // hundred times in one agentic turn where it used to print once.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    let (mut session, _) = session_auto_compact(
        dir.path(),
        vec![
            msg_with_usage(
                vec![text(&"x".repeat(3000))],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 10, "output_tokens": 5}),
            ),
            msg_with_usage(
                vec![text("short")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 10, "output_tokens": 5}),
            ),
        ],
        None,
        100,
        false,
    );
    // Two turns first, with no persist target: the history has to be big
    // enough to need trimming, and a cut point to trim back to.
    session.turn("go", &mut |_| {}).unwrap();
    session.turn("again", &mut |_| {}).unwrap();

    let mut target = persist_target(&path);
    target.max_bytes = 1_000; // the first exchange alone is over 3000 bytes
    session.set_persist_target(Some(target));

    let trims = |events: &[AgentEvent]| {
        notices(events)
            .iter()
            .filter(|n| n.starts_with("session file exceeded"))
            .count()
    };
    let mut first = vec![];
    session.persist_now(&mut |e| first.push(e));
    let mut second = vec![];
    session.persist_now(&mut |e| second.push(e));
    assert_eq!(trims(&first), 1, "the trim is reported: {:?}", notices(&first));
    assert_eq!(trims(&second), 0, "and not again on the next write");
    assert!(
        std::fs::read(&path).unwrap().len() <= 1_000,
        "both writes still happened, trimmed to the cap"
    );
}

// ---- T40 rider: resume seam, notice order, outcome wording ----

/// A resumed session with a real history and a requests handle, so the seam
/// can be driven and the first post-seam request inspected.
fn resumed_auto_compact(
    dir: &std::path::Path,
    history: Vec<RequestMessage>,
    last_context_used: Option<u64>,
    responses: Vec<ResponseMessage>,
    context_window: Option<u64>,
    max_tokens: u32,
    auto_compact: bool,
) -> (Session, Rc<RefCell<Vec<ChatRequest>>>) {
    let requests = Rc::new(RefCell::new(vec![]));
    let provider = MockProvider {
        responses: RefCell::new(responses),
        requests: requests.clone(),
    };
    let cfg = SessionConfig {
        model: "claude-sonnet-5".into(),
        max_tokens,
        system: None,
        thinking: false,
        cwd: dir.to_path_buf(),
        max_iterations: 50,
        temperature: None,
        top_p: None,
        context_window,
        max_tokens_source: None,
        prose_tool_calls: true,
        cost_rates: None,
        cost_advisory_step_usd: temur::config::DEFAULT_COST_ADVISORY_STEP_USD,
        auto_compact,
    };
    let mut file = saved(history, vec![]);
    file.last_context_used = last_context_used;
    let (seed, _) = store::prepare_seed(file);
    (
        Session::resume(Box::new(provider), Registry::standard(), cfg, seed),
        requests,
    )
}

/// Two exchanges plus a tool round-trip: enough that /compact's rule has a
/// real tail (the last plain user message) and a real head to fold.
fn seeded_history() -> Vec<RequestMessage> {
    vec![
        user_msg("first task"),
        assistant_msg(vec![text("first answer")]),
        user_msg("second task"),
        assistant_msg(vec![tool_use("tu_1", "bash", serde_json::json!({"command": "echo 1"}))]),
        RequestMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "tu_1".into(),
                content: "1".into(),
                is_error: false,
            }],
        },
        assistant_msg(vec![text("second answer")]),
    ]
}

#[test]
fn the_resume_seam_compacts_instead_of_advising_when_auto_compact_is_on() {
    // The F4 gap the rider closes: before this, the seam set the advisory
    // latch before the turn began, so --continue -p could never
    // auto-compact and died on request 1 exactly as measured.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = resumed_auto_compact(
        dir.path(),
        seeded_history(),
        Some(900), // 900 of 1000: over the 80% arm at load
        vec![
            summary_response("PRIOR WORK"),
            msg_with_usage(
                vec![text("done")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 100, "output_tokens": 0}),
            ),
        ],
        Some(1000),
        100,
        true,
    );
    let mut events = vec![];
    session.resume_seam_context_action(&mut |e| events.push(e));
    let n = notices(&events);

    assert_eq!(
        n[0], "context: ~900 of 1000 tokens used; compacting automatically",
        "the seam announces, it does not advise: {n:?}"
    );
    assert!(
        n[1].starts_with("compacted: "),
        "and reports the outcome: {n:?}"
    );
    assert!(
        !n.iter().any(|x| x.contains("/compact frees the window")),
        "no advisory once the compaction succeeded: {n:?}"
    );
    // Exactly one provider call so far: the summary.
    assert_eq!(requests.borrow().len(), 1);

    // The seam uses /compact's rule, so the tail is the last plain user
    // message onward, with the summary prepended INSIDE it.
    session.turn("next", &mut |_| {}).unwrap();
    let reqs = requests.borrow();
    let m = &reqs[1].messages;
    assert_eq!(m[0].role, Role::User);
    let head = texts_of(&m[0]);
    assert!(head[0].contains("PRIOR WORK"), "summary leads the tail: {head:?}");
    assert_eq!(head[1], "second task", "the tail's own prompt follows it");
    // ...and the new turn's prompt is last.
    assert_eq!(texts_of(m.last().unwrap()), vec!["next"]);
    for pair in m.windows(2) {
        assert_ne!(pair[0].role, pair[1].role, "alternation holds after the seam");
    }
}

#[test]
fn the_resume_seam_still_only_advises_when_auto_compact_is_off() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = resumed_auto_compact(
        dir.path(),
        seeded_history(),
        Some(900),
        vec![],
        Some(1000),
        100,
        false,
    );
    let mut events = vec![];
    session.resume_seam_context_action(&mut |e| events.push(e));
    assert_eq!(
        notices(&events),
        vec![
            "context: ~900 of 1000 tokens used; /compact frees the window by summarizing the conversation, or start a new session"
                .to_string()
        ],
        "byte-identical to the pre-rider seam"
    );
    assert_eq!(requests.borrow().len(), 0, "no summary call was spent");
}

#[test]
fn a_failed_seam_compaction_names_it_and_falls_back_to_the_advisory() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = resumed_auto_compact(
        dir.path(),
        seeded_history(),
        Some(900),
        vec![summary_response("   ")], // whitespace only: fail-closed
        Some(1000),
        100,
        true,
    );
    let before = session.history().len();
    let mut events = vec![];
    session.resume_seam_context_action(&mut |e| events.push(e));
    let n = notices(&events);
    assert_eq!(n[0], "context: ~900 of 1000 tokens used; compacting automatically");
    assert_eq!(
        n[1],
        "auto-compact failed (the model returned an empty summary); continuing without compacting"
    );
    assert!(n[2].contains("/compact frees the window"), "the reader still gets the advisory");
    assert_eq!(session.history().len(), before, "history untouched");
}

#[test]
fn the_seam_is_silent_below_the_threshold_whatever_auto_compact_says() {
    let dir = tempfile::tempdir().unwrap();
    for on in [true, false] {
        let (mut session, requests) = resumed_auto_compact(
            dir.path(),
            seeded_history(),
            Some(100), // nowhere near either arm
            vec![],
            Some(1000),
            100,
            on,
        );
        let mut events = vec![];
        session.resume_seam_context_action(&mut |e| events.push(e));
        assert!(notices(&events).is_empty(), "auto_compact={on}");
        assert_eq!(requests.borrow().len(), 0, "auto_compact={on}");
    }
}

#[test]
fn a_turn_too_short_to_fold_says_nothing_at_all() {
    // Rider 1 printed "compacting automatically" and then the ordinary
    // advisory, which read as a contradiction. Rider 2 goes further: a
    // crossing nobody can act on yet is not news, so it is not reported and
    // it does not spend the latch.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_auto_compact(
        dir.path(),
        vec![
            rt(1, 800), // crosses on round-trip 1: nothing to fold
            msg_with_usage(
                vec![text("done")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 100, "output_tokens": 0}),
            ),
        ],
        Some(1000),
        100,
        true,
    );
    let n = notices(&collect_events(&mut session, "the task"));
    // Silent, and it stays silent through F2's turn-end check too: the last
    // response reports 100 of 1000, so by the time the turn ends there is no
    // crossing left to report. F2 asks the condition again rather than
    // replaying a remembered one, precisely so a transient crossing does not
    // produce a stale line.
    assert!(n.is_empty(), "silent, not contradictory: {n:?}");
    assert_eq!(requests.borrow().len(), 2, "no summary call was spent");
}

#[test]
fn a_crossing_that_stays_over_compacts_at_the_first_foldable_round_trip() {
    // Rider 2's pin, and the shape the live 4096-window smoke run hit: the
    // threshold is crossed on round-trip 1 and never goes back under.
    //
    // Round-trips 1 and 2 cannot fold (K+1 = 3 needed), so they are silent
    // and spend nothing. Round-trip 3 can, so it announces and compacts.
    // Under rider 1 the crossing at round-trip 1 consumed the latch and
    // NOTHING ever compacted: the session was locked out for good.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_auto_compact(
        dir.path(),
        vec![
            rt(1, 800),
            rt(2, 800),
            rt(3, 800),
            summary_response("STATE SO FAR"),
            msg_with_usage(
                vec![text("done")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 100, "output_tokens": 0}),
            ),
        ],
        Some(1000),
        100,
        true,
    );
    let events = collect_events(&mut session, "the task");
    let n = notices(&events);

    // Exactly one announcement, and it is the third round-trip's.
    assert_eq!(
        n.iter().filter(|x| x.ends_with("compacting automatically")).count(),
        1,
        "one announcement, at the first foldable crossing: {n:?}"
    );
    assert_eq!(n[0], "context: ~800 of 1000 tokens used; compacting automatically");
    assert!(n[1].starts_with("compacted: "), "and it compacted: {n:?}");
    assert!(
        !n.iter().any(|x| x.contains("/compact frees the window")),
        "the advisory never fires on a path that compacts: {n:?}"
    );

    let reqs = requests.borrow();
    // Requests 1-3 are the round-trips; request 4 IS the summary call, so
    // the compaction lands before the turn's next real request, 5.
    assert_eq!(reqs.len(), 5);
    assert!(reqs[3].tools.is_empty(), "request 4 is the summary call");
    assert_eq!(
        reqs[4].messages.len(),
        7,
        "request 5 runs on the compacted history: prompt + summary pair + 2 round-trips"
    );
    assert_eq!(texts_of(&reqs[4].messages[0]), vec!["the task"], "prompt verbatim");
}

#[test]
fn a_second_compaction_in_one_turn_keeps_the_prompt_and_leaves_no_orphan() {
    // F1, from the v0.29.0 code review. `turn_start` was captured once and
    // never moved, but a fold rewrites history as [prompt, summary, resume,
    // tail] and puts the prompt at 0. A turn that does NOT start at the top
    // of history - any later REPL turn, and every `--continue -p`, which is
    // the invocation auto-compaction exists for - therefore handed the
    // SECOND fold a stale index: it kept a tool_result as "the prompt",
    // dropped the real prompt, and left history opening with an orphan
    // tool_result, which is a 400 on both wires and is written to the
    // session file by the P2 saves.
    //
    // Nothing caught it because every other auto-compaction test starts
    // from an empty history, where the stale index is 0 and therefore
    // right by accident.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = resumed_auto_compact(
        dir.path(),
        seeded_history(), // six messages of a completed earlier turn
        None,             // no restored estimate: the seam has nothing to do
        vec![
            rt(1, 100),
            rt(2, 100),
            rt(3, 800), // crossing 1: the first foldable round-trip
            summary_response("FIRST FOLD"),
            rt(4, 100),
            rt(5, 100),
            rt(6, 800), // crossing 2, on the already-folded history
            summary_response("SECOND FOLD"),
            msg_with_usage(
                vec![text("done")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 100, "output_tokens": 0}),
            ),
        ],
        Some(1000),
        100,
        true,
    );
    let n = notices(&collect_events(&mut session, "the real task"));
    assert_eq!(
        n.iter().filter(|x| x.starts_with("compacted: ")).count(),
        2,
        "two folds inside one turn: {n:?}"
    );
    assert_eq!(requests.borrow().len(), 9, "7 round-trips plus 2 summary calls");

    let h = session.history();
    assert_eq!(h[0].role, Role::User, "history opens with the user prompt");
    assert_eq!(
        texts_of(&h[0]),
        vec!["the real task"],
        "invariant (a): the prompt survives BOTH folds, verbatim and alone"
    );
    // No orphans: every result answers a call that is still in history, and
    // still earlier than it.
    for (i, m) in h.iter().enumerate() {
        for b in &m.content {
            if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                assert!(
                    h[..i]
                        .iter()
                        .flat_map(|earlier| earlier.content.iter())
                        .any(|c| matches!(c, ContentBlock::ToolUse { id, .. } if id == tool_use_id)),
                    "orphan tool_result {tool_use_id} at message {i}: {h:?}"
                );
            }
        }
    }
}

#[test]
fn the_outcome_line_reports_round_trips_and_bytes_not_message_counts() {
    // Why the wording changed: this fold replaces 4 round-trips of tool
    // output with a summary, and the old line called it "9 into 7".
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_auto_compact(
        dir.path(),
        vec![
            rt(1, 100),
            rt(2, 100),
            rt(3, 100),
            rt(4, 100),
            rt(5, 100),
            rt(6, 800),
            summary_response("S"),
            msg_with_usage(
                vec![text("done")],
                StopReason::EndTurn,
                serde_json::json!({"input_tokens": 100, "output_tokens": 0}),
            ),
        ],
        Some(1000),
        100,
        true,
    );
    let n = notices(&collect_events(&mut session, "the task"));
    let outcome = n.iter().find(|x| x.starts_with("compacted: ")).unwrap();
    assert!(
        outcome.starts_with("compacted: 4 round-trip(s) summarized, 2 kept, ~"),
        "{outcome}"
    );
    let (before_b, after_b) = parse_compaction_bytes(outcome);
    assert!(after_b < before_b, "a real fold shrinks it: {outcome}");
    assert!(before_b > 0 && after_b > 0, "both figures are real: {outcome}");
}

// ---- T42: context overflow recovery ----

/// A canned llama.cpp: it answers from the script, EXCEPT that a request
/// whose prompt is larger than the server's window comes back as HTTP 400
/// `exceed_context_size_error`, which is the wire shape every desktop
/// experiment 5 CTX primary died on.
///
/// Size is measured as message-content chars plus, when the request carries
/// tool definitions at all, a fixed `tool_overhead`. That one asymmetry is
/// real and it matters here: the summary call omits tools entirely, so it
/// pays no floor.
struct OverflowProvider {
    outcomes: RefCell<Vec<Result<ResponseMessage, ProviderError>>>,
    requests: Rc<RefCell<Vec<ChatRequest>>>,
    /// `None` = no size gate at all; the script is the only source of errors.
    limit: Option<usize>,
    tool_overhead: usize,
    /// T44: set the session's cancel token when the SUMMARY call arrives (a
    /// summary call is the only request that carries no tools), modelling an
    /// Esc landing while the recovery fold is in flight.
    cancel_on_summary: bool,
}

impl Provider for OverflowProvider {
    fn stream(
        &self,
        req: &ChatRequest,
        _on_event: &mut dyn FnMut(StreamEvent),
        _cancel: &CancelToken,
    ) -> Result<ResponseMessage, ProviderError> {
        self.requests.borrow_mut().push(req.clone());
        if self.cancel_on_summary && req.tools.is_empty() {
            _cancel.set();
        }
        if let Some(limit) = self.limit {
            let content: usize = req
                .messages
                .iter()
                .flat_map(|m| m.content.iter())
                .map(|b| match b {
                    ContentBlock::Text { text } => text.chars().count(),
                    ContentBlock::ToolResult { content, .. } => content.chars().count(),
                    _ => 0,
                })
                .sum();
            let floor = if req.tools.is_empty() { 0 } else { self.tool_overhead };
            let size = content + floor;
            if size > limit {
                return Err(ProviderError::Api {
                    status: 400,
                    kind: "exceed_context_size_error".into(),
                    message: format!(
                        "request ({size} tokens) exceeds the available context size ({limit} tokens), try increasing it"
                    ),
                });
            }
        }
        self.outcomes.borrow_mut().remove(0)
    }
}

fn session_overflow(
    dir: &std::path::Path,
    outcomes: Vec<Result<ResponseMessage, ProviderError>>,
    limit: Option<usize>,
    tool_overhead: usize,
    context_window: Option<u64>,
    auto_compact: bool,
) -> (Session, Rc<RefCell<Vec<ChatRequest>>>) {
    overflow_session(
        dir,
        None,
        outcomes,
        limit,
        tool_overhead,
        context_window,
        auto_compact,
        false,
    )
}

/// The full form behind [`session_overflow`]: `seed` resumes from an earlier
/// session (so the turn does NOT start at index 0, which is the only place
/// the F1 `turn_start` discipline is observable), and `cancel_on_summary`
/// interrupts the summary call.
fn overflow_session(
    dir: &std::path::Path,
    seed: Option<Vec<RequestMessage>>,
    outcomes: Vec<Result<ResponseMessage, ProviderError>>,
    limit: Option<usize>,
    tool_overhead: usize,
    context_window: Option<u64>,
    auto_compact: bool,
    cancel_on_summary: bool,
) -> (Session, Rc<RefCell<Vec<ChatRequest>>>) {
    let requests = Rc::new(RefCell::new(vec![]));
    let provider = OverflowProvider {
        outcomes: RefCell::new(outcomes),
        requests: requests.clone(),
        limit,
        tool_overhead,
        cancel_on_summary,
    };
    let cfg = SessionConfig {
        model: "local-test".into(),
        max_tokens: 100,
        system: None,
        thinking: false,
        cwd: dir.to_path_buf(),
        max_iterations: 50,
        temperature: None,
        top_p: None,
        // `None` in most of these: with no window the crossing check can
        // never fire, so what they exercise is the reactive path alone.
        context_window,
        max_tokens_source: None,
        prose_tool_calls: true,
        cost_rates: None,
        cost_advisory_step_usd: temur::config::DEFAULT_COST_ADVISORY_STEP_USD,
        auto_compact,
    };
    let session = match seed {
        Some(history) => {
            let (seed, _) = store::prepare_seed(saved(history, vec![]));
            Session::resume(Box::new(provider), Registry::standard(), cfg, seed)
        }
        None => Session::new(Box::new(provider), Registry::standard(), cfg),
    };
    (session, requests)
}

const OVERFLOW_COMPACT_MARKER: &str =
    "context overflow: the server rejected the request; compacting and retrying";
const OVERFLOW_ELIDE_MARKER: &str =
    "context overflow: the server rejected the request; truncating the largest tool result and retrying";

/// A file of `lines` x 100 chars, and the round-trip that cats it back as
/// one tool result of exactly `lines * 100` chars.
fn cat_rt(dir: &std::path::Path, name: &str, lines: usize, used: u64) -> ResponseMessage {
    let body = format!("{}\n", "x".repeat(99)).repeat(lines);
    std::fs::write(dir.join(name), body).unwrap();
    msg_with_usage(
        vec![tool_use(
            &format!("tu_{name}"),
            "bash",
            serde_json::json!({"command": format!("cat {name}")}),
        )],
        StopReason::ToolUse,
        serde_json::json!({"input_tokens": used, "output_tokens": 0}),
    )
}

fn big_file(dir: &std::path::Path, name: &str, lines: usize) -> ResponseMessage {
    cat_rt(dir, name, lines, 10)
}

fn done() -> ResponseMessage {
    msg(vec![text("done")], StopReason::EndTurn)
}

#[test]
fn overflow_on_round_trip_one_halves_the_largest_result_and_retries() {
    // The gcode shape: ONE capped tool result, on the first round-trip,
    // is the whole problem. Nothing to fold, and folding would keep it in
    // the verbatim tail anyway.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_overflow(
        dir.path(),
        vec![Ok(big_file(dir.path(), "big.txt", 200)), Ok(done())],
        Some(16_000),
        2_000,
        None,
        true, // auto-compaction ON: arm (b) is still the only reachable arm
    );
    let events = collect_events(&mut session, "the task");
    let n = notices(&events);
    assert!(n.iter().any(|x| x == OVERFLOW_ELIDE_MARKER), "{n:?}");
    assert!(
        !n.iter().any(|x| x == OVERFLOW_COMPACT_MARKER),
        "one round-trip is not foldable: {n:?}"
    );
    assert!(
        n.iter().any(|x| x.starts_with("truncated the largest tool result: 20000 -> ")),
        "{n:?}"
    );
    // Sent, rejected, retried: exactly one recovery, exactly one retry.
    assert_eq!(requests.borrow().len(), 3);
    let results = tool_results(session.history());
    assert_eq!(results.len(), 1);
    let cut = results[0].chars().count();
    assert!(cut > 10_000 && cut < 10_600, "halved plus the marker: {cut}");
    assert!(results[0].contains("(truncated again:"), "the model is told why");
    // The turn finished on the retry rather than dying.
    assert!(
        matches!(events.last(), Some(AgentEvent::TurnComplete { .. })),
        "{:?}",
        events.last()
    );
}

#[test]
fn overflow_on_a_foldable_turn_compacts_and_retries() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_overflow(
        dir.path(),
        vec![
            Ok(big_file(dir.path(), "a.txt", 50)),
            Ok(big_file(dir.path(), "b.txt", 50)),
            Ok(big_file(dir.path(), "c.txt", 50)),
            Ok(summary_response("WORK SO FAR")),
            Ok(done()),
        ],
        Some(16_000),
        2_000,
        None,
        true,
    );
    let n = notices(&collect_events(&mut session, "the task"));
    assert!(n.iter().any(|x| x == OVERFLOW_COMPACT_MARKER), "{n:?}");
    assert!(
        !n.iter().any(|x| x == OVERFLOW_ELIDE_MARKER),
        "arm (a) succeeded, so arm (b) never ran: {n:?}"
    );
    assert!(
        n.iter().any(|x| x.starts_with("compacted: 1 round-trip(s) summarized, 2 kept, ~")),
        "{n:?}"
    );
    // 3 turn round-trips, the rejected send, the summary call, the retry.
    assert_eq!(requests.borrow().len(), 6);
    // The summary call carried no tools, and the retry went out on the
    // rewritten history: prompt, summary, resume, two kept round-trips.
    let reqs = requests.borrow();
    assert!(reqs[4].tools.is_empty(), "the summary call omits tools");
    assert_eq!(reqs[5].messages.len(), 7);
    // The oldest result was folded away; the two most recent survive.
    assert_eq!(tool_results(&reqs[5].messages).len(), 2);
}

#[test]
fn overflow_recovery_respects_the_per_turn_bound() {
    let dir = tempfile::tempdir().unwrap();
    // The T40 shape: three crossings compact, the fourth advises, and the
    // bound is spent. The send after that overflows and nothing may act.
    let (mut session, requests) = session_overflow(
        dir.path(),
        vec![
            Ok(rt(1, 100)),
            Ok(rt(2, 100)),
            Ok(rt(3, 800)),
            Ok(summary_response("S1")),
            Ok(rt(4, 800)),
            Ok(summary_response("S2")),
            Ok(rt(5, 800)),
            Ok(summary_response("S3")),
            Ok(rt(6, 800)),
            Err(ProviderError::Api {
                status: 400,
                kind: "exceed_context_size_error".into(),
                message: "request (13402 tokens) exceeds the available context size (12288 tokens)"
                    .into(),
            }),
        ],
        None,
        0,
        // A window IS needed here: the bound is spent by real crossings.
        Some(1000),
        true,
    );
    let mut events = vec![];
    let err = session.turn("the task", &mut |e| events.push(e)).unwrap_err();
    assert!(
        err.to_string().contains("exceed_context_size_error"),
        "the ORIGINAL error propagates: {err}"
    );
    let n = notices(&events);
    assert!(
        !n.iter().any(|x| x == OVERFLOW_COMPACT_MARKER || x == OVERFLOW_ELIDE_MARKER),
        "at the bound nothing recovers: {n:?}"
    );
    // No retry was sent.
    assert_eq!(requests.borrow().len(), 10);
}

#[test]
fn a_different_api_error_is_untouched_by_overflow_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_overflow(
        dir.path(),
        vec![
            Ok(big_file(dir.path(), "a.txt", 50)),
            Err(ProviderError::Api {
                status: 400,
                kind: "invalid_request_error".into(),
                message: "messages: unexpected role".into(),
            }),
        ],
        None,
        0,
        None,
        true,
    );
    let mut events = vec![];
    let err = session.turn("the task", &mut |e| events.push(e)).unwrap_err();
    assert!(err.to_string().contains("invalid_request_error"), "{err}");
    let n = notices(&events);
    assert!(
        !n.iter().any(|x| x == OVERFLOW_COMPACT_MARKER || x == OVERFLOW_ELIDE_MARKER),
        "{n:?}"
    );
    assert_eq!(requests.borrow().len(), 2, "no retry");
    // History untouched: the one result is still whole.
    assert_eq!(tool_results(session.history())[0].chars().count(), 5_000);
}

#[test]
fn an_overflow_that_survives_recovery_propagates_without_looping() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_overflow(
        dir.path(),
        vec![Ok(big_file(dir.path(), "big.txt", 200)), Ok(done())],
        Some(11_000), // half of the result still does not fit
        2_000,
        None,
        true,
    );
    let mut events = vec![];
    let err = session.turn("the task", &mut |e| events.push(e)).unwrap_err();
    assert!(err.to_string().contains("exceed_context_size_error"), "{err}");
    let n = notices(&events);
    assert_eq!(
        n.iter().filter(|x| *x == OVERFLOW_ELIDE_MARKER).count(),
        1,
        "exactly one recovery ran: {n:?}"
    );
    assert_eq!(requests.borrow().len(), 3, "one send, one retry, no loop");
}

#[test]
fn overflow_recovery_declines_when_every_result_is_already_small() {
    let dir = tempfile::tempdir().unwrap();
    // 501 chars: under the 1,024 floor, so halving it would destroy the
    // last of its meaning and still not be what filled the window.
    let (mut session, requests) = session_overflow(
        dir.path(),
        vec![Ok(big_file(dir.path(), "small.txt", 5)), Ok(done())],
        Some(100),
        0,
        None,
        true,
    );
    let mut events = vec![];
    let err = session.turn("the task", &mut |e| events.push(e)).unwrap_err();
    assert!(err.to_string().contains("exceed_context_size_error"), "{err}");
    let n = notices(&events);
    assert!(
        !n.iter().any(|x| x == OVERFLOW_ELIDE_MARKER),
        "nothing was truncated, so nothing claimed to be: {n:?}"
    );
    assert_eq!(requests.borrow().len(), 2, "no retry");
    assert_eq!(tool_results(session.history())[0].chars().count(), 500);
}

// ---- T44: the recovery fold learns whether it worked ----

const OVERFLOW_FALLTHROUGH_MARKER: &str =
    "context overflow: the compaction freed too little; truncating the largest tool result as well";

/// The three T42/T44 trigger lines, byte for byte.
///
/// Desktop experiments grep these out of `temur.txt` and count them; a
/// reworded one silently breaks an instrument that has already published
/// numbers, so they are pinned here as literals and not derived from the
/// source. The markers used by every other test in this file are the same
/// three constants, so this test is what makes those assertions meaningful.
#[test]
fn the_overflow_markers_are_byte_stable() {
    assert_eq!(
        OVERFLOW_COMPACT_MARKER,
        "context overflow: the server rejected the request; compacting and retrying"
    );
    assert_eq!(
        OVERFLOW_ELIDE_MARKER,
        "context overflow: the server rejected the request; truncating the largest tool result and retrying"
    );
    assert_eq!(
        OVERFLOW_FALLTHROUGH_MARKER,
        "context overflow: the compaction freed too little; truncating the largest tool result as well"
    );
}

#[test]
fn a_recovery_fold_that_freed_nothing_falls_through_to_arm_b() {
    // Desktop experiment 6's arm-(a) shape, scripted end to end. An advisory
    // fold takes the turn first; the ONE round-trip it leaves behind is what
    // arm (a) then folds, for approximately zero bytes; and what actually
    // filled the window is sitting in the verbatim tail, where only arm (b)
    // can reach it. In all three live cells arm (a) reported Compacted here
    // and returned, and the retry was rejected again.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_overflow(
        dir.path(),
        vec![
            Ok(rt(1, 100)),
            Ok(rt(2, 100)),
            Ok(rt(3, 20_000)), // crosses 80%: the advisory fold takes the turn
            Ok(summary_response("S1")),
            Ok(big_file(dir.path(), "big.txt", 200)), // 20000 chars, into the tail
            Ok(summary_response("S2")),               // the recovery fold: frees ~nothing
            Ok(done()),
        ],
        Some(16_000),
        2_000,
        // Big enough that the T19 output cap (a quarter of the window at
        // ~4 chars/token, floored at 4000) leaves the 20000-char result
        // whole: capping it here would erase the thing being measured.
        Some(24_000),
        true,
    );
    let events = collect_events(&mut session, "the task");
    let n = notices(&events);

    // Both arms spoke, in order, and the fall-through named itself.
    let seq: Vec<&String> = n
        .iter()
        .filter(|x| x.starts_with("context overflow: "))
        .collect();
    assert_eq!(
        seq,
        vec![
            OVERFLOW_COMPACT_MARKER,
            OVERFLOW_FALLTHROUGH_MARKER,
            OVERFLOW_ELIDE_MARKER
        ],
        "arm (a) first, then the gate, then arm (b): {n:?}"
    );
    // The elision actually ran, on the result in the tail.
    assert!(
        n.iter().any(|x| x.starts_with("truncated the largest tool result: 20000 -> ")),
        "{n:?}"
    );

    // The fold that fell through is REPORTED honestly first, and its own
    // figures are what the gate read: below one sixteenth of `before`.
    let folds: Vec<&String> = n.iter().filter(|x| x.starts_with("compacted: ")).collect();
    assert_eq!(folds.len(), 2, "the advisory fold and the recovery fold: {n:?}");
    let (before_b, after_b) = parse_compaction_bytes(folds[1]);
    let freed = before_b.saturating_sub(after_b);
    assert!(
        freed < before_b / 16,
        "the recovery fold freed {freed} of {before_b}, which must be under the gate"
    );

    // 3 round-trips, the advisory summary call, the send after it, the
    // rejected send, the recovery summary call, the ONE retry.
    assert_eq!(requests.borrow().len(), 8, "exactly one retry");
    // Both arms acted, so the history is folded AND elided.
    let h = session.history();
    assert_eq!(texts_of(&h[0]), vec!["the task"], "prompt verbatim through both");
    let results = tool_results(h);
    let largest = results.iter().map(|r| r.chars().count()).max().unwrap();
    assert!(largest > 10_000 && largest < 10_600, "halved plus the marker: {largest}");
    assert!(
        matches!(events.last(), Some(AgentEvent::TurnComplete { .. })),
        "the turn finished on the retry: {:?}",
        events.last()
    );
}

#[test]
fn a_recovery_fold_that_freed_plenty_never_reaches_arm_b() {
    // The other side of the gate, and the behaviour T42 shipped: three full
    // results, one folded away, a third of the history gone. Arm (b) must
    // not run, so nothing is truncated and nothing claims to be.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_overflow(
        dir.path(),
        vec![
            Ok(big_file(dir.path(), "a.txt", 50)),
            Ok(big_file(dir.path(), "b.txt", 50)),
            Ok(big_file(dir.path(), "c.txt", 50)),
            Ok(summary_response("WORK SO FAR")),
            Ok(done()),
        ],
        Some(16_000),
        2_000,
        None,
        true,
    );
    let n = notices(&collect_events(&mut session, "the task"));
    assert!(n.iter().any(|x| x == OVERFLOW_COMPACT_MARKER), "{n:?}");
    assert!(
        !n.iter().any(|x| x == OVERFLOW_FALLTHROUGH_MARKER),
        "a fold this size is a recovery: {n:?}"
    );
    assert!(!n.iter().any(|x| x == OVERFLOW_ELIDE_MARKER), "{n:?}");
    assert!(
        !n.iter().any(|x| x.starts_with("truncated the largest tool result")),
        "nothing was elided: {n:?}"
    );
    let fold = n.iter().find(|x| x.starts_with("compacted: ")).unwrap();
    let (before_b, after_b) = parse_compaction_bytes(fold);
    assert!(
        before_b.saturating_sub(after_b) >= before_b / 16,
        "this fold cleared the gate: {fold}"
    );
    assert_eq!(requests.borrow().len(), 6, "one retry, no second recovery");
    // The two kept results are whole: arm (b) never touched them.
    for r in tool_results(session.history()) {
        assert!(!r.contains("(truncated again:"), "nothing was halved");
    }
}

#[test]
fn a_fall_through_still_moves_turn_start_to_zero() {
    // F1, on the T44 path. The fall-through REPORTS an elision but it also
    // folded, so the caller owes it the same `turn_start = 0` reset arm (a)
    // gets: the fold rewrote history as [prompt, summary, resume, tail] and
    // put this turn's prompt at index 0. With the stale index a later fold
    // in the same turn keeps a tool_result as "the prompt" and drops the
    // real one - and here it would not even fire, because the stale index
    // makes the turn look too short to fold.
    //
    // Resumed, because a turn starting at index 0 is right by accident.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = overflow_session(
        dir.path(),
        Some(seeded_history()), // six messages of a completed earlier turn
        vec![
            Ok(rt(1, 100)),
            Ok(rt(2, 100)),
            Ok(cat_rt(dir.path(), "big.txt", 300, 100)), // 30000 chars, into the tail
            Ok(summary_response("S1")), // the recovery fold: frees ~nothing
            Ok(rt(4, 28_000)),          // crosses 80%, on the retried history
            Ok(summary_response("S2")), // the second fold, on the reset index
            Ok(done()),
        ],
        Some(24_000),
        2_000,
        Some(34_000), // over the T19 cap ceiling, so the result stays whole
        true,
        false,
    );
    let n = notices(&collect_events(&mut session, "the real task"));
    assert!(n.iter().any(|x| x == OVERFLOW_FALLTHROUGH_MARKER), "{n:?}");
    assert!(n.iter().any(|x| x == OVERFLOW_ELIDE_MARKER), "{n:?}");
    assert_eq!(
        n.iter().filter(|x| x.starts_with("compacted: ")).count(),
        2,
        "the fall-through's own fold, then a second fold on the reset index: {n:?}"
    );
    assert_eq!(requests.borrow().len(), 8);

    let h = session.history();
    assert_eq!(h[0].role, Role::User, "history opens with the user prompt");
    assert_eq!(
        texts_of(&h[0]),
        vec!["the real task"],
        "invariant (a): the prompt survives the fall-through AND the fold after it"
    );
    for (i, m) in h.iter().enumerate() {
        for b in &m.content {
            if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                assert!(
                    h[..i]
                        .iter()
                        .flat_map(|earlier| earlier.content.iter())
                        .any(|c| matches!(c, ContentBlock::ToolUse { id, .. } if id == tool_use_id)),
                    "orphan tool_result {tool_use_id} at message {i}"
                );
            }
        }
    }
}

#[test]
fn a_fall_through_whose_elision_declines_propagates_the_original_error() {
    // The fail-open contract, on the new path. Arm (a) folds and frees
    // nothing, arm (b) finds nothing above the 1,024-char floor and declines,
    // and what reaches the caller is the SERVER's error, not one temur
    // invented. The fold stays applied, exactly as the `Failed` path has
    // always left whatever it did behind.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_overflow(
        dir.path(),
        vec![
            Ok(rt(1, 10)),                                 // 2 chars: folding it frees nothing
            Ok(cat_rt(dir.path(), "a.txt", 10, 10)),       // 1000 chars: under the floor
            Ok(cat_rt(dir.path(), "b.txt", 10, 10)),       // 1000 chars: under the floor
            Ok(summary_response("S1")),
        ],
        Some(1_500),
        0,
        None,
        true,
    );
    let mut events = vec![];
    let err = session.turn("the task", &mut |e| events.push(e)).unwrap_err();
    assert!(
        err.to_string().contains("exceed_context_size_error"),
        "the ORIGINAL error propagates: {err}"
    );
    let n = notices(&events);
    assert!(n.iter().any(|x| x == OVERFLOW_FALLTHROUGH_MARKER), "{n:?}");
    assert!(
        !n.iter().any(|x| x == OVERFLOW_ELIDE_MARKER),
        "nothing was truncated, so nothing claimed to be: {n:?}"
    );
    assert_eq!(requests.borrow().len(), 5, "no retry was sent");
    // The fold stands, and neither result was cut.
    assert_eq!(texts_of(&session.history()[0]), vec!["the task"]);
    for r in tool_results(session.history()) {
        assert!(!r.contains("(truncated again:"), "nothing was halved");
    }
}

#[test]
fn a_cancel_during_the_recovery_summary_runs_no_other_arm() {
    // Unchanged by T44 and pinned because the gate sits right beside it: an
    // interrupt during the recovery's summary call returns None, so arm (b)
    // never runs, no retry goes out, and the caller's cancel check lands the
    // turn. Do not start a second recovery on top of an interrupt.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = overflow_session(
        dir.path(),
        None,
        vec![
            Ok(big_file(dir.path(), "a.txt", 50)),
            Ok(big_file(dir.path(), "b.txt", 50)),
            Ok(big_file(dir.path(), "c.txt", 50)),
            Ok(summary_response("NEVER USED")),
        ],
        Some(16_000),
        2_000,
        None,
        true,
        true, // the summary call sets the cancel token
    );
    let mut events = vec![];
    session.turn("the task", &mut |e| events.push(e)).unwrap();
    let n = notices(&events);
    assert!(n.iter().any(|x| x == OVERFLOW_COMPACT_MARKER), "{n:?}");
    assert!(
        !n.iter().any(|x| x.starts_with("compacted: ")
            || x == OVERFLOW_FALLTHROUGH_MARKER
            || x == OVERFLOW_ELIDE_MARKER),
        "nothing after the interrupted summary: {n:?}"
    );
    assert_eq!(requests.borrow().len(), 5, "3 round-trips, the rejected send, the summary");
    // History untouched by the cancelled fold, and untouched by arm (b).
    for r in tool_results(session.history()) {
        assert_eq!(r.chars().count(), 5_000, "no result was folded away or halved");
    }
}

// ---- T42 P2: the pre-send estimator fold-in ----

fn done_with(used: u64) -> ResponseMessage {
    msg_with_usage(
        vec![text("done")],
        StopReason::EndTurn,
        serde_json::json!({"input_tokens": used, "output_tokens": 0}),
    )
}

#[test]
fn a_large_result_trips_the_crossing_before_the_stale_estimate_would() {
    let dir = tempfile::tempdir().unwrap();
    // The response reported 100 of 1000 tokens, nowhere near either arm.
    // Then a 3,000-char result landed, which the post-response check will
    // not see until the round-trip AFTER the one that is about to go out.
    let (mut session, _) = session_auto_compact(
        dir.path(),
        vec![cat_rt(dir.path(), "big.txt", 30, 100), done_with(100)],
        Some(1000),
        100,
        false,
    );
    let n = notices(&collect_events(&mut session, "the task"));
    assert!(
        n.iter().any(|x| x
            == "context: ~850 of 1000 tokens used; /compact frees the window by summarizing the conversation, or start a new session"),
        "100 reported + 3000 chars/4 pending = 850: {n:?}"
    );
}

#[test]
fn a_small_result_does_not_trip_the_pre_send_check() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _) = session_auto_compact(
        dir.path(),
        vec![cat_rt(dir.path(), "small.txt", 4, 100), done_with(100)],
        Some(1000),
        100,
        false,
    );
    let n = notices(&collect_events(&mut session, "the task"));
    assert!(
        !n.iter().any(|x| x.starts_with("context: ")),
        "100 + 400/4 = 200 is not a crossing: {n:?}"
    );
}

#[test]
fn one_crossing_gets_one_speaker_across_both_check_sites() {
    let dir = tempfile::tempdir().unwrap();
    // The post-response check fires first (800 of 1000), and the big result
    // that lands right after would cross on its own. The shared latch means
    // the pre-send site says nothing.
    let (mut session, _) = session_auto_compact(
        dir.path(),
        vec![cat_rt(dir.path(), "big.txt", 30, 800), done_with(800)],
        Some(1000),
        100,
        false,
    );
    let n = notices(&collect_events(&mut session, "the task"));
    let spoken: Vec<&String> = n.iter().filter(|x| x.starts_with("context: ")).collect();
    assert_eq!(spoken.len(), 1, "exactly one speaker: {n:?}");
    assert!(spoken[0].starts_with("context: ~800 of 1000"), "{:?}", spoken[0]);
}

#[test]
fn the_pre_send_check_compacts_when_the_turn_is_foldable() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_auto_compact(
        dir.path(),
        vec![
            rt(1, 100),
            rt(2, 100),
            cat_rt(dir.path(), "big.txt", 30, 100),
            summary_response("WORK SO FAR"),
            done_with(100),
        ],
        Some(1000),
        100,
        true,
    );
    let n = notices(&collect_events(&mut session, "the task"));
    assert!(
        n.iter().any(|x| x == "context: ~850 of 1000 tokens used; compacting automatically"),
        "{n:?}"
    );
    assert!(
        n.iter().any(|x| x.starts_with("compacted: 1 round-trip(s) summarized, 2 kept, ~")),
        "{n:?}"
    );
    // 3 turn round-trips, the summary call, the send after the fold.
    assert_eq!(requests.borrow().len(), 5);
}

// ---- T42 P3: the auto path's summary call is bounded ----

#[test]
fn the_auto_summary_call_excludes_the_tail_it_is_about_to_keep() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_auto_compact(
        dir.path(),
        vec![
            rt(1, 100),
            rt(2, 100),
            rt(3, 100),
            rt(4, 800), // crosses
            summary_response("WORK SO FAR"),
            rt(5, 100),
            done_with(100),
        ],
        Some(1000),
        100,
        true,
    );
    let n = notices(&collect_events(&mut session, "the task"));
    // Outcome wording is unchanged by the bound.
    assert!(
        n.iter().any(|x| x.starts_with("compacted: 2 round-trip(s) summarized, 2 kept, ~")),
        "{n:?}"
    );

    let reqs = requests.borrow();
    let sreq = &reqs[4];
    assert!(sreq.tools.is_empty(), "the summary call still omits tools");
    // History at the safe point is 9 messages and the tail begins at 5, so
    // the summary call carries [prompt, a1, r1, a2, r2] and nothing else.
    // The two kept round-trips survive verbatim in the result and do not
    // need describing to the summarizer.
    assert_eq!(sreq.messages.len(), 5, "{:?}", sreq.messages.len());
    assert_eq!(
        tool_results(&sreq.messages).len(),
        2,
        "the tail's results are absent from the summary call"
    );
    // Alternation, pinned rather than argued: the pre-tail ends with a user
    // message, so the instruction JOINS it instead of opening a second
    // consecutive user message, which both wires reject.
    let last = sreq.messages.last().unwrap();
    assert_eq!(last.role, Role::User);
    assert!(
        texts_of(last).iter().any(|t| t.contains("The middle of this conversation")),
        "{:?}",
        texts_of(last)
    );
    assert!(
        sreq.messages.windows(2).all(|w| w[0].role != w[1].role),
        "no two consecutive messages share a role"
    );
}

#[test]
fn manual_compact_still_summarizes_the_whole_history() {
    // T42 P3 regression pin. `/compact` is fail-closed and discards
    // everything but the summary, so bounding ITS call would silently throw
    // the conversation away. The bound is the auto path's alone.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(vec![text("answer one")], StopReason::EndTurn),
            msg(vec![text("answer two")], StopReason::EndTurn),
            msg(vec![text("Goal: x\nState: y")], StopReason::EndTurn),
        ],
    );
    collect_events(&mut session, "first question");
    collect_events(&mut session, "second question");
    assert_eq!(session.history().len(), 4);
    assert!(matches!(session.compact(), CompactOutcome::Compacted { .. }));

    let reqs = requests.borrow();
    let sreq = &reqs[2];
    assert_eq!(sreq.messages.len(), 5, "all 4 messages plus the instruction");
    let last = sreq.messages.last().unwrap();
    assert_eq!(last.role, Role::User);
    assert!(
        texts_of(last).iter().any(|t| t.contains("Summarize this conversation")),
        "the MANUAL instruction, not the auto one"
    );
}
