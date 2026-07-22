//! M4 agent-loop tests against a scripted MockProvider. Real tools run in a
//! temp dir; the provider is fully scripted — no network.

use temur::agent::events::AgentEvent;
use temur::agent::{Session, SessionConfig};
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
fn max_tokens_without_window_keeps_exact_old_wording() {
    let dir = tempfile::tempdir().unwrap();
    // Regression pin: no configured window → byte-identical v1 notice.
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
        .any(|n| n == "response truncated: max_tokens reached"));
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
/// turn returns Ok, the "turn interrupted" notice fires, TurnComplete is
/// last, and the landed history is wire-valid.
fn run_interrupted(session: &mut Session, input: &str) -> Vec<AgentEvent> {
    let mut events = vec![];
    session.turn(input, &mut |e| events.push(e)).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Notice(n) if n == "turn interrupted")),
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
            assert_eq!(content, "[interrupted by user]");
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
            ("t1", "[interrupted by user]"),
            ("t2", "[interrupted by user]"),
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
    // D4: Err + token set is the user's interrupt, not a provider failure —
    // turn returns Ok and no "provider error" surfaces.
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
fn stale_token_is_cleared_at_turn_entry() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = interrupt_session(
        dir.path(),
        vec![(false, Ok(msg(vec![text("Hi there")], StopReason::EndTurn)))],
    );
    // Esc landed after the previous turn already finished.
    session.cancel_token().set();

    let mut events = vec![];
    session.turn("hello", &mut |e| events.push(e)).unwrap();

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Notice(n) if n == "turn interrupted")),
        "a stale token must not cancel the next turn: {events:?}"
    );
    assert_eq!(session.history().len(), 2, "normal completion");
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
            assert_eq!(c2, "[interrupted by user]", "pending call synthesized");
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

    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn main() {\n    let y = 2;\n}\n"
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
