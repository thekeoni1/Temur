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
    provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap();

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
    provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap();
    assert_eq!(transport.keys.borrow()[0], "");
}

#[test]
fn sampling_knobs_mapped_when_set() {
    let (provider, transport) =
        provider_and_transport(vec![Ok("text_simple")], None);
    let mut req = sample_request();
    req.temperature = Some(0.5);
    req.top_p = Some(0.9);
    provider.stream(&req, &mut |_| {}, &CancelToken::new()).unwrap();
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
                    input_raw: None,
                },
                ContentBlock::ToolUse {
                    id: "call_B2".into(),
                    name: "bash".into(),
                    input: serde_json::json!({"command": "ls /tmp"}),
                    input_raw: None,
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
    provider.stream(&req, &mut |_| {}, &CancelToken::new()).unwrap();

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
                input_raw: None,
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
    provider.stream(&req, &mut |_| {}, &CancelToken::new()).unwrap();
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
        .stream(&sample_request(), &mut |e| events.push(e), &CancelToken::new())
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
        .stream(&sample_request(), &mut |e| events.push(e), &CancelToken::new())
        .unwrap();

    assert_eq!(msg.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(events, vec![StreamEvent::ToolUseStarted { name: "read".into() }]);
    assert_eq!(msg.content.len(), 1);
    match &msg.content[0] {
        ContentBlock::ToolUse { id, name, input, .. } => {
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
        .stream(&sample_request(), &mut |e| events.push(e), &CancelToken::new())
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
        ContentBlock::ToolUse { id, name, input, .. } => {
            assert_eq!(id, "call_A1");
            assert_eq!(name, "read");
            assert_eq!(input, &serde_json::json!({"filePath": "/tmp/a.txt"}));
        }
        other => panic!("expected tool_use, got {other:?}"),
    }
    match &msg.content[2] {
        ContentBlock::ToolUse { id, name, input, .. } => {
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
    let msg = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap();
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
        .stream(&sample_request(), &mut |e| events.push(e), &CancelToken::new())
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
    let msg = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::MaxTokens));
    // T13 F10: the truncation is also recorded in stop_details, which is
    // how it survives the ToolUse override when calls were assembled too.
    assert_eq!(
        msg.stop_details.expect("truncation carries details").kind,
        "max_tokens"
    );
}

// --- T13 F10: assembled tool calls win over finish_reason ------------------

#[test]
fn stop_with_assembled_calls_still_means_tool_use() {
    // The T13 finding, live 2026-08-05 and pinned by curl: Gemini's compat
    // surface reports finish_reason "tool_calls" on the NON-streaming
    // response and "stop" on the STREAMING one for the identical request
    // with the identical call attached. temur streams. Mapping "stop"
    // faithfully made the agent loop discard a real, well-formed write call
    // in silence (no tool ran, no file appeared, the saved session was left
    // holding a tool_use with no tool_result). The call id and arguments
    // below are the ones from that captured session.
    let provider = provider_with(vec![Ok("gemini_stop_with_calls")]);
    let mut events = vec![];
    let msg = provider
        .stream(&sample_request(), &mut |e| events.push(e), &CancelToken::new())
        .unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::ToolUse));
    assert!(msg.stop_details.is_none()); // nothing was truncated
    assert_eq!(events, vec![StreamEvent::ToolUseStarted { name: "write".into() }]);
    match &msg.content[0] {
        ContentBlock::ToolUse { id, name, input, input_raw } => {
            assert_eq!(id, "guEZm7Du");
            assert_eq!(name, "write");
            assert_eq!(
                input,
                &serde_json::json!({
                    "filePath": "/tmp/t13-gemini.txt",
                    "content": "hello from gemini\n"
                })
            );
            assert_eq!(input_raw.as_deref(), None);
        }
        other => panic!("expected tool_use, got {other:?}"),
    }
}

#[test]
fn absent_finish_reason_with_calls_still_means_tool_use() {
    // The original quirk the F10 rule generalizes: a local server that ends
    // the stream without ever sending finish_reason. Unchanged behavior.
    let provider = provider_with(vec![Ok("quirk_no_finish_reason")]);
    let msg = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::ToolUse));
    assert!(msg.stop_details.is_none());
    match &msg.content[0] {
        ContentBlock::ToolUse { id, name, .. } => {
            assert_eq!(id, "call_L1");
            assert_eq!(name, "read");
        }
        other => panic!("expected tool_use, got {other:?}"),
    }
}

#[test]
fn content_filter_beats_assembled_calls() {
    // The one exception to the F10 override (P3.5 amendment): a FILTERED
    // completion must not dispatch side-effectful calls. The calls survive
    // in content, but the stop reason keeps the agent on its Refusal arm,
    // which shows a notice, discards, never auto-retries, and breaks
    // without pushing the assistant turn.
    let provider = provider_with(vec![Ok("content_filter_with_calls")]);
    let msg = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::Refusal));
    assert!(msg.stop_details.is_none()); // content_filter carries no details
    assert!(
        matches!(&msg.content[0], ContentBlock::ToolUse { name, .. } if name == "bash"),
        "the call is still reported, just not dispatched: {:?}",
        msg.content
    );
}

#[test]
fn refusal_text_beats_assembled_calls_too() {
    // The wire's other refusal signal, for consistency with the row above:
    // refusal text streams via delta.refusal (finish_reason "stop") while
    // tool calls were assembled. The refusal override runs last and wins,
    // carrying the explanation.
    let provider = provider_with(vec![Ok("refusal_delta_with_calls")]);
    let msg = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::Refusal));
    let details = msg.stop_details.as_ref().expect("refusal carries details");
    assert_eq!(details.kind, "refusal");
    assert_eq!(
        details.explanation.as_deref(),
        Some("I'm sorry, I can't assist with that request.")
    );
}

#[test]
fn tool_calls_finish_reason_is_untouched_by_the_override() {
    // The ordinary OpenAI path: same answer before and after F10.
    let provider = provider_with(vec![Ok("tool_fragmented")]);
    let msg = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::ToolUse));
    assert!(msg.stop_details.is_none());
}

#[test]
fn midstream_error_envelope_becomes_api_error() {
    let provider = provider_with(vec![Ok("error_midstream")]);
    let err = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap_err();
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
    let err = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap_err();
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
    let msg = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap();
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
    let err = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap_err();
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
    let err = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap_err();
    match err {
        ProviderError::Api { status, kind, message } => {
            assert_eq!(status, 500);
            assert_eq!(kind, "api_error");
            assert_eq!(message, "model failed to load");
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[test]
fn array_wrapped_error_body_is_unwrapped() {
    // T13 F9, the real shape captured live 2026-08-05: Google wraps the
    // OpenAI error envelope in a ONE-ELEMENT JSON ARRAY. The object-only
    // parser dropped it to defaults and printed
    // "api error (HTTP 404) api_error:" with no message, hiding the one
    // sentence that explained the failure (a retired model id). 404 is not
    // retryable, so one call is all it takes.
    let (provider, transport) = provider_and_transport(
        vec![Err(TransportError::Status {
            code: 404,
            retry_after: None,
            body: r#"[{"error":{"code":404,"message":"models/gemini-2.5-flash is not found for API version v1beta, or is not supported for generateContent.","status":"NOT_FOUND"}}]"#
                .into(),
        })],
        None,
    );
    let err = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap_err();
    match err {
        ProviderError::Api { status, kind, message } => {
            assert_eq!(status, 404);
            // No "type" and a NUMERIC "code": the label comes from "status".
            assert_eq!(kind, "NOT_FOUND");
            assert!(message.contains("is not found for API version"), "{message}");
        }
        other => panic!("expected Api error, got {other:?}"),
    }
    assert_eq!(transport.bodies.borrow().len(), 1); // no retry
}

// --- quirk fixtures: local servers (llama.cpp / Ollama) --------------------

#[test]
fn quirk_absent_usage_stays_none_never_zero() {
    let provider = provider_with(vec![Ok("quirk_no_usage")]);
    let msg = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap();
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
    let msg = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::ToolUse));
    match &msg.content[0] {
        ContentBlock::ToolUse { id, name, input, .. } => {
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
        .stream(&sample_request(), &mut |e| events.push(e), &CancelToken::new())
        .unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(events, vec![StreamEvent::ToolUseStarted { name: "bash".into() }]);
    match &msg.content[0] {
        ContentBlock::ToolUse { id, name, input, .. } => {
            assert_eq!(id, "call_0");
            assert_eq!(name, "bash");
            assert_eq!(input, &serde_json::json!({"command": "ls /tmp"}));
        }
        other => panic!("expected tool_use, got {other:?}"),
    }
}

#[test]
fn truncated_tool_arguments_preserved_as_input_raw() {
    // Arguments cut off by finish_reason "length": input stays {}, and the
    // raw fragment the model actually emitted survives as input_raw.
    //
    // T13 F10 changed the stop reason here: calls were assembled, so the
    // response dispatches (ToolUse) rather than ending the turn, and the
    // truncation rides in stop_details so the agent reports it as well. The
    // mangled trailing call is not executed either way: the agent's lossless
    // repair guard (T4) sees input_raw and feeds the error back instead.
    let provider = provider_with(vec![Ok("tool_truncated_args")]);
    let msg = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap();
    assert_eq!(msg.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(
        msg.stop_details
            .as_ref()
            .expect("truncation still surfaced")
            .kind,
        "max_tokens"
    );
    match &msg.content[0] {
        ContentBlock::ToolUse {
            id,
            name,
            input,
            input_raw,
        } => {
            assert_eq!(id, "call_T1");
            assert_eq!(name, "write");
            assert_eq!(input, &serde_json::json!({}));
            assert_eq!(input_raw.as_deref(), Some("{\"filePath\": \"notes"));
        }
        other => panic!("expected tool_use, got {other:?}"),
    }
}

#[test]
fn input_raw_never_reaches_the_wire() {
    // T4: two requests identical except one history tool_use carries
    // input_raw — the serialized bodies must be byte-identical, proving the
    // raw string is dropped at the neutral→wire conversion.
    let body_with = |input_raw: Option<String>| {
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
                    input_raw,
                }],
            },
            RequestMessage {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_A1".into(),
                    content: "arguments were not valid JSON".into(),
                    is_error: true,
                }],
            },
        ];
        provider.stream(&req, &mut |_| {}, &CancelToken::new()).unwrap();
        let body = transport.bodies.borrow()[0].clone();
        body
    };
    assert_eq!(
        body_with(None),
        body_with(Some("{\"filePath\": \"trunc".into())),
        "input_raw changed the OpenAI-compat request body"
    );
}

#[test]
fn quirk_repeated_role_deltas_and_detailless_usage() {
    let provider = provider_with(vec![Ok("quirk_role_deltas")]);
    let msg = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap();
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
        max_tokens_source: None,
        prose_tool_calls: true,
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

// ---------------------------------------------------------------------------
// T6 cancellation (I1): mirror of the Anthropic cancel-seam tests on this
// wire. Same deterministic throttled-reader technique — no threads, no clock.
// ---------------------------------------------------------------------------

use std::cell::Cell;
use std::rc::Rc;

fn fixture_bytes(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/openai/{name}.sse",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// Serves one in-memory SSE body in ≤16-byte reads, setting `token` once
/// `cancel_at` total bytes have been delivered and recording the total.
struct CancellingTransport {
    data: Vec<u8>,
    cancel_at: u64,
    token: CancelToken,
    delivered: Rc<Cell<u64>>,
}

struct ThrottledReader {
    data: Vec<u8>,
    pos: usize,
    cancel_at: u64,
    token: CancelToken,
    delivered: Rc<Cell<u64>>,
}

impl Read for ThrottledReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = buf.len().min(16).min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        self.delivered.set(self.delivered.get() + n as u64);
        if self.delivered.get() >= self.cancel_at {
            self.token.set();
        }
        Ok(n)
    }
}

impl Transport for CancellingTransport {
    fn post_stream(
        &self,
        _url: &str,
        _api_key: &str,
        _body: &str,
    ) -> Result<Box<dyn Read>, TransportError> {
        Ok(Box::new(ThrottledReader {
            data: self.data.clone(),
            pos: 0,
            cancel_at: self.cancel_at,
            token: self.token.clone(),
            delivered: self.delivered.clone(),
        }))
    }
}

fn cancelling_provider(
    fixture: &str,
    cancel_at: u64,
) -> (OpenAiCompatProvider, CancelToken, Rc<Cell<u64>>, u64) {
    let data = fixture_bytes(fixture);
    let total = data.len() as u64;
    let token = CancelToken::new();
    let delivered = Rc::new(Cell::new(0u64));
    let provider = OpenAiCompatProvider::new(
        "http://local.test/v1",
        None,
        Box::new(CancellingTransport {
            data,
            cancel_at,
            token: token.clone(),
            delivered: delivered.clone(),
        }),
    );
    (provider, token, delivered, total)
}

/// Byte offset of `needle` (first occurrence; panics loudly on fixture drift).
fn find(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
        .unwrap_or_else(|| panic!("fixture no longer contains {:?}", String::from_utf8_lossy(needle)))
}

#[test]
fn cancel_mid_stream_returns_partial_and_stops_reading() {
    // Cut inside the "Hello," chunk: that chunk is the last one accumulated,
    // " world!" and the finish chunk never arrive.
    let data = fixture_bytes("text_simple");
    let cut = find(&data, b"Hello,") as u64;
    let (provider, token, delivered, total) = cancelling_provider("text_simple", cut);

    let mut events = vec![];
    let msg = provider
        .stream(&sample_request(), &mut |e| events.push(e), &token)
        .unwrap();

    assert!(msg.stop_reason.is_none(), "finish_reason never arrived");
    match &msg.content[..] {
        [ContentBlock::Text { text }] => assert_eq!(text, "Hello,"),
        other => panic!("unexpected partial content: {other:?}"),
    }
    assert_eq!(events, vec![StreamEvent::TextDelta("Hello,".into())]);
    assert!(
        delivered.get() < total,
        "stream fully drained ({} of {total} bytes) despite cancel",
        delivered.get()
    );
}

#[test]
fn cancel_mid_tool_json_marks_input_raw() {
    // Cut inside the first argument fragment ("{\"fi"): the call's arguments
    // never complete, so assembly must attach them as input_raw.
    let data = fixture_bytes("tool_fragmented");
    let cut = find(&data, br#"{\"fi"#) as u64;
    let (provider, token, delivered, total) = cancelling_provider("tool_fragmented", cut);

    let msg = provider
        .stream(&sample_request(), &mut |_| {}, &token)
        .unwrap();

    match &msg.content[..] {
        [ContentBlock::ToolUse {
            name,
            input,
            input_raw,
            ..
        }] => {
            assert_eq!(name, "read");
            assert_eq!(input, &serde_json::json!({}));
            assert_eq!(
                input_raw.as_deref(),
                Some("{\"fi"),
                "incomplete tool JSON must be preserved raw"
            );
        }
        other => panic!("unexpected partial content: {other:?}"),
    }
    assert!(delivered.get() < total, "stream must not be fully drained");
}

#[test]
fn cancel_before_first_frame_is_incomplete_without_posting() {
    let (provider, transport) = provider_and_transport(vec![Ok("text_simple")], None);
    let token = CancelToken::new();
    token.set();
    let err = provider
        .stream(&sample_request(), &mut |_| {}, &token)
        .unwrap_err();
    assert!(matches!(err, ProviderError::Incomplete), "got {err:?}");
    assert!(
        transport.bodies.borrow().is_empty(),
        "a pre-set token must prevent the POST entirely"
    );
}

// ------------------------------------------------------------- F5 (v0.1.1)

/// Reader that serves a fixed prefix, then sets the cancel token and fails —
/// modeling a transport error racing the user's Esc with zero timing.
struct FailAfterPrefix {
    data: std::io::Cursor<Vec<u8>>,
    set_on_error: CancelToken,
}

impl Read for FailAfterPrefix {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.data.read(buf)?;
        if n == 0 {
            self.set_on_error.set();
            return Err(std::io::Error::other("connection reset mid-stream"));
        }
        Ok(n)
    }
}

struct PartialThenErrorTransport {
    prefix: Vec<u8>,
    set_on_error: CancelToken,
}

impl Transport for PartialThenErrorTransport {
    fn post_stream(
        &self,
        _url: &str,
        _api_key: &str,
        _body: &str,
    ) -> Result<Box<dyn Read>, TransportError> {
        Ok(Box::new(FailAfterPrefix {
            data: std::io::Cursor::new(self.prefix.clone()),
            set_on_error: self.set_on_error.clone(),
        }))
    }
}

/// F5(a), second wire: a read error while the cancel token is set returns
/// Ok(partial) — the already-streamed chunks survive.
#[test]
fn cancel_racing_read_error_keeps_streamed_partial() {
    let full = std::fs::read_to_string(format!(
        "{}/tests/fixtures/openai/text_simple.sse",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    // Cut before the finish_reason chunk (the 4th data line).
    let cut = full
        .match_indices("data: {")
        .nth(3)
        .expect("fixture has at least four chunks")
        .0;
    let cancel = CancelToken::new();
    let provider = OpenAiCompatProvider::new(
        "http://127.0.0.1:8080/v1",
        None,
        Box::new(PartialThenErrorTransport {
            prefix: full[..cut].into(),
            set_on_error: cancel.clone(),
        }),
    );
    let msg = provider
        .stream(&sample_request(), &mut |_| {}, &cancel)
        .expect("partial must survive an Err that races the cancel");
    assert!(
        matches!(&msg.content[0], ContentBlock::Text { text } if text == "Hello, world!"),
        "streamed text kept: {:?}",
        msg.content
    );
    assert!(msg.stop_reason.is_none(), "cut stream has no stop reason");
}

/// Control: the same failure with the token clear stays a hard error.
#[test]
fn read_error_without_cancel_is_still_an_error() {
    let full = std::fs::read_to_string(format!(
        "{}/tests/fixtures/openai/text_simple.sse",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let cut = full.match_indices("data: {").nth(3).unwrap().0;
    let provider = OpenAiCompatProvider::new(
        "http://127.0.0.1:8080/v1",
        None,
        Box::new(PartialThenErrorTransport {
            prefix: full[..cut].into(),
            set_on_error: CancelToken::new(),
        }),
    );
    let err = provider
        .stream(&sample_request(), &mut |_| {}, &CancelToken::new())
        .unwrap_err();
    assert!(matches!(err, ProviderError::Stream(_)), "got {err:?}");
}

// ---------------- T20 P3: prefix-stability invariant (OpenAI-compat wire) --
//
// Same invariant as the Anthropic side: requests are APPEND-ONLY, so a
// local server's prefix KV reuse (llama.cpp --cache-reuse) keeps working as
// the conversation grows. This wire has no cache_control at all, so the
// prefix must match byte for byte with no exemption.

#[test]
fn prefix_stability_compat_requests_are_append_only() {
    // H: system + user + assistant(text, thinking, tool_use) + tool result.
    // The conversion fans tool results out into their own wire messages;
    // the invariant is over the CONVERTED arrays: the first request's
    // messages must be a byte-identical prefix of the second's.
    let mut req_h = sample_request();
    req_h.messages = vec![
        RequestMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "start".into() }],
        },
        RequestMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: "plan".into(),
                    signature: None,
                },
                ContentBlock::Text { text: "working".into() },
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read".into(),
                    input: serde_json::json!({"filePath": "/tmp/a.txt"}),
                    input_raw: None,
                },
            ],
        },
        RequestMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".into(),
                content: "file contents".into(),
                is_error: false,
            }],
        },
    ];
    let mut req_h1 = req_h.clone();
    req_h1.messages.push(RequestMessage {
        role: Role::Assistant,
        content: vec![ContentBlock::Text { text: "done".into() }],
    });
    req_h1.messages.push(RequestMessage {
        role: Role::User,
        content: vec![ContentBlock::Text { text: "next".into() }],
    });

    let (p1, t1) = provider_and_transport(vec![Ok("text_simple")], None);
    p1.stream(&req_h, &mut |_| {}, &CancelToken::new()).unwrap();
    let (p2, t2) = provider_and_transport(vec![Ok("text_simple")], None);
    p2.stream(&req_h1, &mut |_| {}, &CancelToken::new()).unwrap();

    let b1: serde_json::Value = serde_json::from_str(&t1.bodies.borrow()[0]).unwrap();
    let b2: serde_json::Value = serde_json::from_str(&t2.bodies.borrow()[0]).unwrap();

    // Everything OUTSIDE messages (model, max_tokens, stream flags, tools)
    // is byte-identical: the appended exchange changes nothing else.
    let strip_messages = |v: &serde_json::Value| {
        let mut c = v.clone();
        c.as_object_mut().unwrap().remove("messages");
        to_sorted_json_string(&c).unwrap()
    };
    assert_eq!(strip_messages(&b1), strip_messages(&b2), "non-message body changed");

    // messages(H) is a byte prefix of messages(H+1). Element 0 is the
    // system message, so its stability is covered by the same loop.
    let m1 = b1["messages"].as_array().unwrap();
    let m2 = b2["messages"].as_array().unwrap();
    assert!(m2.len() > m1.len(), "appending must grow the array");
    for i in 0..m1.len() {
        assert_eq!(
            to_sorted_json_string(&m1[i]).unwrap(),
            to_sorted_json_string(&m2[i]).unwrap(),
            "message {i} was rewritten between H and H+1: the request is not append-only"
        );
    }
}
