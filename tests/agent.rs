//! M4 agent-loop tests against a scripted MockProvider. Real tools run in a
//! temp dir; the provider is fully scripted — no network.

use temur::agent::events::AgentEvent;
use temur::agent::{Session, SessionConfig, INTERRUPT_MARKER};
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
    assert!(n1[0].contains("context: ~250 of 1000 tokens used"));
    assert!(n1[0].contains("the next response may not fit (max_tokens 800)"));
    assert!(n1[0].contains("consider starting a new session"));
    // Turn two re-satisfies the condition; the warning is once per SESSION.
    let n2 = notices(&collect_events(&mut session, "two"));
    assert!(n2.is_empty(), "no repeat warning: {n2:?}");
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
        "qwen3-1.7b".into(),
        1024,
        None,
        Some("local".into()),
    );
    let events = collect_events(&mut session, "hi");
    assert!(notices(&events)
        .iter()
        .any(|n| n == "response truncated: max_tokens (1024, from profile \"local\") reached; raise max_tokens in config.json"),
        "{events:?}");
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
    session.switch_provider(Box::new(provider_b), "model-b".into(), 512, Some(9_999), None);
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
        Box::new(RecordingTransport {
            fixture: "text_simple",
            urls: urls.clone(),
            bodies: bodies.clone(),
        }),
    );
    session.switch_provider(Box::new(compat), "qwen-sw".into(), 1024, None, None);
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
    list: Box<dyn Fn(&ResolvedProfile) -> Result<Vec<String>, temur::error::Error>>,
    /// Mirrors main's cached_model_ids local (T16): the last `/models`
    /// listing, empty until a test fills it.
    cached_model_ids: Vec<String>,
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
            cached_model_ids: Vec::new(),
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
            cached_model_ids: &self.cached_model_ids,
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
        Ok(vec!["m-1".into(), "m-2".into()])
    });
    let events = commands::run(commands::parse("/models"), &mut h.ctx(&mut session, &build));
    assert_eq!(
        events,
        vec![AgentEvent::ModelsListed(vec!["m-1".into(), "m-2".into()])]
    );

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
    h.cached_model_ids = vec!["served-a".into(), "served-b".into()];
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
    h.cached_model_ids = vec!["served-a".into()];
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
    h.cached_model_ids = vec!["claude-opus-5".into()];
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
