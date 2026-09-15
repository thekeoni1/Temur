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
        _cancel: &CancelToken,
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
        provider_state: None,
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
        provider_state: None,
    }
}

fn session_with(
    dir: &std::path::Path,
    responses: Vec<ResponseMessage>,
) -> (Session, Rc<RefCell<Vec<ChatRequest>>>) {
    session_with_prose(dir, responses, true)
}

/// T61: the same session, told that nobody is reading it. What main.rs
/// resolves from `-p` or from a piped plain REPL arrives in the core as
/// this one bool, so the tests set the bool.
fn session_unattended(
    dir: &std::path::Path,
    responses: Vec<ResponseMessage>,
) -> (Session, Rc<RefCell<Vec<ChatRequest>>>) {
    session_with_flags(dir, responses, true, true)
}

/// `prose_tool_calls` explicit: `true` is the product default (T19 P3
/// prose-call execution), `false` restores T4 detect+nudge.
fn session_with_prose(
    dir: &std::path::Path,
    responses: Vec<ResponseMessage>,
    prose_tool_calls: bool,
) -> (Session, Rc<RefCell<Vec<ChatRequest>>>) {
    session_with_flags(dir, responses, prose_tool_calls, false)
}

/// Both mode bools explicit. `unattended` is T61's: `false` is the
/// interactive arm every test above this one runs in.
fn session_with_flags(
    dir: &std::path::Path,
    responses: Vec<ResponseMessage>,
    prose_tool_calls: bool,
    unattended: bool,
) -> (Session, Rc<RefCell<Vec<ChatRequest>>>) {
    session_with_nudge(dir, responses, prose_tool_calls, unattended, true)
}

/// T64 P1: the mode bools plus the `unattended_nudge` opt-out.
fn session_with_nudge(
    dir: &std::path::Path,
    responses: Vec<ResponseMessage>,
    prose_tool_calls: bool,
    unattended: bool,
    unattended_nudge: bool,
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
        max_tokens_source: None,
        prose_tool_calls,
        cost_rates: None,
        cost_advisory_step_usd: temur::config::DEFAULT_COST_ADVISORY_STEP_USD,
        auto_compact: false,
        unattended,
        unattended_nudge,
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
    // prose_tool_calls = false: the T4 detect+nudge behavior, exactly.
    // A tool call written as prose: nothing executes, the model gets a
    // corrective user message, and the scripted structural retry succeeds.
    let dir = tempfile::tempdir().unwrap();
    let prose = "<tool_call>{\"name\": \"write\", \"arguments\": \
                 {\"filePath\": \"nudged.txt\", \"content\": \"x\"}}</tool_call>";
    let (mut session, requests) = session_with_prose(
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
        false,
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

// ------------------------------------------------- T19 P3: prose execution

#[test]
fn prose_call_executes_and_result_feeds_next_request() {
    // The default (prose_tool_calls = true): an UNAMBIGUOUS tool call
    // written as plain text executes, and its result goes back as a plain
    // user text message in the next request.
    let dir = tempfile::tempdir().unwrap();
    let prose = "{\"name\": \"write\", \"arguments\": \
                 {\"filePath\": \"prose.txt\", \"content\": \"via prose\"}}";
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(vec![text(prose)], StopReason::EndTurn),
            msg(vec![text("done")], StopReason::EndTurn),
        ],
    );
    let events = collect_events(&mut session, "write the file");

    assert_eq!(requests.borrow().len(), 2, "the result must trigger a follow-up request");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("prose.txt")).unwrap(),
        "via prose",
        "the prose call must actually execute"
    );
    assert!(
        notices(&events)
            .iter()
            .any(|n| n.contains("prose-call recovery: executed the write tool call")),
        "{:?}",
        notices(&events)
    );
    // The feedback is PLAIN USER TEXT (no tool_use id exists), in the
    // documented shape.
    let reqs = requests.borrow();
    let last = reqs[1].messages.last().unwrap();
    assert!(matches!(last.role, Role::User));
    match &last.content[..] {
        [ContentBlock::Text { text }] => {
            assert!(
                text.starts_with(
                    "Result of the write tool call you wrote as text (executed by prose-call recovery):"
                ),
                "{text}"
            );
            assert!(text.contains("prose.txt"), "{text}");
        }
        other => panic!("expected plain user text, got {other:?}"),
    }
}

#[test]
fn prose_call_failures_count_toward_nudge_cap_and_terminate() {
    // A prose call that EXECUTES but fails (write to an existing unread
    // file, the P2 rule) feeds the error back and counts toward
    // NUDGE_LIMIT, so a model stuck on a failing prose call terminates.
    //
    // T31 (H1): the calls differ by target, because a model that resends
    // one call VERBATIM now takes the repeat-guard path instead of a second
    // execution (see `identical_prose_call_is_not_executed_twice`). This
    // test is about the failure cap, so it keeps failing with fresh calls.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("locked.txt"), "original").unwrap();
    std::fs::write(dir.path().join("locked2.txt"), "original").unwrap();
    std::fs::write(dir.path().join("locked3.txt"), "original").unwrap();
    let prose = |target: &str| {
        msg(
            vec![text(&format!(
                "{{\"name\": \"write\", \"arguments\": \
                 {{\"filePath\": \"{target}\", \"content\": \"clobber\"}}}}"
            ))],
            StopReason::EndTurn,
        )
    };
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            prose("locked.txt"),
            prose("locked2.txt"),
            prose("locked3.txt"),
        ],
    );
    let events = collect_events(&mut session, "go");

    // Fail (1), fail (2 = cap), then the third prose call is over the cap:
    // the turn ends as a plain EndTurn.
    assert_eq!(requests.borrow().len(), 3);
    let failed = notices(&events)
        .iter()
        .filter(|n| n.contains("failed; fed the error back"))
        .count();
    assert_eq!(failed, 2, "exactly two failed prose executions per turn");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("locked.txt")).unwrap(),
        "original",
        "the read-first rule holds through prose recovery"
    );
    // The error feedback reached the model as plain user text.
    let reqs = requests.borrow();
    match &reqs[1].messages.last().unwrap().content[..] {
        [ContentBlock::Text { text }] => {
            assert!(
                text.starts_with(
                    "Error result of the write tool call you wrote as text (executed by prose-call recovery):"
                ),
                "{text}"
            );
            assert!(text.contains("has not been read in this session"), "{text}");
        }
        other => panic!("expected plain user text, got {other:?}"),
    }
}

#[test]
fn ambiguous_or_lossy_prose_still_nudges_never_executes() {
    // Two candidates in one message: no execution (nudge as today), even
    // with prose_tool_calls on.
    let dir = tempfile::tempdir().unwrap();
    let two = "<tool_call>{\"name\": \"write\", \"arguments\": {\"filePath\": \"a.txt\", \"content\": \"1\"}}</tool_call>\n\
               <tool_call>{\"name\": \"write\", \"arguments\": {\"filePath\": \"b.txt\", \"content\": \"2\"}}</tool_call>";
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(vec![text(two)], StopReason::EndTurn),
            msg(vec![text("ok")], StopReason::EndTurn),
        ],
    );
    let events = collect_events(&mut session, "go");
    assert_eq!(requests.borrow().len(), 2);
    assert!(!dir.path().join("a.txt").exists(), "ambiguous prose must not execute");
    assert!(!dir.path().join("b.txt").exists());
    assert!(notices(&events).iter().any(|n| n.contains("plain text")));
    drop(events);

    // Lossy (truncated) inner JSON: never executes. (detect_text_tool_call
    // cannot parse truncated JSON either, same as pre-T19, so the turn
    // ends as a plain EndTurn, no nudge.)
    let lossy = "{\"name\": \"write\", \"arguments\": {\"filePath\": \"c.txt\", \"content\": \"cut";
    let (mut session, requests) = session_with(
        dir.path(),
        vec![msg(vec![text(lossy)], StopReason::EndTurn)],
    );
    let events = collect_events(&mut session, "go");
    assert_eq!(requests.borrow().len(), 1);
    assert!(!dir.path().join("c.txt").exists(), "lossy prose must not execute");
    drop(events);
}

// ------------------------------------- T30: preamble before a fenced call

/// T29 queue finding 1, measured 2026-08-12: Qwen2.5-Coder-1.5B narrates a
/// sentence and THEN writes a fenced call. That used to be neither executed
/// (the T19 predicate demands the whole trimmed message) nor nudged (the T4
/// detector shared the same gate), so the turn ended in silence. Detection
/// widened; execution did not.
#[test]
fn preamble_then_fenced_call_nudges_and_never_executes() {
    let dir = tempfile::tempdir().unwrap();
    let preamble = "I'll create the file now.\n\n```json\n{\"name\": \"write\", \"arguments\": \
                    {\"filePath\": \"preamble.txt\", \"content\": \"x\"}}\n```";
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(vec![text(preamble)], StopReason::EndTurn),
            msg(
                vec![tool_use(
                    "tu_1",
                    "write",
                    serde_json::json!({"filePath": "preamble.txt", "content": "structured"}),
                )],
                StopReason::ToolUse,
            ),
            msg(vec![text("done")], StopReason::EndTurn),
        ],
    );
    let events = collect_events(&mut session, "write the file");

    assert_eq!(requests.borrow().len(), 3);
    // The fenced call behind preamble was NOT executed: what landed is the
    // structured retry the nudge asked for.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("preamble.txt")).unwrap(),
        "structured"
    );
    assert!(
        notices(&events).iter().any(|n| n.contains("plain text")),
        "the nudge must fire: {:?}",
        notices(&events)
    );
    assert!(
        !notices(&events).iter().any(|n| n.contains("prose-call recovery")),
        "execution must NOT widen: {:?}",
        notices(&events)
    );
}

/// The widened path is bounded by the same cap as the old one.
#[test]
fn preamble_fenced_nudges_are_capped_at_two_as_well() {
    let dir = tempfile::tempdir().unwrap();
    let preamble = || {
        msg(
            vec![text(
                "Let me look at it.\n```json\n{\"name\": \"read\", \"arguments\": {}}\n```",
            )],
            StopReason::EndTurn,
        )
    };
    let (mut session, requests) =
        session_with(dir.path(), vec![preamble(), preamble(), preamble()]);
    let events = collect_events(&mut session, "go");

    assert_eq!(requests.borrow().len(), 3);
    let nudge_notices = notices(&events)
        .iter()
        .filter(|n| n.contains("plain text"))
        .count();
    assert_eq!(nudge_notices, 2, "exactly two nudges per turn");
}

// ------------------------- T31: prose repeat guard, unknown-tool feedback

/// H1, operator dogfood 2026-08-14 (eval task 8): Qwen2.5-Coder-1.5B wrote
/// one fenced `write` call and then resent it byte for byte about sixty
/// times. Each resend was a fresh SUCCESSFUL prose-call execution, and
/// successes are uncapped, so the turn only ended when the context window
/// overflowed. The first call must still run; identical resends must not.
#[test]
fn identical_prose_call_is_not_executed_twice() {
    let dir = tempfile::tempdir().unwrap();
    // The transcript's exact shape: fenced JSON, whole message.
    let repeat = || {
        msg(
            vec![text(
                "```json\n{\"name\": \"write\", \"arguments\": \
                 {\"content\": \"eval-gz-99\", \"filePath\": \"notes.txt\"}}\n```",
            )],
            StopReason::EndTurn,
        )
    };
    let (mut session, requests) = session_with(
        dir.path(),
        vec![repeat(), repeat(), repeat(), repeat()],
    );
    let events = collect_events(&mut session, "write the file");
    let notices = notices(&events);

    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
        "eval-gz-99",
        "the FIRST call still executes"
    );
    assert_eq!(
        notices
            .iter()
            .filter(|n| n.contains("prose-call recovery: executed the write tool call"))
            .count(),
        1,
        "exactly one execution, not one per resend: {notices:?}"
    );
    assert_eq!(
        notices
            .iter()
            .filter(|n| n.contains("repeated verbatim; not executed again"))
            .count(),
        2,
        "the repeats are answered, and the answers are capped: {notices:?}"
    );
    // Resend 1 and 2 get the notice; by resend 3 the cap is reached and the
    // turn ends on a plain EndTurn instead of trading notices forever.
    assert_eq!(requests.borrow().len(), 4);
    // The notice reached the model as plain user text, honestly.
    let reqs = requests.borrow();
    match &reqs[2].messages.last().unwrap().content[..] {
        [ContentBlock::Text { text }] => {
            assert!(
                text.starts_with("You already made that exact write tool call"),
                "{text}"
            );
            assert!(text.contains("Nothing was executed this time"), "{text}");
        }
        other => panic!("expected plain user text, got {other:?}"),
    }
}

/// A DIFFERENT call resets the guard: the second write is not a repeat of
/// the first, so it executes. The guard must not stall a working model.
#[test]
fn different_prose_call_resets_the_repeat_guard() {
    let dir = tempfile::tempdir().unwrap();
    let call = |path: &str| {
        msg(
            vec![text(&format!(
                "{{\"name\": \"write\", \"arguments\": \
                 {{\"filePath\": \"{path}\", \"content\": \"x\"}}}}"
            ))],
            StopReason::EndTurn,
        )
    };
    let (mut session, _requests) = session_with(
        dir.path(),
        vec![call("one.txt"), call("two.txt"), msg(vec![text("done")], StopReason::EndTurn)],
    );
    let events = collect_events(&mut session, "write both files");

    assert!(dir.path().join("one.txt").exists(), "first call executes");
    assert!(dir.path().join("two.txt").exists(), "a changed call executes too");
    assert!(
        !notices(&events)
            .iter()
            .any(|n| n.contains("repeated verbatim")),
        "no repeat guard on distinct calls: {:?}",
        notices(&events)
    );
}

/// H3, operator dogfood 2026-08-14 (eval task 7): a fenced call to a tool
/// that does not exist matched neither the execution predicate nor the
/// detector (both require a REGISTERED name), so the turn ended in total
/// silence after 31 output tokens. It must now say so, by name, and list
/// what does exist, without ever executing anything.
#[test]
fn unknown_tool_call_is_named_and_never_executed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("obsolete.tmp"), "junk").unwrap();
    let bogus = "```json\n{\"name\": \"delete\", \"arguments\": \
                 {\"filePath\": \"obsolete.tmp\"}}\n```";
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(vec![text(bogus)], StopReason::EndTurn),
            // The correction lands: a real tool, structured.
            msg(
                vec![tool_use(
                    "tu_1",
                    "bash",
                    serde_json::json!({"command": "rm obsolete.tmp"}),
                )],
                StopReason::ToolUse,
            ),
            msg(vec![text("removed")], StopReason::EndTurn),
        ],
    );
    let events = collect_events(&mut session, "delete obsolete.tmp");

    assert_eq!(requests.borrow().len(), 3, "silence is what this fixes");
    assert!(
        notices(&events)
            .iter()
            .any(|n| n.contains("a tool that does not exist (\"delete\")")),
        "{:?}",
        notices(&events)
    );
    assert!(
        !notices(&events).iter().any(|n| n.contains("prose-call recovery")),
        "an unknown tool must NEVER execute: {:?}",
        notices(&events)
    );
    // The feedback names the bogus tool and lists the registry, in order.
    let reqs = requests.borrow();
    let registered: Vec<String> = reqs[1].tools.iter().map(|d| d.name.clone()).collect();
    match &reqs[1].messages.last().unwrap().content[..] {
        [ContentBlock::Text { text }] => {
            assert!(text.contains("There is no tool named \"delete\""), "{text}");
            assert!(text.contains(&registered.join(", ")), "{text}");
            for name in &registered {
                assert!(text.contains(name.as_str()), "{name} missing from {text}");
            }
        }
        other => panic!("expected plain user text, got {other:?}"),
    }
    // The scripted follow-up ran, so the turn recovered rather than dying.
    assert!(!dir.path().join("obsolete.tmp").exists());
}

/// The unknown-tool path is bounded by the same cap as every other nudge.
#[test]
fn unknown_tool_nudges_are_capped_at_two() {
    let dir = tempfile::tempdir().unwrap();
    let bogus = || {
        msg(
            vec![text(
                "```json\n{\"name\": \"delete\", \"arguments\": {\"filePath\": \"a\"}}\n```",
            )],
            StopReason::EndTurn,
        )
    };
    let (mut session, requests) =
        session_with(dir.path(), vec![bogus(), bogus(), bogus()]);
    let events = collect_events(&mut session, "go");

    assert_eq!(requests.borrow().len(), 3);
    assert_eq!(
        notices(&events)
            .iter()
            .filter(|n| n.contains("does not exist"))
            .count(),
        2,
        "exactly two unknown-tool nudges per turn"
    );
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

// --------------------------------------- T35 (D2): promise-then-stop nudge

#[test]
fn promised_work_without_a_call_is_nudged_then_the_model_acts() {
    // The dogfood shape, verbatim (2026-08-14, qwen3-4b): the turn ends
    // announcing analysis, having called nothing. Nothing runs between
    // turns, so without the nudge the operator waits forever.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(
                vec![text("Please wait while I analyze it")],
                StopReason::EndTurn,
            ),
            msg(
                vec![tool_use(
                    "tu_1",
                    "write",
                    serde_json::json!({"filePath": "analyzed.txt", "content": "ok"}),
                )],
                StopReason::ToolUse,
            ),
            msg(vec![text("done")], StopReason::EndTurn),
        ],
    );
    let events = collect_events(&mut session, "analyze the file");

    assert_eq!(requests.borrow().len(), 3);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("analyzed.txt")).unwrap(),
        "ok"
    );
    assert!(
        notices(&events)
            .iter()
            .any(|n| n.contains("promised work without calling a tool")),
        "{:?}",
        notices(&events)
    );
    assert!(session.history().iter().any(|m| {
        matches!(m.role, Role::User)
            && m.content.iter().any(|b| matches!(
                b,
                ContentBlock::Text { text } if text.contains("Nothing runs between turns")
            ))
    }));
}

#[test]
fn the_same_phrase_after_a_dispatched_tool_does_not_nudge() {
    // The turn DID work. A closing "please wait" then reads as prose about
    // work already done, not as a turn that stopped without starting, so
    // the turn ends where the model ended it.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(
                vec![tool_use(
                    "tu_1",
                    "write",
                    serde_json::json!({"filePath": "did.txt", "content": "work"}),
                )],
                StopReason::ToolUse,
            ),
            msg(
                vec![text("Please wait while I analyze it")],
                StopReason::EndTurn,
            ),
        ],
    );
    let events = collect_events(&mut session, "do the work");

    assert_eq!(requests.borrow().len(), 2, "no third request: no nudge");
    assert!(
        !notices(&events)
            .iter()
            .any(|n| n.contains("promised work without calling a tool")),
        "{:?}",
        notices(&events)
    );
}

#[test]
fn a_promise_phrase_followed_by_the_work_does_not_nudge() {
    // The tail rule at loop level: the phrase is present but the substance
    // comes after it, so the message is a finished answer.
    let dir = tempfile::tempdir().unwrap();
    let body = "the parser accepts every scalar shape in the matrix, and the one \
                remaining gap is the timeout knob, which is read but never enforced. "
        .repeat(3);
    let (mut session, requests) = session_with(
        dir.path(),
        vec![msg(
            vec![text(&format!("I will now summarize: {body}"))],
            StopReason::EndTurn,
        )],
    );
    let events = collect_events(&mut session, "summarize");

    assert_eq!(requests.borrow().len(), 1);
    assert!(
        !notices(&events)
            .iter()
            .any(|n| n.contains("promised work without calling a tool")),
        "{:?}",
        notices(&events)
    );
}

// ------------------------------------ T45 (P2): D12 scope-denial nudge

#[test]
fn the_d12_shape_is_nudged_and_then_answers() {
    // The dogfood shape, verbatim (2026-09-03, qwen3-4b): asked to explain
    // something it knows, the model declined as out of tool scope and
    // called nothing. The same question reworded was answered in the same
    // session, so the knowledge was there and the phrasing alone lost it.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(
                vec![text(
                    "I'm unable to explain implicit differentiation as it's outside \
                     the scope of available tools. Would you like me to assist with \
                     anything related to coding or file operations?",
                )],
                StopReason::EndTurn,
            ),
            msg(
                vec![text("Implicit differentiation differentiates both sides.")],
                StopReason::EndTurn,
            ),
        ],
    );
    let events = collect_events(&mut session, "can you explain implicit differentiation to me");

    assert_eq!(requests.borrow().len(), 2, "one nudge, one more request");
    assert!(
        notices(&events)
            .iter()
            .any(|n| n.contains("declined a question as out of tool scope")),
        "{:?}",
        notices(&events)
    );
    assert!(session.history().iter().any(|m| {
        matches!(m.role, Role::User)
            && m.content.iter().any(|b| matches!(
                b,
                ContentBlock::Text { text } if text.contains("You do not need a tool to answer that")
            ))
    }));
}

#[test]
fn a_scope_denial_after_a_dispatched_tool_does_not_nudge() {
    // The turn DID work. A closing scope sentence then reads as prose
    // about what was done, not as a refusal to start.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(
                vec![tool_use(
                    "tu_1",
                    "write",
                    serde_json::json!({"filePath": "did.txt", "content": "work"}),
                )],
                StopReason::ToolUse,
            ),
            msg(
                vec![text("The rest is outside the scope of available tools.")],
                StopReason::EndTurn,
            ),
        ],
    );
    let events = collect_events(&mut session, "do the work");

    assert_eq!(requests.borrow().len(), 2, "no third request: no nudge");
    assert!(
        !notices(&events)
            .iter()
            .any(|n| n.contains("declined a question as out of tool scope")),
        "{:?}",
        notices(&events)
    );
}

#[test]
fn a_scope_denial_phrase_followed_by_the_answer_does_not_nudge() {
    // The tail rule at loop level, copied from T35 P3: the phrase is
    // present but the answer comes after it, so the reply is finished.
    //
    // T48: the body is sized from the archive rather than from the
    // constant, for the same reason as its sibling in recover.rs. The
    // four real mid-message mentions measured in the T45 replay logs put
    // their anchors 1251 to 1427 characters from the end; `.repeat(3)`
    // put this one at 444, which only ever cleared the old 300-character
    // window. `.repeat(11)` puts it at 1452, inside the measured range.
    let dir = tempfile::tempdir().unwrap();
    let body = "differentiate both sides with respect to x, treat y as a function \
                of x, and apply the chain rule to every term containing it. "
        .repeat(11);
    let (mut session, requests) = session_with(
        dir.path(),
        vec![msg(
            vec![text(&format!(
                "Some of that is outside the scope of available tools, but here is \
                 the explanation: {body}"
            ))],
            StopReason::EndTurn,
        )],
    );
    let events = collect_events(&mut session, "explain it");

    assert_eq!(requests.borrow().len(), 1);
    assert!(
        !notices(&events)
            .iter()
            .any(|n| n.contains("declined a question as out of tool scope")),
        "{:?}",
        notices(&events)
    );
}

// ------------------------------------ T58 (P2): D25 file-denial nudge

#[test]
fn the_d25_shape_is_nudged_and_then_reads_the_file() {
    // The dogfood shape, verbatim (2026-09-09, qwen3-4b, cwd holding
    // exactly one PDF): asked for feedback on "my resume", the model
    // declared an inability and called nothing. Both prompt sentences
    // meant to prevent this were in the prompt it saw, and neither
    // addresses a declared inability.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(
                vec![text(
                    "I can't directly read or process files like a resume. However, \
                     if you'd like to share the content of your resume (or specific \
                     parts), I'd be happy to help you review, edit, or provide \
                     feedback on it. Just paste the text here!",
                )],
                StopReason::EndTurn,
            ),
            msg(
                vec![tool_use(
                    "tu_1",
                    "read",
                    serde_json::json!({"filePath": "sample-resume.pdf"}),
                )],
                StopReason::ToolUse,
            ),
            msg(
                vec![text("Jordan Q. Sample's experience section is the strongest part.")],
                StopReason::EndTurn,
            ),
        ],
    );
    let events = collect_events(&mut session, "can you read my resume and give me feedback?");

    assert_eq!(
        requests.borrow().len(),
        3,
        "one nudge, then the read, then the answer"
    );
    assert!(
        notices(&events)
            .iter()
            .any(|n| n.contains("said it cannot read a file without trying")),
        "{:?}",
        notices(&events)
    );
    // The nudge text pinned VERBATIM, not by fragment: it is the whole
    // remedy, and a reworded nudge is a different experiment from the one
    // T58 measured.
    let expected = "You can read files here. Find it with glob or by reading the \
                    working directory, then read it with the read tool; PDF, Word \
                    and spreadsheet files come back as text. Do that now, then \
                    answer.";
    assert!(
        session.history().iter().any(|m| {
            matches!(m.role, Role::User)
                && m.content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::Text { text } if text == expected))
        }),
        "the file-denial nudge text has drifted"
    );
}

#[test]
fn a_file_denial_after_a_dispatched_tool_does_not_nudge() {
    // The turn DID read something. A closing sentence about not being
    // able to process files then reads as prose about what was done.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(
                vec![tool_use(
                    "tu_1",
                    "read",
                    serde_json::json!({"filePath": "sample-resume.pdf"}),
                )],
                StopReason::ToolUse,
            ),
            msg(
                vec![text("Some formats I can't read or process, but this one read fine.")],
                StopReason::EndTurn,
            ),
        ],
    );
    let events = collect_events(&mut session, "read my resume");

    assert_eq!(requests.borrow().len(), 2, "no third request: no nudge");
    assert!(
        !notices(&events)
            .iter()
            .any(|n| n.contains("said it cannot read a file without trying")),
        "{:?}",
        notices(&events)
    );
}

#[test]
fn a_file_denial_phrase_followed_by_the_answer_does_not_nudge() {
    // The tail rule at loop level: "upload" appears EARLY, the feedback
    // follows it, and the reply is finished. Sized past
    // SCOPE_DENIAL_TAIL_CHARS the way its T48 sibling above is.
    let dir = tempfile::tempdir().unwrap();
    let body = "the summary section is strong, the experience entries carry \
                numbers, and the skills list is the part to cut down. "
        .repeat(12);
    let (mut session, requests) = session_with(
        dir.path(),
        vec![msg(
            vec![text(&format!(
                "There is no upload here, so I read it from the working directory. \
                 {body}"
            ))],
            StopReason::EndTurn,
        )],
    );
    let events = collect_events(&mut session, "give me feedback on my resume");

    assert_eq!(requests.borrow().len(), 1);
    assert!(
        !notices(&events)
            .iter()
            .any(|n| n.contains("said it cannot read a file without trying")),
        "{:?}",
        notices(&events)
    );
}

#[test]
fn file_denial_nudges_are_capped_at_two() {
    // The promise sibling's shape, with three D25-style denials and no
    // tool call anywhere. NUDGE_LIMIT is shared across every nudge kind,
    // so the file family is bounded the same way: two nudges, then the
    // third denial ends the turn instead of trading messages forever.
    // Three DIFFERENT phrases, so this cannot pass by one phrase being
    // matched three times.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(
                vec![text(
                    "I can't directly read or process files like a resume. Just \
                     paste the text here!",
                )],
                StopReason::EndTurn,
            ),
            msg(
                vec![text(
                    "I can't read files directly, but I can help if you paste the \
                     text content.",
                )],
                StopReason::EndTurn,
            ),
            msg(
                vec![text("I don't have access to your resume file right now.")],
                StopReason::EndTurn,
            ),
        ],
    );
    let events = collect_events(&mut session, "can you read my resume and give me feedback?");

    assert_eq!(requests.borrow().len(), 3);
    let n = notices(&events)
        .iter()
        .filter(|n| n.contains("said it cannot read a file without trying"))
        .count();
    assert_eq!(n, 2, "{:?}", notices(&events));
}

#[test]
fn a_plain_final_answer_does_not_nudge() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![msg(
            vec![text("The file has 42 lines.")],
            StopReason::EndTurn,
        )],
    );
    let events = collect_events(&mut session, "how long is it");

    assert_eq!(requests.borrow().len(), 1);
    assert!(
        !notices(&events)
            .iter()
            .any(|n| n.contains("promised work without calling a tool")),
        "{:?}",
        notices(&events)
    );
}

#[test]
fn promise_nudges_are_capped_at_two() {
    // A model that only ever promises terminates: two nudges, then the
    // third promise ends the turn instead of trading messages forever.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(vec![text("Please wait while I analyze it")], StopReason::EndTurn),
            msg(vec![text("One moment")], StopReason::EndTurn),
            msg(vec![text("I will now begin")], StopReason::EndTurn),
        ],
    );
    let events = collect_events(&mut session, "analyze");

    assert_eq!(requests.borrow().len(), 3);
    let n = notices(&events)
        .iter()
        .filter(|n| n.contains("promised work without calling a tool"))
        .count();
    assert_eq!(n, 2, "{:?}", notices(&events));
}

// ---------------------------------------------------------------------------
// T36: the futile-call guard. A rotating repertoire of tool calls passes the
// doom-loop guard (no identical-consecutive pair), the alternating-pair guard
// (no strict A,B,A,B,A,B) and ProseRepeatGuard (prose path only). What the
// archived loop had that real work does not is calls returning byte-identical
// results to calls already in context. Shapes below are the archived one
// (~/temur-eval-archive/llama32-coercion-2026-08-16, task8.run1) and the
// legitimate-work shapes the guard must leave alone.
// ---------------------------------------------------------------------------

/// One bash call per response, so dispatch number == request number.
fn bash_call(cmd: &str) -> ResponseMessage {
    msg(
        vec![tool_use("tu_x", "bash", serde_json::json!({"command": cmd}))],
        StopReason::ToolUse,
    )
}

fn futile_notice(events: &[AgentEvent]) -> Option<String> {
    notices(&events.to_vec())
        .into_iter()
        .find(|n| n.contains("repeated earlier calls with unchanged results"))
}

#[test]
fn rotating_repertoire_notices_at_six_and_stops_at_eighteen() {
    // The archived shape, minimised: three distinct calls cycling, each with
    // an unchanging result. Dispatches 1-3 are first sightings; every one
    // after is futile, so the count is dispatch-3. Notice at dispatch 9
    // (count 6), stop at dispatch 21 (count 18). Exactly 21 responses are
    // scripted: a 22nd request would panic the mock, pinning the stop
    // structurally as well as by assertion.
    let dir = tempfile::tempdir().unwrap();
    let cycle = ["echo alpha", "echo beta", "echo gamma"];
    let responses: Vec<ResponseMessage> = (0..21).map(|i| bash_call(cycle[i % 3])).collect();
    let (mut session, requests) = session_with(dir.path(), responses);
    let events = collect_events(&mut session, "keep going");

    assert_eq!(requests.borrow().len(), 21);
    // Neither existing guard is what fired.
    assert!(!notices(&events).iter().any(|n| n.contains("alternated")));
    assert!(!notices(&events)
        .iter()
        .any(|n| n.contains("repeated 3 times in a row")));

    let notice = futile_notice(&events).expect("futile notice");
    assert!(notice.starts_with("6 tool calls this turn"), "{notice}");
    // T64 P1b: the UI notices now say which guard counted; every call here
    // repeats its own input, so all 18 are by input.
    assert!(notices(&events).iter().any(|n| n
        == "stopped: 18 tool calls this turn repeated earlier calls with unchanged results (18 by input, 0 by result)"));

    // The notice fires exactly once, and rides the SAME user message as the
    // results it is about: request 10 carries it as trailing text after the
    // tool_result block for dispatch 9.
    assert_eq!(
        notices(&events)
            .iter()
            .filter(|n| n.contains("asked the model to use what it already has"))
            .count(),
        1
    );
    let reqs = requests.borrow();
    let last = reqs[9].messages.last().unwrap();
    assert!(matches!(&last.content[0], ContentBlock::ToolResult { .. }));
    match &last.content[1] {
        ContentBlock::Text { text } => {
            assert!(text.starts_with("6 of the tool calls this turn re-ran a call"), "{text}");
            assert!(text.contains("byte-identical results"));
        }
        other => panic!("expected trailing text block, got {other:?}"),
    }
    // Execution CONTINUED after the notice: dispatch 10 ran for real.
    assert!(reqs.len() > 10);
}

#[test]
fn changed_results_never_count_as_futile() {
    // The same three fingerprints cycling, but every result differs (each
    // command appends to its own counter file and prints the new count).
    // Fingerprint-only counting would have tripped the notice long before
    // dispatch 21; a progress discriminator must not.
    let dir = tempfile::tempdir().unwrap();
    let cycle = [
        "echo x >> a.count; wc -l < a.count",
        "echo x >> b.count; wc -l < b.count",
        "echo x >> c.count; wc -l < c.count",
    ];
    let mut responses: Vec<ResponseMessage> = (0..21).map(|i| bash_call(cycle[i % 3])).collect();
    responses.push(msg(vec![text("done")], StopReason::EndTurn));
    let (mut session, requests) = session_with(dir.path(), responses);
    let events = collect_events(&mut session, "count things");

    assert_eq!(requests.borrow().len(), 22);
    assert!(futile_notice(&events).is_none(), "{:?}", notices(&events));
}

#[test]
fn reread_after_an_edit_never_counts_as_futile() {
    // Edit-then-reread is the canonical legitimate repeat: the read
    // fingerprint is byte-identical every time and the result is not,
    // because the write in between changed the file. Twelve reads, so a
    // fingerprint-only guard would have fired at the sixth.
    let dir = tempfile::tempdir().unwrap();
    let mut responses = vec![];
    for i in 0..12 {
        responses.push(msg(
            vec![tool_use(
                "tu_w",
                "write",
                serde_json::json!({"filePath": "notes.txt", "content": format!("revision {i}\n")}),
            )],
            StopReason::ToolUse,
        ));
        responses.push(msg(
            vec![tool_use(
                "tu_r",
                "read",
                serde_json::json!({"filePath": "notes.txt"}),
            )],
            StopReason::ToolUse,
        ));
    }
    responses.push(msg(vec![text("done")], StopReason::EndTurn));
    let (mut session, requests) = session_with(dir.path(), responses);
    let events = collect_events(&mut session, "revise the file");

    assert_eq!(requests.borrow().len(), 25);
    assert!(futile_notice(&events).is_none(), "{:?}", notices(&events));
}

#[test]
fn a_ten_file_edit_rotation_never_counts_as_futile() {
    // The tension named in the ROADMAP entry: a model editing ten files
    // really does rotate through the same few TOOLS. Distinct arguments mean
    // distinct fingerprints, so nothing here is ever a repeat: twenty
    // dispatches, no notice.
    let dir = tempfile::tempdir().unwrap();
    let mut responses = vec![];
    for i in 0..10 {
        responses.push(msg(
            vec![tool_use(
                "tu_w",
                "write",
                serde_json::json!({"filePath": format!("f{i}.txt"), "content": "header\n"}),
            )],
            StopReason::ToolUse,
        ));
        responses.push(msg(
            vec![tool_use(
                "tu_r",
                "read",
                serde_json::json!({"filePath": format!("f{i}.txt")}),
            )],
            StopReason::ToolUse,
        ));
    }
    responses.push(msg(vec![text("all ten done")], StopReason::EndTurn));
    let (mut session, requests) = session_with(dir.path(), responses);
    let events = collect_events(&mut session, "edit ten files");

    assert_eq!(requests.borrow().len(), 21);
    assert!(futile_notice(&events).is_none(), "{:?}", notices(&events));
}

#[test]
fn repeated_identical_failures_count_as_futile() {
    // The archived loop's nineteen byte-identical range errors: an error
    // result is what the model SEES, so an identical retry of a failing call
    // is exactly as uninformative as an identical retry of a succeeding one.
    //
    // The rotation isolates that claim. Two `read` calls fail with the same
    // error text every time; the third call succeeds with a DIFFERENT result
    // every time and so never counts. Only the failures can reach the
    // threshold, and they do it at dispatch 11 (each futile from its second
    // sighting: dispatches 4, 5, 7, 8, 10, 11), not at dispatch 9.
    let dir = tempfile::tempdir().unwrap();
    let missing = |name: &str| {
        msg(
            vec![tool_use(
                "tu_r",
                "read",
                serde_json::json!({"filePath": name}),
            )],
            StopReason::ToolUse,
        )
    };
    let mut responses = vec![];
    for i in 0..11 {
        responses.push(match i % 3 {
            0 => missing("missing-one.txt"),
            1 => missing("missing-two.txt"),
            _ => bash_call("echo x >> tick.count; wc -l < tick.count"),
        });
    }
    responses.push(msg(vec![text("giving up")], StopReason::EndTurn));
    let (mut session, requests) = session_with(dir.path(), responses);
    let events = collect_events(&mut session, "try the impossible");

    assert_eq!(requests.borrow().len(), 12);
    // The reads really did come back as ERROR results, which is the whole
    // point of hashing what the model sees rather than only successes.
    assert!(events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolEnd { is_error: true, .. }))
        .count()
        >= 6);
    // The consecutive-all-errored cap never gets near five: the succeeding
    // call resets it every third batch.
    assert!(!notices(&events).iter().any(|n| n.contains("consecutive batches")));
    let notice = futile_notice(&events).expect("futile notice");
    assert!(notice.starts_with("6 tool calls this turn"), "{notice}");
    // Dispatch 11 is the sixth futile one, so the notice rides request 12.
    let reqs = requests.borrow();
    match &reqs[11].messages.last().unwrap().content[1] {
        ContentBlock::Text { text } => assert!(text.contains("byte-identical results")),
        other => panic!("expected trailing text block, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// T61: an unattended session is told to finish and prove it.
//
// The Aug 27 Terminal-Bench cells on the GPU box leave two endings on the
// table that cost the prompt nothing: 9 of 64 cells end by asking the user a
// question nobody is there to answer, and 10 of 64 end with a completion
// claim that ran nothing to check it. Non-solving cells finish in 50 to 250 s
// of a 900 s budget, so one more turn is affordable everywhere.
// ---------------------------------------------------------------------------

/// The sentence the nudge sends, as one string, so a test cannot pass
/// against a paraphrase of it.
const UNATTENDED_NUDGE: &str = "Nobody is reading this session, so no answer or approval will \
                                come. If the task is not finished, decide for yourself and \
                                finish it. If it is finished, run whatever proves the result \
                                (the tests, the compiler, the file's contents), fix what \
                                fails, then stop.";

fn user_texts(session: &Session) -> Vec<String> {
    session
        .history()
        .iter()
        .filter(|m| matches!(m.role, Role::User))
        .flat_map(|m| {
            m.content.iter().filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
        })
        .collect()
}

fn unattended_notices(events: &[AgentEvent]) -> Vec<String> {
    notices(events)
        .into_iter()
        .filter(|n| n.starts_with("unattended:"))
        .collect()
}

#[test]
fn an_unattended_turn_that_asks_a_question_is_told_to_finish_it() {
    // The first Aug 27 shape, verbatim: the cell ends by asking the operator
    // which way to go, in a session with no operator, and most of the budget
    // goes unused. One nudge, then the model decides for itself.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_unattended(
        dir.path(),
        vec![
            msg(
                vec![text(
                    "I could not get the build to configure. Would you like me to try a \
                     different approach?",
                )],
                StopReason::EndTurn,
            ),
            msg(vec![text("I built it with the fallback and it links.")], StopReason::EndTurn),
        ],
    );
    let events = collect_events(&mut session, "build it");

    // The loop continued: a second request went out carrying the nudge.
    assert_eq!(requests.borrow().len(), 2);
    assert_eq!(
        user_texts(&session)
            .iter()
            .filter(|t| t.as_str() == UNATTENDED_NUDGE)
            .count(),
        1,
        "exactly one nudge, verbatim: {:?}",
        user_texts(&session)
    );
    // The second text-only EndTurn ends the turn rather than earning a
    // second nudge.
    assert_eq!(unattended_notices(&events).len(), 1, "{:?}", notices(&events));
}

#[test]
fn an_interactive_session_never_sees_the_unattended_nudge() {
    // The same script with somebody reading. The question is addressed to a
    // person who can answer it, so the turn ends where the model ended it.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_with(
        dir.path(),
        vec![
            msg(
                vec![text(
                    "I could not get the build to configure. Would you like me to try a \
                     different approach?",
                )],
                StopReason::EndTurn,
            ),
            msg(vec![text("unused")], StopReason::EndTurn),
        ],
    );
    let events = collect_events(&mut session, "build it");

    assert_eq!(requests.borrow().len(), 1, "no second request: no nudge");
    assert!(unattended_notices(&events).is_empty(), "{:?}", notices(&events));
    assert!(!user_texts(&session).iter().any(|t| t == UNATTENDED_NUDGE));
}

#[test]
fn the_narrowed_trigger_reads_mutation_and_the_question_mark() {
    // Ruling 1, pinned three ways in one place. Unattended alone is not
    // enough: the turn must have CHANGED something (so there is a result to
    // prove) or have ended on a question (which nobody will answer). A
    // read-only turn that states its answer is finished, and is left alone,
    // which is what keeps `temur -p "what does this repo do"` at one
    // request and one answer.

    // (a) read-only tool, plain statement: NOT nudged.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("seen.txt"), "contents").unwrap();
    let (mut session, requests) = session_unattended(
        dir.path(),
        vec![
            msg(
                vec![tool_use(
                    "tu_1",
                    "read",
                    serde_json::json!({"filePath": "seen.txt"}),
                )],
                StopReason::ToolUse,
            ),
            msg(vec![text("The file contains the word contents.")], StopReason::EndTurn),
        ],
    );
    let events = collect_events(&mut session, "what does the file say");
    assert_eq!(requests.borrow().len(), 2, "the call and its result, no nudge");
    assert!(
        unattended_notices(&events).is_empty(),
        "read-only and finished: {:?}",
        notices(&events)
    );

    // (b) mutating tool, the same plain statement: nudged. This is the
    // completion-claim shape, and the message that CARRIED the call is
    // still never nudged itself (that would interrupt a turn mid-flight):
    // the nudge lands on the text-only message that follows it.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_unattended(
        dir.path(),
        vec![
            msg(
                vec![
                    text("Writing the file now."),
                    tool_use(
                        "tu_1",
                        "write",
                        serde_json::json!({"filePath": "out.txt", "content": "ok"}),
                    ),
                ],
                StopReason::ToolUse,
            ),
            msg(vec![text("Done, and it says ok.")], StopReason::EndTurn),
            msg(vec![text("Verified with a read: it says ok.")], StopReason::EndTurn),
        ],
    );
    let events = collect_events(&mut session, "write the file");
    assert_eq!(requests.borrow().len(), 3, "the call, its result, then the nudge");
    assert_eq!(unattended_notices(&events).len(), 1, "{:?}", notices(&events));

    // (c) no tool at all, but the text ends on a question: nudged.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, requests) = session_unattended(
        dir.path(),
        vec![
            msg(vec![text("Shall I try a different approach?")], StopReason::EndTurn),
            msg(vec![text("I took the second approach and it worked.")], StopReason::EndTurn),
        ],
    );
    let events = collect_events(&mut session, "get it building");
    assert_eq!(requests.borrow().len(), 2, "the question, then the nudge");
    assert_eq!(unattended_notices(&events).len(), 1, "{:?}", notices(&events));
}

#[test]
fn a_promise_in_an_unattended_session_gets_the_promise_nudge() {
    // Priority, pinned where it can actually be observed. Under Ruling 1 a
    // mutating turn can never also be a promise turn (the promise predicate
    // needs a turn that dispatched NOTHING), so the overlap is exactly this
    // shape: no tool ran, the text carries a promise phrase, and it ends on
    // a question. Both predicates want it; the promise wording is the one
    // that fits, so the promise nudge fires and T61 never sees the turn.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _requests) = session_unattended(
        dir.path(),
        vec![
            msg(
                vec![text("Please wait while I verify the file. Shall I continue?")],
                StopReason::EndTurn,
            ),
            msg(vec![text("The file is correct.")], StopReason::EndTurn),
        ],
    );
    let events = collect_events(&mut session, "verify the file");

    let ns = notices(&events);
    assert!(
        ns.iter().any(|n| n.contains("promised work without calling a tool")),
        "{ns:?}"
    );
    assert!(unattended_notices(&events).is_empty(), "{ns:?}");
    let nudges: Vec<String> = user_texts(&session)
        .into_iter()
        .filter(|t| t.contains("Nothing runs between turns") || t == UNATTENDED_NUDGE)
        .collect();
    assert_eq!(nudges.len(), 1, "{nudges:?}");
    assert!(nudges[0].contains("Nothing runs between turns"), "{}", nudges[0]);
}

#[test]
fn the_unattended_nudge_fires_once_per_session_not_once_per_turn() {
    // The plain piped shape: prompts arrive down a pipe, so one session runs
    // several turns with nobody reading any of them. The nudge is a thing
    // the session says once, and the second turn is left alone.
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _requests) = session_unattended(
        dir.path(),
        vec![
            msg(vec![text("Shall I continue with the second file?")], StopReason::EndTurn),
            msg(vec![text("I finished the first file and checked it.")], StopReason::EndTurn),
            msg(vec![text("Shall I continue with the third file?")], StopReason::EndTurn),
        ],
    );
    let first = collect_events(&mut session, "do the first file");
    let second = collect_events(&mut session, "do the second file");

    assert_eq!(unattended_notices(&first).len(), 1, "{:?}", notices(&first));
    assert!(
        unattended_notices(&second).is_empty(),
        "the latch is the session's, not the turn's: {:?}",
        notices(&second)
    );
    assert_eq!(
        user_texts(&session)
            .iter()
            .filter(|t| t.as_str() == UNATTENDED_NUDGE)
            .count(),
        1
    );
}

/// T64 P1: `"unattended_nudge": false` turns the nudge off. The same
/// script that earns a nudge below ends after its claim, as before T61.
#[test]
fn the_unattended_nudge_can_be_turned_off() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("plus_comm.v"), "Theorem plus_comm.\nAdmitted.\n").unwrap();
    let (mut session, requests) = session_with_nudge(
        dir.path(),
        vec![
            msg(
                vec![tool_use(
                    "tu_1",
                    "write",
                    serde_json::json!({"filePath": "plus_comm.v", "content": "Theorem plus_comm.\nProof. reflexivity. Qed.\n"}),
                )],
                StopReason::ToolUse,
            ),
            msg(vec![text("The proof is now complete and should compile.")], StopReason::EndTurn),
            msg(vec![text("I ran coqc and it compiles.")], StopReason::EndTurn),
        ],
        true,
        true,
        false,
    );
    let events = collect_events(&mut session, "prove it");
    assert!(unattended_notices(&events).is_empty(), "{:?}", notices(&events));
    assert_eq!(
        user_texts(&session).iter().filter(|t| t.as_str() == UNATTENDED_NUDGE).count(),
        0
    );
    assert_eq!(requests.borrow().len(), 2, "no third request: the turn ended on the claim");
}

#[test]
fn the_unattended_notice_is_emitted_once_and_verbatim() {
    // The transcript line the Terminal-Bench cells are counted from. Its
    // wording is part of the instrument, so it is asserted whole.
    //
    // The script is the Aug 27 prove-plus-comm shape, which is why the claim
    // earns a nudge at all under Ruling 1: the cell EDITED the proof file
    // and then asserted the result without running the compiler over it.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("plus_comm.v"), "Theorem plus_comm.\nAdmitted.\n").unwrap();
    let (mut session, _requests) = session_unattended(
        dir.path(),
        vec![
            msg(
                vec![tool_use(
                    "tu_1",
                    "write",
                    serde_json::json!({"filePath": "plus_comm.v", "content": "Theorem plus_comm.\nProof. reflexivity. Qed.\n"}),
                )],
                StopReason::ToolUse,
            ),
            msg(vec![text("The proof is now complete and should compile.")], StopReason::EndTurn),
            msg(vec![text("I ran coqc and it compiles.")], StopReason::EndTurn),
        ],
    );
    let events = collect_events(&mut session, "prove it");

    assert_eq!(
        unattended_notices(&events),
        vec!["unattended: the turn ended without a tool call; one continue nudge sent"]
    );
}

// ---------------------------------------------------------------------------
// T64 P1b (F11): the result-hash guard. T36 keys on {name}:{input}, so a
// one-character change to the input evaded it while the result stayed
// byte-identical. Same counter, same notice and stop, and the model-facing
// text unchanged; only the UI notices carry the per-guard tallies.
// ---------------------------------------------------------------------------

fn stop_notice(events: &[AgentEvent]) -> Option<String> {
    notices(&events.to_vec())
        .into_iter()
        .find(|n| n.starts_with("stopped: ") && n.contains("unchanged results"))
}

#[test]
fn identical_results_under_changing_inputs_notice_at_six_and_stop_at_eighteen() {
    // Twenty distinct commands, one output. Dispatches 1 and 2 build the
    // streak; from dispatch 3 each is futile, so the count is dispatch-2:
    // the notice at dispatch 8 (count 6), the stop at dispatch 20 (count 18).
    // Exactly 20 responses: a 21st request would panic the mock.
    let dir = tempfile::tempdir().unwrap();
    let responses: Vec<ResponseMessage> =
        (1..=20).map(|i| bash_call(&format!("echo same #{i}"))).collect();
    let (mut session, requests) = session_with(dir.path(), responses);
    let events = collect_events(&mut session, "keep going");

    assert_eq!(requests.borrow().len(), 20);
    let notice = futile_notice(&events).expect("futile notice");
    assert_eq!(
        notice,
        "6 tool calls this turn repeated earlier calls with unchanged results; asked the model to use what it already has (0 by input, 6 by result)"
    );
    assert_eq!(
        stop_notice(&events).as_deref(),
        Some("stopped: 18 tool calls this turn repeated earlier calls with unchanged results (0 by input, 18 by result)")
    );
}

#[test]
fn no_output_results_never_count_as_repeats() {
    // Twenty distinct commands that print nothing each return exactly
    // "(no output)": legitimate work, never futile.
    let dir = tempfile::tempdir().unwrap();
    let mut responses: Vec<ResponseMessage> =
        (1..=20).map(|i| bash_call(&format!("true #{i}"))).collect();
    responses.push(msg(vec![text("done")], StopReason::EndTurn));
    let (mut session, requests) = session_with(dir.path(), responses);
    let events = collect_events(&mut session, "keep going");

    assert_eq!(requests.borrow().len(), 21);
    assert!(futile_notice(&events).is_none(), "{:?}", notices(&events));
    assert!(stop_notice(&events).is_none(), "{:?}", notices(&events));
}

#[test]
fn a_rotation_with_changing_results_never_counts() {
    let dir = tempfile::tempdir().unwrap();
    let mut responses: Vec<ResponseMessage> =
        (1..=20).map(|i| bash_call(&format!("echo {i}"))).collect();
    responses.push(msg(vec![text("done")], StopReason::EndTurn));
    let (mut session, requests) = session_with(dir.path(), responses);
    let events = collect_events(&mut session, "keep going");

    assert_eq!(requests.borrow().len(), 21);
    assert!(futile_notice(&events).is_none(), "{:?}", notices(&events));
}

#[test]
fn a_call_both_guards_match_is_counted_once() {
    // Three commands cycling, all printing "x". Dispatch 3 is the first
    // futile call and T36 has not seen its input, so it counts by result.
    // From dispatch 4 every call repeats its own input with the same
    // result, and counts by input only. The tallies always sum to the
    // count: the notice at dispatch 8, the stop at dispatch 20.
    let dir = tempfile::tempdir().unwrap();
    let cycle = ["echo x", "echo x #b", "echo x #c"];
    let responses: Vec<ResponseMessage> = (0..20).map(|i| bash_call(cycle[i % 3])).collect();
    let (mut session, requests) = session_with(dir.path(), responses);
    let events = collect_events(&mut session, "keep going");

    assert_eq!(requests.borrow().len(), 20);
    let notice = futile_notice(&events).expect("futile notice");
    assert!(notice.starts_with("6 tool calls this turn"), "{notice}");
    assert!(notice.ends_with("(5 by input, 1 by result)"), "{notice}");
    assert_eq!(
        stop_notice(&events).as_deref(),
        Some("stopped: 18 tool calls this turn repeated earlier calls with unchanged results (17 by input, 1 by result)")
    );
}

#[test]
fn a_two_input_alternation_is_stopped_by_the_alternating_pair_guard_first() {
    // Ruling T64-11 asked where this evasion lands. `cat f` and `cat f `
    // (a trailing space) alternate with one result. From dispatch 3 each
    // call repeats a fingerprint this turn has seen, so T36 counts it by
    // input and the result guard never does: its count needs an unseen
    // input. The count never reaches the notice at 6, because T4's
    // alternating-pair guard sees A,B,A,B,A,B at the sixth response and
    // stops before dispatching it.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f"), "same\n").unwrap();
    let responses: Vec<ResponseMessage> =
        (0..10).map(|i| bash_call(if i % 2 == 0 { "cat f" } else { "cat f " })).collect();
    let (mut session, requests) = session_with(dir.path(), responses);
    let events = collect_events(&mut session, "keep going");

    assert_eq!(requests.borrow().len(), 6);
    assert!(
        notices(&events).iter().any(|n| n == "stopped: two tool calls alternated 3 times in a row"),
        "{:?}",
        notices(&events)
    );
    assert!(futile_notice(&events).is_none(), "{:?}", notices(&events));
    assert!(stop_notice(&events).is_none(), "{:?}", notices(&events));
}

// ---------------------------------------------------------------------------
// T64 P1b follow-up (Ruling T64-14): acknowledgements of a change never count
// as repeats. bash is NOT excluded: identical_results_under_changing_inputs_
// notice_at_six_and_stop_at_eighteen drives bash and still notices at 6.
// ---------------------------------------------------------------------------

#[test]
fn distinct_edits_to_one_file_never_count_as_repeats() {
    // Three different single edits to one file, then five more: every result
    // is "Edited <path> (1 replacement(s))", byte-identical, from new inputs.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "x1 x2 x3 x4 x5 x6 x7 x8\n").unwrap();
    let mut responses: Vec<ResponseMessage> = (1..=8)
        .map(|i| {
            msg(
                vec![tool_use(
                    "tu_e",
                    "edit",
                    serde_json::json!({"filePath": "f.txt", "oldString": format!("x{i}"), "newString": format!("y{i}")}),
                )],
                StopReason::ToolUse,
            )
        })
        .collect();
    responses.push(msg(vec![text("done")], StopReason::EndTurn));
    let (mut session, requests) = session_with(dir.path(), responses);
    let events = collect_events(&mut session, "edit it");

    assert_eq!(requests.borrow().len(), 9);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "y1 y2 y3 y4 y5 y6 y7 y8\n",
        "all eight edits really ran"
    );
    assert!(futile_notice(&events).is_none(), "{:?}", notices(&events));
}

#[test]
fn same_size_rewrites_of_one_file_never_count_as_repeats() {
    // write answers "Overwrote <path> (3 bytes, replaced 3 bytes of prior
    // content)" for every same-size rewrite: identical, and still progress.
    // Ten writes: the first says "Created", the other nine are identical, so
    // without the acknowledgement rule the streak counts 7 and the notice
    // fires at 6. With the rule nothing counts.
    let dir = tempfile::tempdir().unwrap();
    let mut responses: Vec<ResponseMessage> = (0..=9)
        .map(|i| {
            msg(
                vec![tool_use("tu_w", "write", serde_json::json!({"filePath": "w.txt", "content": format!("v{i}\n")}))],
                StopReason::ToolUse,
            )
        })
        .collect();
    responses.push(msg(vec![text("done")], StopReason::EndTurn));
    let (mut session, requests) = session_with(dir.path(), responses);
    let events = collect_events(&mut session, "write it");

    assert_eq!(requests.borrow().len(), 11);
    assert_eq!(std::fs::read_to_string(dir.path().join("w.txt")).unwrap(), "v9\n");
    assert!(futile_notice(&events).is_none(), "{:?}", notices(&events));
}

#[test]
fn whitespace_only_results_never_count_as_repeats() {
    // printf of three spaces returns "   ", not "(no output)", so this is the
    // empty-after-trim branch on its own.
    let dir = tempfile::tempdir().unwrap();
    let mut responses: Vec<ResponseMessage> =
        (1..=20).map(|i| bash_call(&format!("printf '   ' #{i}"))).collect();
    responses.push(msg(vec![text("done")], StopReason::EndTurn));
    let (mut session, requests) = session_with(dir.path(), responses);
    let events = collect_events(&mut session, "keep going");

    assert_eq!(requests.borrow().len(), 21);
    assert!(futile_notice(&events).is_none(), "{:?}", notices(&events));
}

#[test]
fn a_different_result_restarts_the_streak() {
    // Pairs of identical results from new inputs: a, a, b, b, c, c, ... The
    // streak restarts at every change, so it never reaches 3. A streak that
    // failed to restart would count from the third call.
    let dir = tempfile::tempdir().unwrap();
    let mut responses: Vec<ResponseMessage> = (0..20)
        .map(|i| bash_call(&format!("echo v{} #{i}", i / 2)))
        .collect();
    responses.push(msg(vec![text("done")], StopReason::EndTurn));
    let (mut session, requests) = session_with(dir.path(), responses);
    let events = collect_events(&mut session, "keep going");

    assert_eq!(requests.borrow().len(), 21);
    assert!(futile_notice(&events).is_none(), "{:?}", notices(&events));
}

#[test]
fn an_acknowledgement_between_identical_results_resets_the_streak() {
    // Two identical bash results, then an edit, seven times over. The
    // acknowledgement RESETS the streak, so it never reaches 3. A rule that
    // merely skipped acknowledgements would let the bash streak run to 14
    // and count 12, firing the notice. (Without the rule at all, the edit's
    // different result also restarts the streak, so the pin that A2 exists
    // is distinct_edits_to_one_file_never_count_as_repeats, not this one.)
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "x1 x2 x3 x4 x5 x6 x7\n").unwrap();
    let mut responses: Vec<ResponseMessage> = Vec::new();
    for k in 1..=7 {
        responses.push(bash_call(&format!("echo same #{k}a")));
        responses.push(bash_call(&format!("echo same #{k}b")));
        responses.push(msg(
            vec![tool_use(
                "tu_e",
                "edit",
                serde_json::json!({"filePath": "f.txt", "oldString": format!("x{k}"), "newString": format!("y{k}")}),
            )],
            StopReason::ToolUse,
        ));
    }
    responses.push(msg(vec![text("done")], StopReason::EndTurn));
    let (mut session, requests) = session_with(dir.path(), responses);
    let events = collect_events(&mut session, "keep going");

    assert_eq!(requests.borrow().len(), 22);
    assert_eq!(std::fs::read_to_string(dir.path().join("f.txt")).unwrap(), "y1 y2 y3 y4 y5 y6 y7\n");
    assert!(futile_notice(&events).is_none(), "{:?}", notices(&events));
}

