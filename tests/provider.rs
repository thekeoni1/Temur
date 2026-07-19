//! M2 provider tests — the full request→stream→completion path over a
//! fixture Transport. No network, no live API.

use temur::provider::anthropic::transport::{Transport, TransportError};
use temur::provider::anthropic::AnthropicProvider;
use temur::provider::*;
use std::cell::RefCell;
use std::io::Read;

/// Scripted transport: pops one outcome per call, records request bodies.
struct ScriptedTransport {
    outcomes: RefCell<Vec<Result<&'static str, TransportError>>>, // fixture name or error
    bodies: RefCell<Vec<String>>,
    keys: RefCell<Vec<String>>,
    urls: RefCell<Vec<String>>,
}

impl ScriptedTransport {
    fn new(outcomes: Vec<Result<&'static str, TransportError>>) -> Self {
        ScriptedTransport {
            outcomes: RefCell::new(outcomes),
            bodies: RefCell::new(vec![]),
            keys: RefCell::new(vec![]),
            urls: RefCell::new(vec![]),
        }
    }
}

impl Transport for ScriptedTransport {
    fn post_stream(
        &self,
        url: &str,
        api_key: &str,
        body: &str,
    ) -> Result<Box<dyn Read>, TransportError> {
        self.urls.borrow_mut().push(url.to_string());
        self.keys.borrow_mut().push(api_key.to_string());
        self.bodies.borrow_mut().push(body.to_string());
        match self.outcomes.borrow_mut().remove(0) {
            Ok(fixture) => {
                let path = format!(
                    "{}/tests/fixtures/{fixture}.sse",
                    env!("CARGO_MANIFEST_DIR")
                );
                Ok(Box::new(std::fs::File::open(path).unwrap()))
            }
            Err(e) => Err(e),
        }
    }
}

fn sample_request() -> ChatRequest {
    ChatRequest {
        model: "claude-sonnet-5".into(),
        max_tokens: 32_000,
        system: Some("You are a coding agent.".into()),
        thinking: false,
        messages: vec![RequestMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hi".into(),
            }],
        }],
        tools: vec![ToolDef {
            name: "read".into(),
            description: "Reads a file".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"filePath": {"type": "string"}},
                "required": ["filePath"]
            }),
        }],
    }
}

fn provider_with(outcomes: Vec<Result<&'static str, TransportError>>) -> AnthropicProvider {
    AnthropicProvider::new(
        "https://api.example.test",
        "test-key-not-a-secret".into(),
        Box::new(ScriptedTransport::new(outcomes)),
    )
}

// Leaked-transport helper so tests can inspect call records after the
// provider takes ownership.
fn provider_and_transport(
    outcomes: Vec<Result<&'static str, TransportError>>,
) -> (AnthropicProvider, &'static ScriptedTransport) {
    let transport: &'static ScriptedTransport =
        Box::leak(Box::new(ScriptedTransport::new(outcomes)));
    struct Borrowed(&'static ScriptedTransport);
    impl Transport for Borrowed {
        fn post_stream(
            &self,
            url: &str,
            api_key: &str,
            body: &str,
        ) -> Result<Box<dyn Read>, TransportError> {
            self.0.post_stream(url, api_key, body)
        }
    }
    let provider = AnthropicProvider::new(
        "https://api.example.test",
        "test-key-not-a-secret".into(),
        Box::new(Borrowed(transport)),
    );
    (provider, transport)
}

#[test]
fn request_body_shape() {
    let (provider, transport) = provider_and_transport(vec![Ok("text_simple")]);
    provider.stream(&sample_request(), &mut |_| {}).unwrap();

    assert_eq!(
        transport.urls.borrow()[0],
        "https://api.example.test/v1/messages"
    );
    assert_eq!(transport.keys.borrow()[0], "test-key-not-a-secret");

    let body: serde_json::Value = serde_json::from_str(&transport.bodies.borrow()[0]).unwrap();
    assert_eq!(body["model"], "claude-sonnet-5");
    assert_eq!(body["max_tokens"], 32_000);
    assert_eq!(body["stream"], true);
    // system: single block with the static cache breakpoint
    assert_eq!(body["system"][0]["type"], "text");
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    // tools serialized with schema
    assert_eq!(body["tools"][0]["name"], "read");
    assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
    // thinking off => field absent entirely
    assert!(body.get("thinking").is_none());
    // messages round-trip
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    // the key must never leak into the body
    assert!(!transport.bodies.borrow()[0].contains("test-key-not-a-secret"));
}

#[test]
fn thinking_flag_adds_adaptive() {
    let (provider, transport) = provider_and_transport(vec![Ok("text_simple")]);
    let mut req = sample_request();
    req.thinking = true;
    provider.stream(&req, &mut |_| {}).unwrap();
    let body: serde_json::Value = serde_json::from_str(&transport.bodies.borrow()[0]).unwrap();
    assert_eq!(body["thinking"]["type"], "adaptive");
}

#[test]
fn streams_events_and_assembles_tool_use() {
    let provider = provider_with(vec![Ok("tool_use_parallel")]);
    let mut events = vec![];
    let msg = provider
        .stream(&sample_request(), &mut |e| events.push(e))
        .unwrap();

    assert_eq!(msg.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(msg.content.len(), 3);
    assert_eq!(
        events,
        vec![
            StreamEvent::TextDelta("I'll read the file and list the directory.".into()),
            StreamEvent::ToolUseStarted { name: "read".into() },
            StreamEvent::ToolUseStarted { name: "bash".into() },
        ]
    );
}

#[test]
fn retries_429_with_retry_after_then_succeeds() {
    let (provider, transport) = provider_and_transport(vec![
        Err(TransportError::Status {
            code: 429,
            retry_after: Some(0),
            body: r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#
                .into(),
        }),
        Ok("text_simple"),
    ]);
    let msg = provider.stream(&sample_request(), &mut |_| {}).unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(transport.bodies.borrow().len(), 2); // exactly one retry
}

#[test]
fn does_not_retry_400() {
    let (provider, transport) = provider_and_transport(vec![Err(TransportError::Status {
        code: 400,
        retry_after: None,
        body: r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#
            .into(),
    })]);
    let err = provider.stream(&sample_request(), &mut |_| {}).unwrap_err();
    match err {
        ProviderError::Api {
            status,
            kind,
            message,
        } => {
            assert_eq!(status, 400);
            assert_eq!(kind, "invalid_request_error");
            assert_eq!(message, "bad");
        }
        other => panic!("expected Api error, got {other:?}"),
    }
    assert_eq!(transport.bodies.borrow().len(), 1); // no retry
}

#[test]
fn midstream_error_event_becomes_api_error() {
    let provider = provider_with(vec![Ok("error_midstream")]);
    let err = provider.stream(&sample_request(), &mut |_| {}).unwrap_err();
    match err {
        ProviderError::Api { kind, .. } => assert_eq!(kind, "overloaded_error"),
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[test]
fn refusal_completion_carries_stop_details() {
    let provider = provider_with(vec![Ok("refusal_pre_output")]);
    let msg = provider.stream(&sample_request(), &mut |_| {}).unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::Refusal));
    assert_eq!(
        msg.stop_details.unwrap().category.as_deref(),
        Some("cyber")
    );
}

/// Every `cache_control`-carrying (message_index, block_index) in a request
/// body's `messages` array.
fn marked_blocks(body: &serde_json::Value) -> Vec<(usize, usize)> {
    let mut marked = vec![];
    for (mi, m) in body["messages"].as_array().unwrap().iter().enumerate() {
        for (bi, b) in m["content"].as_array().unwrap().iter().enumerate() {
            if b.get("cache_control").is_some() {
                marked.push((mi, bi));
            }
        }
    }
    marked
}

#[test]
fn cache_breakpoints_on_system_and_last_message_block() {
    // Multi-message history shaped like a mid-turn agent request:
    // user → assistant(text + 2 tool_use) → user(2 tool_results).
    let (provider, transport) = provider_and_transport(vec![Ok("text_simple")]);
    let mut req = sample_request();
    req.messages = vec![
        RequestMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "start".into() }],
        },
        RequestMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text { text: "working".into() },
                ContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "read".into(),
                    input: serde_json::json!({"filePath": "/tmp/a.txt"}),
                },
                ContentBlock::ToolUse {
                    id: "tu_2".into(),
                    name: "bash".into(),
                    input: serde_json::json!({"command": "ls /tmp"}),
                },
            ],
        },
        RequestMessage {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "tu_1".into(),
                    content: "file contents".into(),
                    is_error: false,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "tu_2".into(),
                    content: "dir listing".into(),
                    is_error: false,
                },
            ],
        },
    ];
    provider.stream(&req, &mut |_| {}).unwrap();

    let body: serde_json::Value = serde_json::from_str(&transport.bodies.borrow()[0]).unwrap();
    // Static breakpoint on the system block…
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    // …and exactly ONE message-level breakpoint, on the LAST content block
    // of the LAST message (the second tool_result), nowhere else.
    assert_eq!(
        marked_blocks(&body),
        vec![(2, 1)],
        "moving breakpoint must sit on the final block only"
    );
    assert_eq!(
        body["messages"][2]["content"][1]["cache_control"]["type"],
        "ephemeral"
    );
}

#[test]
fn moving_breakpoint_advances_across_agent_iterations() {
    // Full agent loop over the REAL provider + build_body: iteration 1 asks
    // for two tool calls (fixture), iteration 2 ends the turn. Proves the
    // wire bodies carry the moving breakpoint as history grows.
    use temur::agent::{Session, SessionConfig};
    use temur::tools::Registry;

    let (provider, transport) =
        provider_and_transport(vec![Ok("tool_use_parallel"), Ok("text_simple")]);
    let dir = tempfile::tempdir().unwrap();
    let cfg = SessionConfig {
        model: "claude-sonnet-5".into(),
        max_tokens: 32_000,
        system: Some("test system".into()),
        thinking: false,
        cwd: dir.path().to_path_buf(),
        max_iterations: 10,
    };
    let mut session = Session::new(Box::new(provider), Registry::standard(), cfg);
    session.turn("do the smoke task", &mut |_| {}).unwrap();

    let bodies = transport.bodies.borrow();
    assert_eq!(bodies.len(), 2, "tool round-trip = two provider calls");

    // Request 1: history is just the user message — breakpoint on its block.
    let b1: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    assert_eq!(b1["messages"].as_array().unwrap().len(), 1);
    assert_eq!(marked_blocks(&b1), vec![(0, 0)]);
    assert_eq!(b1["system"][0]["cache_control"]["type"], "ephemeral");

    // Request 2: user → assistant(text + 2 tool_use) → user(2 tool_results).
    // The breakpoint MOVED to the last tool_result; the block it marked in
    // request 1 is now unmarked (markers are per-request, never persisted
    // into history).
    let b2: serde_json::Value = serde_json::from_str(&bodies[1]).unwrap();
    let msgs = b2["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    let last_bi = msgs[2]["content"].as_array().unwrap().len() - 1;
    assert_eq!(b2["messages"][2]["content"][last_bi]["type"], "tool_result");
    assert_eq!(
        marked_blocks(&b2),
        vec![(2, last_bi)],
        "single breakpoint, advanced to the newest block"
    );
    assert!(
        b2["messages"][0]["content"][0].get("cache_control").is_none(),
        "request 1's marker must not persist into request 2"
    );
    assert_eq!(b2["system"][0]["cache_control"]["type"], "ephemeral");
}
