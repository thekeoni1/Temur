//! T2 OpenAI-compat provider tests — the full request→stream→completion
//! path over a fixture Transport. No network, no live API.
//!
//! Fixture provenance, three layers: hand-authored from the OpenAI API
//! reference (fragmented/parallel tool calls, [DONE], final-chunk usage),
//! cross-checked against openai-python/openai-node SDK streaming fixtures,
//! plus quirk fixtures modeling local servers (llama.cpp/Ollama): absent
//! usage, absent tool-call IDs, whole-call-in-one-chunk, role deltas.

use temur::provider::openai_compat::OpenAiCompatProvider;
use temur::provider::transport::{Transport, TransportError};
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
                    "{}/tests/fixtures/openai/{fixture}.sse",
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
        model: "qwen2.5-coder-7b".into(),
        max_tokens: 8_000,
        system: Some("You are a coding agent.".into()),
        thinking: false,
        temperature: None,
        top_p: None,
        messages: vec![RequestMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "hi".into() }],
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

fn provider_with(outcomes: Vec<Result<&'static str, TransportError>>) -> OpenAiCompatProvider {
    OpenAiCompatProvider::new(
        "http://local.test/v1",
        Some("test-key-not-a-secret".into()),
        Box::new(ScriptedTransport::new(outcomes)),
    )
}

// Leaked-transport helper so tests can inspect call records after the
// provider takes ownership.
fn provider_and_transport(
    outcomes: Vec<Result<&'static str, TransportError>>,
    api_key: Option<String>,
) -> (OpenAiCompatProvider, &'static ScriptedTransport) {
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
    let provider =
        OpenAiCompatProvider::new("http://local.test/v1", api_key, Box::new(Borrowed(transport)));
    (provider, transport)
}

#[test]
fn request_body_shape() {
    let (provider, transport) =
        provider_and_transport(vec![Ok("text_simple")], Some("test-key-not-a-secret".into()));
    provider.stream(&sample_request(), &mut |_| {}).unwrap();

    assert_eq!(
        transport.urls.borrow()[0],
        "http://local.test/v1/chat/completions"
    );
    assert_eq!(transport.keys.borrow()[0], "test-key-not-a-secret");

    let body: serde_json::Value = serde_json::from_str(&transport.bodies.borrow()[0]).unwrap();
    assert_eq!(body["model"], "qwen2.5-coder-7b");
    // The classic wire name — the compat universe never learned
    // max_completion_tokens.
    assert_eq!(body["max_tokens"], 8_000);
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    // system prompt is a plain leading system message (no cache_control —
    // that is Anthropic wire surface).
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][0]["content"], "You are a coding agent.");
    assert!(body["messages"][0].get("cache_control").is_none());
    assert_eq!(body["messages"][1]["role"], "user");
    assert_eq!(body["messages"][1]["content"], "hi");
    // tools nested under function
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], "read");
    assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
    // knobs absent when unset; thinking has no mapping on this wire
    assert!(body.get("temperature").is_none());
    assert!(body.get("top_p").is_none());
    assert!(body.get("thinking").is_none());
    // the key must never leak into the body
    assert!(!transport.bodies.borrow()[0].contains("test-key-not-a-secret"));
}

#[test]
fn keyless_sends_empty_key() {
    // None key = keyless local endpoint: the transport seam sees "" and the
    // HTTP transport sends no auth header at all.
    let (provider, transport) = provider_and_transport(vec![Ok("text_simple")], None);
    provider.stream(&sample_request(), &mut |_| {}).unwrap();
    assert_eq!(transport.keys.borrow()[0], "");
}

#[test]
fn sampling_knobs_mapped_when_set() {
    let (provider, transport) =
        provider_and_transport(vec![Ok("text_simple")], None);
    let mut req = sample_request();
    req.temperature = Some(0.5);
    req.top_p = Some(0.9);
    provider.stream(&req, &mut |_| {}).unwrap();
    let body: serde_json::Value = serde_json::from_str(&transport.bodies.borrow()[0]).unwrap();
    assert_eq!(body["temperature"], 0.5);
    assert_eq!(body["top_p"], 0.9);
}

#[test]
fn history_fans_out_tool_results_and_drops_thinking() {
    // Neutral history: user → assistant(text + thinking + 2 tool_use) →
    // user(2 tool_results + text). On this wire that must become:
    // system, user, assistant(content + tool_calls, thinking dropped),
    // tool, tool (directly after the assistant message), user.
    let (provider, transport) = provider_and_transport(vec![Ok("text_simple")], None);
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
                ContentBlock::Thinking {
                    thinking: "private reasoning".into(),
                    signature: Some("sig".into()),
                },
                ContentBlock::ToolUse {
                    id: "call_A1".into(),
                    name: "read".into(),
                    input: serde_json::json!({"filePath": "/tmp/a.txt"}),
                },
                ContentBlock::ToolUse {
                    id: "call_B2".into(),
                    name: "bash".into(),
                    input: serde_json::json!({"command": "ls /tmp"}),
                },
            ],
        },
        RequestMessage {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call_A1".into(),
                    content: "file contents".into(),
                    is_error: false,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call_B2".into(),
                    content: "dir listing".into(),
                    is_error: true,
                },
                ContentBlock::Text { text: "keep going".into() },
            ],
        },
    ];
    provider.stream(&req, &mut |_| {}).unwrap();

    let body: serde_json::Value = serde_json::from_str(&transport.bodies.borrow()[0]).unwrap();
    let msgs = body["messages"].as_array().unwrap();
    let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(
        roles,
        vec!["system", "user", "assistant", "tool", "tool", "user"]
    );
    // assistant: text content + tool_calls with STRING arguments
    assert_eq!(msgs[2]["content"], "working");
    assert!(!msgs[2].to_string().contains("private reasoning"));
    assert_eq!(msgs[2]["tool_calls"][0]["id"], "call_A1");
    assert_eq!(msgs[2]["tool_calls"][0]["type"], "function");
    assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "read");
    let args = msgs[2]["tool_calls"][0]["function"]["arguments"].as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(args).unwrap(),
        serde_json::json!({"filePath": "/tmp/a.txt"})
    );
    // tool messages answer by tool_call_id, in call order
    assert_eq!(msgs[3]["tool_call_id"], "call_A1");
    assert_eq!(msgs[3]["content"], "file contents");
    assert_eq!(msgs[4]["tool_call_id"], "call_B2");
    assert_eq!(msgs[4]["content"], "dir listing");
    // trailing user text survives as its own message
    assert_eq!(msgs[5]["content"], "keep going");
}

#[test]
fn assistant_tool_only_message_omits_content() {
    let (provider, transport) = provider_and_transport(vec![Ok("text_simple")], None);
    let mut req = sample_request();
    req.messages = vec![
        RequestMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "go".into() }],
        },
        RequestMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_A1".into(),
                name: "read".into(),
                input: serde_json::json!({}),
            }],
        },
        RequestMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_A1".into(),
                content: "ok".into(),
                is_error: false,
            }],
        },
    ];
    provider.stream(&req, &mut |_| {}).unwrap();
    let body: serde_json::Value = serde_json::from_str(&transport.bodies.borrow()[0]).unwrap();
    let assistant = &body["messages"][2];
    assert_eq!(assistant["role"], "assistant");
    assert!(assistant.get("content").is_none());
    assert_eq!(assistant["tool_calls"][0]["id"], "call_A1");
}

#[test]
fn streams_text_and_maps_final_chunk_usage() {
    let provider = provider_with(vec![Ok("text_simple")]);
    let mut events = vec![];
    let msg = provider
        .stream(&sample_request(), &mut |e| events.push(e))
        .unwrap();

    assert_eq!(
        events,
        vec![
            StreamEvent::TextDelta("Hello,".into()),
            StreamEvent::TextDelta(" world!".into()),
        ]
    );
    assert_eq!(
        msg.content,
        vec![ContentBlock::Text { text: "Hello, world!".into() }]
    );
    assert_eq!(msg.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(msg.id, "chatcmpl-9XYZ001");
    assert_eq!(msg.model, "gpt-4o-mini");
    // usage from the include_usage final chunk, best-effort mapped
    assert_eq!(msg.usage.input_tokens, Some(25));
    assert_eq!(msg.usage.output_tokens, Some(20));
    assert_eq!(msg.usage.cache_read_input_tokens, Some(10));
    assert_eq!(msg.usage.cache_creation_input_tokens, None);
}

#[test]
fn assembles_arguments_fragmented_across_chunks() {
    let provider = provider_with(vec![Ok("tool_fragmented")]);
    let mut events = vec![];
    let msg = provider
        .stream(&sample_request(), &mut |e| events.push(e))
        .unwrap();

    assert_eq!(msg.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(events, vec![StreamEvent::ToolUseStarted { name: "read".into() }]);
    assert_eq!(msg.content.len(), 1);
    match &msg.content[0] {
        ContentBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "call_A1");
            assert_eq!(name, "read");
            assert_eq!(input, &serde_json::json!({"filePath": "/tmp/a.txt"}));
        }
        other => panic!("expected tool_use, got {other:?}"),
    }
}

#[test]
fn assembles_parallel_tool_calls() {
    let provider = provider_with(vec![Ok("tool_parallel")]);
    let mut events = vec![];
    let msg = provider
        .stream(&sample_request(), &mut |e| events.push(e))
        .unwrap();

    assert_eq!(msg.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(msg.content.len(), 3);
    assert_eq!(
        events[0],
        StreamEvent::TextDelta("I'll read the file and list the directory.".into())
    );
    assert_eq!(events[1], StreamEvent::ToolUseStarted { name: "read".into() });
    assert_eq!(events[2], StreamEvent::ToolUseStarted { name: "bash".into() });
    match &msg.content[1] {
        ContentBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "call_A1");
            assert_eq!(name, "read");
            assert_eq!(input, &serde_json::json!({"filePath": "/tmp/a.txt"}));
        }
        other => panic!("expected tool_use, got {other:?}"),
    }
    match &msg.content[2] {
        ContentBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "call_B2");
            assert_eq!(name, "bash");
            assert_eq!(input, &serde_json::json!({"command": "ls /tmp"}));
        }
        other => panic!("expected tool_use, got {other:?}"),
    }
    assert_eq!(msg.usage.output_tokens, Some(89));
    assert_eq!(msg.usage.cache_read_input_tokens, Some(256));
}

#[test]
fn content_filter_maps_to_refusal() {
    let provider = provider_with(vec![Ok("content_filter")]);
    let msg = provider.stream(&sample_request(), &mut |_| {}).unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::Refusal));
    assert!(msg.stop_details.is_none()); // no details on this wire
}

#[test]
fn refusal_deltas_map_to_refusal_with_explanation() {
    // SDK-capture-confirmed shape the hand-authored set missed: structured-
    // output refusals stream the text via delta.refusal with finish_reason
    // "stop". Must become a neutral Refusal with the text as explanation —
    // never a silent empty EndTurn.
    let provider = provider_with(vec![Ok("refusal_delta")]);
    let mut events = vec![];
    let msg = provider
        .stream(&sample_request(), &mut |e| events.push(e))
        .unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::Refusal));
    let details = msg.stop_details.expect("refusal carries details");
    assert_eq!(details.kind, "refusal");
    assert_eq!(
        details.explanation.as_deref(),
        Some("I'm sorry, I can't assist with that request.")
    );
    // Refused text is not streamed as regular output.
    assert!(events.is_empty());
    assert!(msg.content.is_empty());
}

#[test]
fn length_maps_to_max_tokens() {
    let provider = provider_with(vec![Ok("length_stop")]);
    let msg = provider.stream(&sample_request(), &mut |_| {}).unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::MaxTokens));
}

#[test]
fn midstream_error_envelope_becomes_api_error() {
    let provider = provider_with(vec![Ok("error_midstream")]);
    let err = provider.stream(&sample_request(), &mut |_| {}).unwrap_err();
    match err {
        ProviderError::Api { status, kind, message } => {
            assert_eq!(status, 200);
            assert_eq!(kind, "server_error");
            assert!(message.contains("overloaded"));
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[test]
fn done_only_stream_is_incomplete() {
    let provider = provider_with(vec![Ok("empty_stream")]);
    let err = provider.stream(&sample_request(), &mut |_| {}).unwrap_err();
    assert!(matches!(err, ProviderError::Incomplete));
}

#[test]
fn retries_429_with_retry_after_then_succeeds() {
    // Same shared retry policy as the Anthropic provider.
    let (provider, transport) = provider_and_transport(
        vec![
            Err(TransportError::Status {
                code: 429,
                retry_after: Some(0),
                body: r#"{"error":{"message":"rate limited","type":"rate_limit_exceeded"}}"#.into(),
            }),
            Ok("text_simple"),
        ],
        None,
    );
    let msg = provider.stream(&sample_request(), &mut |_| {}).unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(transport.bodies.borrow().len(), 2); // exactly one retry
}

#[test]
fn http_error_envelope_parsed_not_retried() {
    let (provider, transport) = provider_and_transport(
        vec![Err(TransportError::Status {
            code: 400,
            retry_after: None,
            body: r#"{"error":{"message":"we could not parse your request","type":"invalid_request_error","code":"invalid_json"}}"#
                .into(),
        })],
        None,
    );
    let err = provider.stream(&sample_request(), &mut |_| {}).unwrap_err();
    match err {
        ProviderError::Api { status, kind, message } => {
            assert_eq!(status, 400);
            assert_eq!(kind, "invalid_request_error");
            assert!(message.contains("could not parse"));
        }
        other => panic!("expected Api error, got {other:?}"),
    }
    assert_eq!(transport.bodies.borrow().len(), 1); // no retry
}

#[test]
fn bare_string_error_body_tolerated() {
    // Some local servers answer {"error": "message"} instead of the OpenAI
    // object shape. 500 is retryable, so the script serves it three times
    // (initial + MAX_RETRIES).
    let overloaded = || TransportError::Status {
        code: 500,
        retry_after: Some(0),
        body: r#"{"error":"model failed to load"}"#.into(),
    };
    let (provider, _) = provider_and_transport(
        vec![Err(overloaded()), Err(overloaded()), Err(overloaded())],
        None,
    );
    let err = provider.stream(&sample_request(), &mut |_| {}).unwrap_err();
    match err {
        ProviderError::Api { status, kind, message } => {
            assert_eq!(status, 500);
            assert_eq!(kind, "api_error");
            assert_eq!(message, "model failed to load");
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

// --- quirk fixtures: local servers (llama.cpp / Ollama) --------------------

#[test]
fn quirk_absent_usage_stays_none_never_zero() {
    let provider = provider_with(vec![Ok("quirk_no_usage")]);
    let msg = provider.stream(&sample_request(), &mut |_| {}).unwrap();
    assert_eq!(
        msg.content,
        vec![ContentBlock::Text { text: "Local hello.".into() }]
    );
    assert_eq!(msg.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(msg.usage, Usage::default()); // all None — "not reported"
    assert_eq!(msg.model, "llama3.2");
    assert_eq!(msg.id, ""); // no id on the wire; nothing invented
}

#[test]
fn quirk_absent_tool_call_id_is_synthesized() {
    let provider = provider_with(vec![Ok("quirk_no_ids")]);
    let msg = provider.stream(&sample_request(), &mut |_| {}).unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::ToolUse));
    match &msg.content[0] {
        ContentBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "call_0"); // deterministic synthesis
            assert_eq!(name, "read");
            assert_eq!(input, &serde_json::json!({"filePath": "/tmp/a.txt"}));
        }
        other => panic!("expected tool_use, got {other:?}"),
    }
}

#[test]
fn quirk_whole_call_in_one_chunk_without_index() {
    let mut events = vec![];
    let provider = provider_with(vec![Ok("quirk_whole_call")]);
    let msg = provider
        .stream(&sample_request(), &mut |e| events.push(e))
        .unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(events, vec![StreamEvent::ToolUseStarted { name: "bash".into() }]);
    match &msg.content[0] {
        ContentBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "call_0");
            assert_eq!(name, "bash");
            assert_eq!(input, &serde_json::json!({"command": "ls /tmp"}));
        }
        other => panic!("expected tool_use, got {other:?}"),
    }
}

#[test]
fn quirk_repeated_role_deltas_and_detailless_usage() {
    let provider = provider_with(vec![Ok("quirk_role_deltas")]);
    let msg = provider.stream(&sample_request(), &mut |_| {}).unwrap();
    assert_eq!(
        msg.content,
        vec![ContentBlock::Text { text: "ok computer".into() }]
    );
    assert_eq!(msg.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(msg.usage.input_tokens, Some(10));
    assert_eq!(msg.usage.output_tokens, Some(3));
    assert_eq!(msg.usage.cache_read_input_tokens, None); // details absent
}

// --- agent loop end-to-end over this provider ------------------------------

#[test]
fn agent_turn_round_trips_tool_calls_as_tool_messages() {
    // Full agent loop over the REAL provider + build_body: iteration 1 makes
    // two tool calls (fixture), iteration 2 ends the turn. Proves the
    // neutral agent core drives this wire with zero changes: the second
    // request must carry the assistant tool_calls and role:"tool" answers.
    use temur::agent::{Session, SessionConfig};
    use temur::tools::Registry;

    let (provider, transport) =
        provider_and_transport(vec![Ok("tool_parallel"), Ok("text_simple")], None);
    let dir = tempfile::tempdir().unwrap();
    let cfg = SessionConfig {
        model: "qwen2.5-coder-7b".into(),
        max_tokens: 8_000,
        system: Some("test system".into()),
        thinking: false,
        cwd: dir.path().to_path_buf(),
        max_iterations: 10,
        temperature: None,
        top_p: None,
        context_window: None,
    };
    let mut session = Session::new(Box::new(provider), Registry::standard(), cfg);
    session.turn("do the smoke task", &mut |_| {}).unwrap();

    let bodies = transport.bodies.borrow();
    assert_eq!(bodies.len(), 2, "tool round-trip = two provider calls");

    let b2: serde_json::Value = serde_json::from_str(&bodies[1]).unwrap();
    let msgs = b2["messages"].as_array().unwrap();
    // system, user, assistant(tool_calls), tool, tool
    let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, vec!["system", "user", "assistant", "tool", "tool"]);
    assert_eq!(msgs[2]["tool_calls"][0]["id"], "call_A1");
    assert_eq!(msgs[2]["tool_calls"][1]["id"], "call_B2");
    assert_eq!(msgs[3]["tool_call_id"], "call_A1");
    assert_eq!(msgs[4]["tool_call_id"], "call_B2");
    // arguments round-trip as strings
    let args = msgs[2]["tool_calls"][1]["function"]["arguments"].as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(args).unwrap(),
        serde_json::json!({"command": "ls /tmp"})
    );
}
