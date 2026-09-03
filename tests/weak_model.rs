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

/// `prose_tool_calls` explicit: `true` is the product default (T19 P3
/// prose-call execution), `false` restores T4 detect+nudge.
fn session_with_prose(
    dir: &std::path::Path,
    responses: Vec<ResponseMessage>,
    prose_tool_calls: bool,
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
    let dir = tempfile::tempdir().unwrap();
    let body = "differentiate both sides with respect to x, treat y as a function \
                of x, and apply the chain rule to every term containing it. "
        .repeat(3);
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
    assert!(notices(&events).iter().any(|n| n
        == "stopped: 18 tool calls this turn repeated earlier calls with unchanged results"));

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
