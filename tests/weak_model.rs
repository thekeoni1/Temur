//! T4 weak-model hardening tests: scripted fixture responses that ARE the
//! misbehaviors small local models actually produce — malformed/truncated
//! tool arguments, hallucinated tool names, alternating loops, empty
//! responses, tool calls written as prose — asserting the loop degrades
//! politely. Offline, no network; real tools run in a temp dir.

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
    ) -> Result<ResponseMessage, ProviderError> {
        self.requests.borrow_mut().push(req.clone());
        Ok(self.responses.borrow_mut().remove(0))
    }
}

fn msg(content: Vec<ContentBlock>, stop: StopReason) -> ResponseMessage {
    let value = serde_json::json!({
        "id": "msg_test",
        "model": "local-weak",
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

/// A call whose wire arguments failed to parse: input {} + the raw string,
/// exactly as both providers now deliver it.
fn tool_use_raw(id: &str, name: &str, raw: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: id.into(),
        name: name.into(),
        input: serde_json::json!({}),
        input_raw: Some(raw.into()),
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
        model: "local-weak".into(),
        max_tokens: 8_000,
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
fn lossless_repair_executes_and_writes_file() {
    // Fenced-but-valid JSON arguments: repaired losslessly, executed for
    // real (the file lands on disk), with a repair Notice.
    let dir = tempfile::tempdir().unwrap();
    let raw = "```json\n{\"filePath\": \"out.txt\", \"content\": \"repaired!\"}\n```";
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(vec![tool_use_raw("tu_1", "write", raw)], StopReason::ToolUse),
            msg(vec![text("done")], StopReason::EndTurn),
        ],
    );
    let events = collect_events(&mut session, "write the file");

    assert_eq!(requests.borrow().len(), 2);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("out.txt")).unwrap(),
        "repaired!"
    );
    assert!(notices(&events).iter().any(|n| n.contains("repaired")));
    // The result fed back is a success, not an error.
    let reqs = requests.borrow();
    match &reqs[1].messages.last().unwrap().content[0] {
        ContentBlock::ToolResult { is_error, .. } => assert!(!is_error),
        other => panic!("expected tool_result, got {other:?}"),
    }
}

#[test]
fn lossy_truncation_is_never_executed() {
    // Truncated arguments COULD be completed into schema-valid JSON — but a
    // completed truncation is semantically wrong (a silent wrong write), so
    // the call must not run: no file, is_error feedback instead.
    let dir = tempfile::tempdir().unwrap();
    let raw = "{\"filePath\": \"loss.txt\", \"content\": \"abc";
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(vec![tool_use_raw("tu_1", "write", raw)], StopReason::ToolUse),
            msg(vec![text("understood")], StopReason::EndTurn),
        ],
    );
    let events = collect_events(&mut session, "write the file");

    assert!(!dir.path().join("loss.txt").exists(), "lossy repair must not execute");
    match &requests.borrow()[1].messages.last().unwrap().content[0] {
        ContentBlock::ToolResult { is_error, content, .. } => {
            assert!(is_error);
            assert!(content.contains("NOT executed"));
        }
        other => panic!("expected tool_result, got {other:?}"),
    }
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolEnd { is_error: true, .. })));
}

#[test]
fn unrepairable_args_feed_error_then_scripted_retry_succeeds() {
    // Unrepairable JSON (missing colon): the error result echoes the raw
    // string and asks for a re-issue; the scripted correct retry succeeds.
    let dir = tempfile::tempdir().unwrap();
    let raw = "{\"filePath\" \"fixed.txt\"}";
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(vec![tool_use_raw("tu_1", "write", raw)], StopReason::ToolUse),
            msg(
                vec![tool_use(
                    "tu_2",
                    "write",
                    serde_json::json!({"filePath": "fixed.txt", "content": "second try"}),
                )],
                StopReason::ToolUse,
            ),
            msg(vec![text("done")], StopReason::EndTurn),
        ],
    );
    collect_events(&mut session, "write the file");

    assert_eq!(requests.borrow().len(), 3);
    match &requests.borrow()[1].messages.last().unwrap().content[0] {
        ContentBlock::ToolResult { is_error, content, .. } => {
            assert!(is_error);
            assert!(content.contains("NOT executed"));
            assert!(content.contains(raw), "raw arguments echoed back");
            assert!(content.contains("valid JSON"));
        }
        other => panic!("expected tool_result, got {other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("fixed.txt")).unwrap(),
        "second try"
    );
}

#[test]
fn hallucinated_tool_name_fed_back_then_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(
                vec![tool_use("tu_1", "compile", serde_json::json!({"target": "all"}))],
                StopReason::ToolUse,
            ),
            msg(
                vec![tool_use(
                    "tu_2",
                    "write",
                    serde_json::json!({"filePath": "real.txt", "content": "recovered"}),
                )],
                StopReason::ToolUse,
            ),
            msg(vec![text("done")], StopReason::EndTurn),
        ],
    );
    collect_events(&mut session, "build it");

    assert_eq!(requests.borrow().len(), 3);
    match &requests.borrow()[1].messages.last().unwrap().content[0] {
        ContentBlock::ToolResult { is_error, content, .. } => {
            assert!(is_error);
            assert!(content.contains("unknown tool: compile"));
        }
        other => panic!("expected tool_result, got {other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("real.txt")).unwrap(),
        "recovered"
    );
}

#[test]
fn alternating_pair_trips_guard_at_six_requests() {
    let dir = tempfile::tempdir().unwrap();
    let call = |cmd: &str| {
        msg(
            vec![tool_use("tu_x", "bash", serde_json::json!({"command": cmd}))],
            StopReason::ToolUse,
        )
    };
    // Exactly 6 scripted responses: a 7th request would panic the mock,
    // so the request count is structurally pinned as well as asserted.
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            call("echo a"),
            call("echo b"),
            call("echo a"),
            call("echo b"),
            call("echo a"),
            call("echo b"),
        ],
    );
    let events = collect_events(&mut session, "loop forever");

    assert_eq!(requests.borrow().len(), 6);
    assert!(notices(&events).iter().any(|n| n.contains("alternated")));
    // The plain doom-loop guard must NOT have been the one to fire.
    assert!(!notices(&events).iter().any(|n| n.contains("repeated")));
}

#[test]
fn empty_response_loop_trips_at_three() {
    // PauseTurn resends would loop forever on a model that keeps answering
    // nothing; whitespace-only text counts as empty too.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(vec![], StopReason::PauseTurn),
            msg(vec![text("  \n ")], StopReason::PauseTurn),
            msg(vec![], StopReason::PauseTurn),
        ],
    );
    let events = collect_events(&mut session, "hello");

    assert_eq!(requests.borrow().len(), 3);
    assert!(notices(&events).iter().any(|n| n.contains("empty")));
}

#[test]
fn single_empty_end_turn_finishes_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) =
        session_with(dir.path(), vec![msg(vec![], StopReason::EndTurn)]);
    let events = collect_events(&mut session, "hello");
    assert_eq!(requests.borrow().len(), 1);
    assert!(notices(&events).is_empty(), "no guard notice on a clean empty finish");
}

#[test]
fn consecutive_failure_cap_trips_at_five_with_doom_loop_silent() {
    // Five DIFFERENT failing calls: every batch is all-error, none is
    // identical or alternating, so only the failure cap fires — at exactly
    // five requests, with the fifth batch's results still pushed.
    let dir = tempfile::tempdir().unwrap();
    let fail = |i: u32| {
        msg(
            vec![tool_use(
                &format!("tu_{i}"),
                "read",
                serde_json::json!({"wrongParam": i}),
            )],
            StopReason::ToolUse,
        )
    };
    let (mut session, requests) = session_with(
        dir.path(),
        vec![fail(0), fail(1), fail(2), fail(3), fail(4)],
    );
    let events = collect_events(&mut session, "thrash");

    assert_eq!(requests.borrow().len(), 5);
    let ns = notices(&events);
    assert!(ns.iter().any(|n| n.contains("consecutive batches")));
    assert!(!ns.iter().any(|n| n.contains("repeated")), "doom-loop must not fire");
    assert!(!ns.iter().any(|n| n.contains("alternated")));
    // History stays consistent: the fifth batch's error results were pushed
    // before stopping.
    let hist = session.history();
    match hist.last().unwrap().content.first().unwrap() {
        ContentBlock::ToolResult { is_error, .. } => assert!(is_error),
        other => panic!("expected trailing tool_result, got {other:?}"),
    }
}

#[test]
fn text_tool_call_nudged_then_recovers() {
    // A tool call written as prose: nothing executes, the model gets a
    // corrective user message, and the scripted structural retry succeeds.
    let dir = tempfile::tempdir().unwrap();
    let prose = "<tool_call>{\"name\": \"write\", \"arguments\": \
                 {\"filePath\": \"nudged.txt\", \"content\": \"x\"}}</tool_call>";
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(vec![text(prose)], StopReason::EndTurn),
            msg(
                vec![tool_use(
                    "tu_1",
                    "write",
                    serde_json::json!({"filePath": "nudged.txt", "content": "structured"}),
                )],
                StopReason::ToolUse,
            ),
            msg(vec![text("done")], StopReason::EndTurn),
        ],
    );
    let events = collect_events(&mut session, "write the file");

    assert_eq!(requests.borrow().len(), 3);
    // The prose was never executed as a tool call; the retry's write is
    // what landed.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("nudged.txt")).unwrap(),
        "structured"
    );
    assert!(notices(&events).iter().any(|n| n.contains("plain text")));
    // The corrective user message is in history, between the two assistant
    // messages.
    assert!(session.history().iter().any(|m| {
        matches!(m.role, Role::User)
            && m.content.iter().any(|b| matches!(
                b,
                ContentBlock::Text { text } if text.contains("Nothing was executed")
            ))
    }));
}

#[test]
fn nudges_capped_at_exactly_two() {
    let dir = tempfile::tempdir().unwrap();
    let prose = || {
        msg(
            vec![text("[TOOL_CALL] {\"name\": \"read\", \"arguments\": {}}")],
            StopReason::EndTurn,
        )
    };
    let (mut session, requests) =
        session_with(dir.path(), vec![prose(), prose(), prose()]);
    let events = collect_events(&mut session, "go");

    // Nudge, nudge, then the third detection is over the cap: the turn ends.
    assert_eq!(requests.borrow().len(), 3);
    let nudge_notices = notices(&events)
        .iter()
        .filter(|n| n.contains("plain text"))
        .count();
    assert_eq!(nudge_notices, 2, "exactly two nudges per turn");
}
