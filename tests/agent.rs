//! M4 agent-loop tests against a scripted MockProvider. Real tools run in a
//! temp dir; the provider is fully scripted — no network.

use temur::agent::events::AgentEvent;
use temur::agent::{Session, SessionConfig};
use temur::provider::anthropic::types::StopDetails;
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
            assert_eq!(turn_usage.input_tokens, 10);
            assert_eq!(session_usage.output_tokens, 5);
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
fn iteration_limit_stops_runaway_turns() {
    let dir = tempfile::tempdir().unwrap();
    // Alternate two different calls so the doom-loop guard never fires.
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
