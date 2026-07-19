//! M1 fixture suite: parse each docs-authored SSE fixture, assert the event
//! sequence, and assemble the final message via MessageAccumulator.
//! Provenance: hand-authored from the Messages API streaming reference,
//! cross-checked against Anthropic's official SDK streaming test fixtures.

use temur::provider::anthropic::sse::SseReader;
use temur::provider::anthropic::types::*;
use std::io::BufReader;

fn load(name: &str) -> Vec<SseEvent> {
    let path = format!("{}/tests/fixtures/{name}.sse", env!("CARGO_MANIFEST_DIR"));
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    SseReader::new(BufReader::new(file))
        .map(|r| r.expect("fixture event must parse"))
        .collect()
}

fn assemble(events: &[SseEvent]) -> MessageAccumulator {
    let mut acc = MessageAccumulator::new();
    for ev in events {
        acc.push(ev);
    }
    acc
}

#[test]
fn text_simple() {
    let events = load("text_simple");
    assert_eq!(events.len(), 8);
    assert!(matches!(events[0], SseEvent::MessageStart { .. }));
    assert!(matches!(events[2], SseEvent::Ping));
    assert!(matches!(events[7], SseEvent::MessageStop));

    let msg = assemble(&events).into_message().unwrap();
    assert_eq!(
        msg.content,
        vec![ContentBlock::Text {
            text: "Hello, world!".into()
        }]
    );
    assert_eq!(msg.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(msg.usage.input_tokens, 25);
    assert_eq!(msg.usage.output_tokens, 20);
}

#[test]
fn tool_use_parallel_blocks_and_input_json_assembly() {
    let msg = assemble(&load("tool_use_parallel")).into_message().unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(msg.content.len(), 3);
    assert_eq!(
        msg.content[0],
        ContentBlock::Text {
            text: "I'll read the file and list the directory.".into()
        }
    );
    match &msg.content[1] {
        ContentBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "toolu_01AAA");
            assert_eq!(name, "read");
            assert_eq!(input, &serde_json::json!({"filePath": "/tmp/a.txt"}));
        }
        other => panic!("expected tool_use, got {other:?}"),
    }
    match &msg.content[2] {
        ContentBlock::ToolUse { name, input, .. } => {
            assert_eq!(name, "bash");
            assert_eq!(input, &serde_json::json!({"command": "ls /tmp"}));
        }
        other => panic!("expected tool_use, got {other:?}"),
    }
    assert_eq!(msg.usage.output_tokens, 89);
}

#[test]
fn thinking_block_with_signature() {
    let msg = assemble(&load("thinking")).into_message().unwrap();
    assert_eq!(msg.content.len(), 2);
    match &msg.content[0] {
        ContentBlock::Thinking {
            thinking,
            signature,
        } => {
            assert_eq!(thinking, "Let me think about this.");
            assert_eq!(signature.as_deref(), Some("EqQBCgIYAhIkSig="));
        }
        other => panic!("expected thinking, got {other:?}"),
    }
    assert_eq!(
        msg.content[1],
        ContentBlock::Text {
            text: "The answer is 4.".into()
        }
    );
    assert_eq!(msg.stop_reason, Some(StopReason::EndTurn));
}

#[test]
fn refusal_pre_output_empty_text_block_and_stop_details() {
    // SDK-fixture-confirmed: a pre-output refusal still opens an empty text
    // block, and message_delta carries stop_details.
    let msg = assemble(&load("refusal_pre_output")).into_message().unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::Refusal));
    assert_eq!(msg.content, vec![ContentBlock::Text { text: String::new() }]);
    let details = msg.stop_details.expect("stop_details on refusal");
    assert_eq!(details.kind, "refusal");
    assert_eq!(details.category.as_deref(), Some("cyber"));
}

#[test]
fn max_tokens_mid_tool_json_leaves_input_empty_without_block_stop() {
    // SDK-fixture-confirmed: max_tokens can cut a tool_use block off with no
    // content_block_stop; the incomplete JSON must not be parsed or panic.
    // (Also exercises trailing whitespace after the JSON on data lines.)
    let msg = assemble(&load("incomplete_tool_json")).into_message().unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::MaxTokens));
    match &msg.content[0] {
        ContentBlock::ToolUse { input, .. } => {
            assert_eq!(input, &serde_json::json!({}));
        }
        other => panic!("expected tool_use, got {other:?}"),
    }
}

#[test]
fn refusal_midstream_keeps_partial_flagged_by_stop_reason() {
    let msg = assemble(&load("refusal_midstream")).into_message().unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::Refusal));
    assert_eq!(
        msg.content,
        vec![ContentBlock::Text {
            text: "I can start by".into()
        }]
    );
}

#[test]
fn pause_turn_stop_reason() {
    let msg = assemble(&load("pause_turn")).into_message().unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::PauseTurn));
}

#[test]
fn max_tokens_stop_reason() {
    let msg = assemble(&load("max_tokens")).into_message().unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::MaxTokens));
}

#[test]
fn midstream_error_event_is_captured_not_fatal() {
    let events = load("error_midstream");
    let acc = assemble(&events);
    let err = acc.error.clone().expect("error event captured");
    assert_eq!(err.kind, "overloaded_error");
    assert_eq!(err.message, "Overloaded");
    // Partial content up to the error is still available.
    let msg = acc.into_message().unwrap();
    assert_eq!(
        msg.content,
        vec![ContentBlock::Text {
            text: "Partial answ".into()
        }]
    );
    assert_eq!(msg.stop_reason, None);
}

#[test]
fn unknown_events_blocks_deltas_and_fields_are_tolerated() {
    let events = load("unknown_tolerance");
    assert!(events.iter().any(|e| matches!(e, SseEvent::Unknown)));
    let msg = assemble(&events).into_message().unwrap();
    // Known content assembled despite unknown deltas/blocks around it.
    assert_eq!(
        msg.content[0],
        ContentBlock::Text { text: "ok".into() }
    );
    assert_eq!(msg.content[1], ContentBlock::Unknown);
    assert_eq!(msg.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(msg.usage.output_tokens, 4);
}

#[test]
fn unknown_stop_reason_string_maps_to_unknown() {
    let sr: StopReason = serde_json::from_str("\"totally_new_reason\"").unwrap();
    assert_eq!(sr, StopReason::Unknown);
}

#[test]
fn multiline_data_lines_are_joined() {
    // SSE spec: multiple data: lines join with '\n'. Anthropic sends single
    // lines, but the parser must not corrupt a spec-legal stream.
    let raw = "event: ping\ndata: {\"type\":\ndata: \"ping\"}\n\n";
    let events: Vec<_> = SseReader::new(BufReader::new(raw.as_bytes()))
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(events, vec![SseEvent::Ping]);
}
