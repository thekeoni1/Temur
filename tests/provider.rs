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
        temperature: None,
        top_p: None,
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
    provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap();

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
fn sampling_knobs_absent_when_unset_and_mapped_when_set() {
    // None (the default everywhere today) sends nothing — pre-T1 behavior.
    let (provider, transport) = provider_and_transport(vec![Ok("text_simple")]);
    provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap();
    let body: serde_json::Value = serde_json::from_str(&transport.bodies.borrow()[0]).unwrap();
    assert!(body.get("temperature").is_none());
    assert!(body.get("top_p").is_none());

    // Set → mapped onto Anthropic's field names.
    let (provider, transport) = provider_and_transport(vec![Ok("text_simple")]);
    let mut req = sample_request();
    req.temperature = Some(0.5);
    req.top_p = Some(0.9);
    provider.stream(&req, &mut |_| {}, &CancelToken::new()).unwrap();
    let body: serde_json::Value = serde_json::from_str(&transport.bodies.borrow()[0]).unwrap();
    assert_eq!(body["temperature"], 0.5);
    assert_eq!(body["top_p"], 0.9);
}

#[test]
fn thinking_flag_adds_adaptive() {
    let (provider, transport) = provider_and_transport(vec![Ok("text_simple")]);
    let mut req = sample_request();
    req.thinking = true;
    provider.stream(&req, &mut |_| {}, &CancelToken::new()).unwrap();
    let body: serde_json::Value = serde_json::from_str(&transport.bodies.borrow()[0]).unwrap();
    assert_eq!(body["thinking"]["type"], "adaptive");
}

#[test]
fn streams_events_and_assembles_tool_use() {
    let provider = provider_with(vec![Ok("tool_use_parallel")]);
    let mut events = vec![];
    let msg = provider
        .stream(&sample_request(), &mut |e| events.push(e), &CancelToken::new())
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
    let msg = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap();
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
    let err = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap_err();
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
    let err = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap_err();
    match err {
        ProviderError::Api { kind, .. } => assert_eq!(kind, "overloaded_error"),
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[test]
fn refusal_completion_carries_stop_details() {
    let provider = provider_with(vec![Ok("refusal_pre_output")]);
    let msg = provider.stream(&sample_request(), &mut |_| {}, &CancelToken::new()).unwrap();
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
                    input_raw: None,
                },
                ContentBlock::ToolUse {
                    id: "tu_2".into(),
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
    provider.stream(&req, &mut |_| {}, &CancelToken::new()).unwrap();

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

// ---------------------------------------------------------------------------
// T6 cancellation (I1): the provider-side cancel seam. No threads, no timing
// races — a throttled reader sets the token deterministically at a byte
// offset computed from the fixture's own content.
// ---------------------------------------------------------------------------

use std::cell::Cell;
use std::rc::Rc;

fn fixture_bytes(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}.sse",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// Serves one in-memory SSE body in ≤16-byte reads, setting `token` once
/// `cancel_at` total bytes have been delivered and recording the total —
/// models a mid-stream Esc without any thread or clock.
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
) -> (AnthropicProvider, CancelToken, Rc<Cell<u64>>, u64) {
    let data = fixture_bytes(fixture);
    let total = data.len() as u64;
    let token = CancelToken::new();
    let delivered = Rc::new(Cell::new(0u64));
    let provider = AnthropicProvider::new(
        "https://api.example.test",
        "test-key-not-a-secret".into(),
        Box::new(CancellingTransport {
            data,
            cancel_at,
            token: token.clone(),
            delivered: delivered.clone(),
        }),
    );
    (provider, token, delivered, total)
}

#[test]
fn cancel_mid_stream_returns_partial_and_stops_reading() {
    // Cut while the content_block_start frame is being read: message_start
    // is fully accumulated, no text delta ever arrives.
    let data = fixture_bytes("text_simple");
    let cut = find(&data, br#""content_block":{"type":"text""#) as u64;
    let (provider, token, delivered, total) = cancelling_provider("text_simple", cut);

    let mut events = vec![];
    let msg = provider
        .stream(&sample_request(), &mut |e| events.push(e), &token)
        .unwrap();

    // Partial: the message started but no delta was consumed, and the stream
    // stopped mid-body instead of draining.
    assert_eq!(msg.id, "msg_01A");
    assert!(msg.stop_reason.is_none(), "no message_delta was reached");
    let text: String = msg
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "", "cancel landed before any text delta");
    assert!(events.is_empty(), "no UI events after the kept prefix");
    assert!(
        delivered.get() < total,
        "stream fully drained ({} of {total} bytes) despite cancel",
        delivered.get()
    );
}

#[test]
fn cancel_mid_tool_json_marks_input_raw() {
    // Cut inside the first real input_json_delta of the first tool call:
    // its content_block_stop never runs, so the partial JSON must surface
    // as input_raw (the agent's incomplete-block marker).
    let data = fixture_bytes("tool_use_parallel");
    let cut = find(&data, b"filePath") as u64;
    let (provider, token, delivered, total) = cancelling_provider("tool_use_parallel", cut);

    let msg = provider
        .stream(&sample_request(), &mut |_| {}, &token)
        .unwrap();

    assert!(msg.stop_reason.is_none());
    match &msg.content[..] {
        [ContentBlock::Text { text }, ContentBlock::ToolUse {
            name, input_raw, ..
        }] => {
            assert_eq!(text, "I'll read the file and list the directory.");
            assert_eq!(name, "read");
            assert_eq!(
                input_raw.as_deref(),
                Some("{\"filePath\":"),
                "incomplete tool JSON must be preserved raw"
            );
        }
        other => panic!("unexpected partial content: {other:?}"),
    }
    assert!(delivered.get() < total, "stream must not be fully drained");
}

#[test]
fn cancel_before_first_frame_is_incomplete_without_posting() {
    let (provider, transport) = provider_and_transport(vec![Ok("text_simple")]);
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

#[test]
fn retry_backoff_sleep_is_sliced_by_cancel() {
    // 429 with Retry-After: 30 would normally sleep 30 s before the retry;
    // the transport sets the token as it fails, so the sliced backoff must
    // notice within ~200 ms and return the pending error.
    struct FailingTransport(CancelToken);
    impl Transport for FailingTransport {
        fn post_stream(
            &self,
            _url: &str,
            _api_key: &str,
            _body: &str,
        ) -> Result<Box<dyn Read>, TransportError> {
            self.0.set();
            Err(TransportError::Status {
                code: 429,
                retry_after: Some(30),
                body: String::new(),
            })
        }
    }

    let token = CancelToken::new();
    let start = std::time::Instant::now();
    let err = match temur::provider::transport::post_stream_with_retries(
        &FailingTransport(token.clone()),
        "https://api.example.test/v1/messages",
        "test-key-not-a-secret",
        "{}",
        &token,
    ) {
        Err(e) => e,
        Ok(_) => panic!("expected the 429 to surface as an error"),
    };
    assert!(matches!(err, TransportError::Status { code: 429, .. }));
    assert!(
        start.elapsed() < std::time::Duration::from_secs(1),
        "backoff must abort promptly on cancel (took {:?})",
        start.elapsed()
    );
}

/// Byte offset of `needle` in `haystack` (first occurrence; panics if absent
/// so a fixture edit fails loudly here instead of hanging an assertion).
fn find(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
        .unwrap_or_else(|| panic!("fixture no longer contains {:?}", String::from_utf8_lossy(needle)))
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

/// F5(a): a read error while the cancel token is set returns Ok(partial) —
/// the already-streamed text survives instead of being thrown away.
#[test]
fn cancel_racing_read_error_keeps_streamed_partial() {
    let full = std::fs::read_to_string(format!(
        "{}/tests/fixtures/text_simple.sse",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let cut = full
        .find("event: message_delta")
        .expect("fixture has a message_delta to cut before");
    let cancel = CancelToken::new();
    let provider = AnthropicProvider::new(
        "https://api.example.test",
        "test-key-not-a-secret".into(),
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

/// Control: the same mid-stream failure WITHOUT the cancel is still a hard
/// stream error — F5 narrows nothing outside the interrupt path.
#[test]
fn read_error_without_cancel_is_still_an_error() {
    let full = std::fs::read_to_string(format!(
        "{}/tests/fixtures/text_simple.sse",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let cut = full.find("event: message_delta").unwrap();
    // The transport sets a token nobody polls; the provider's own token
    // stays clear.
    let provider = AnthropicProvider::new(
        "https://api.example.test",
        "test-key-not-a-secret".into(),
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

// ------------------------------------------------ T9: model-listing parse

/// Anthropic wire shape: GET /v1/models envelope with type/display_name
/// noise around each id.
#[test]
fn parse_models_json_anthropic_shape() {
    let body = r#"{
        "data": [
            {"type": "model", "id": "claude-sonnet-5", "display_name": "Claude Sonnet 5", "created_at": "2025-09-29T00:00:00Z"},
            {"type": "model", "id": "claude-opus-4-8", "display_name": "Claude Opus 4.8", "created_at": "2025-08-05T00:00:00Z"}
        ],
        "has_more": false,
        "first_id": "claude-sonnet-5",
        "last_id": "claude-opus-4-8"
    }"#;
    assert_eq!(
        parse_models_json(body).unwrap(),
        vec!["claude-sonnet-5".to_string(), "claude-opus-4-8".to_string()]
    );
}

/// OpenAI-compat wire shape: llama.cpp-style GET /models envelope.
#[test]
fn parse_models_json_openai_shape() {
    let body = r#"{
        "object": "list",
        "data": [
            {"id": "/model.gguf", "object": "model", "created": 1753300000, "owned_by": "llamacpp",
             "meta": {"vocab_type": 2, "n_ctx_train": 40960}}
        ]
    }"#;
    assert_eq!(parse_models_json(body).unwrap(), vec!["/model.gguf".to_string()]);
}

#[test]
fn parse_models_json_empty_list_is_ok_and_empty() {
    assert_eq!(parse_models_json(r#"{"data": [], "has_more": false}"#).unwrap(), Vec::<String>::new());
    assert_eq!(parse_models_json(r#"{"object":"list","data":[]}"#).unwrap(), Vec::<String>::new());
}

#[test]
fn parse_models_json_malformed_is_a_clean_error() {
    // Not JSON at all.
    let err = parse_models_json("<html>502 Bad Gateway</html>").unwrap_err().to_string();
    assert!(err.contains("bad JSON"), "{err}");
    // JSON but no data array.
    let err = parse_models_json(r#"{"models": ["x"]}"#).unwrap_err().to_string();
    assert!(err.contains("data"), "{err}");
    // data present but not an array.
    let err = parse_models_json(r#"{"data": "nope"}"#).unwrap_err().to_string();
    assert!(err.contains("data"), "{err}");
    // Entries without a string id are skipped, not errors.
    assert_eq!(
        parse_models_json(r#"{"data": [{"id": 7}, {"id": "ok-1"}, {"name": "x"}]}"#).unwrap(),
        vec!["ok-1".to_string()]
    );
}

// ------------------------------------- T15: keyless listing GET (hermetic)

/// One-shot canned HTTP server on 127.0.0.1: accepts a single connection,
/// captures the request head, answers with `body`, closes. Returns the
/// base URL to aim at and the join handle yielding the captured request.
fn one_shot_server(status_line: &str, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let response = format!(
        "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let handle = std::thread::spawn(move || {
        use std::io::Write;
        let (mut stream, _) = listener.accept().unwrap();
        let mut req = Vec::new();
        let mut buf = [0u8; 1024];
        // Read until the blank line ending the request head; a GET has no body.
        loop {
            let n = stream.read(&mut buf).unwrap();
            req.extend_from_slice(&buf[..n]);
            if n == 0 || req.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        stream.write_all(response.as_bytes()).unwrap();
        String::from_utf8_lossy(&req).into_owned()
    });
    (format!("http://127.0.0.1:{port}/v1"), handle)
}

const KEYLESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

#[test]
fn keyless_listing_gets_ids_and_sends_no_auth_header() {
    let (base, server) = one_shot_server(
        "HTTP/1.1 200 OK",
        r#"{"object":"list","data":[{"id":"served-a"},{"id":"served-b"}]}"#,
    );
    let ids = list_models_keyless(&base, KEYLESS_TIMEOUT).unwrap();
    assert_eq!(ids, vec!["served-a".to_string(), "served-b".to_string()]);
    let request = server.join().unwrap();
    let head = request.to_ascii_lowercase();
    assert!(head.starts_with("get /v1/models "), "path: {request}");
    // The T15 amendment in one assertion: NOTHING resembling credentials
    // may be on this wire.
    assert!(!head.contains("authorization"), "auth header sent: {request}");
    assert!(!head.contains("x-api-key"), "api key header sent: {request}");
}

#[test]
fn keyless_listing_http_error_is_a_clean_error_naming_the_status() {
    let (base, server) = one_shot_server("HTTP/1.1 503 Service Unavailable", "overloaded");
    let err = list_models_keyless(&base, KEYLESS_TIMEOUT).unwrap_err().to_string();
    assert!(err.contains("HTTP 503"), "{err}");
    server.join().unwrap();
}

#[test]
fn keyless_listing_refused_connection_is_a_clean_error() {
    // Bind, learn the port, drop: connecting there now fails fast.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let err = list_models_keyless(
        &format!("http://127.0.0.1:{port}/v1"),
        KEYLESS_TIMEOUT,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("model listing GET"), "{err}");
    assert!(err.contains(&format!("127.0.0.1:{port}")), "{err}");
}

#[test]
fn keyless_listing_bad_json_is_a_clean_error() {
    let (base, server) = one_shot_server("HTTP/1.1 200 OK", "<html>gateway</html>");
    let err = list_models_keyless(&base, KEYLESS_TIMEOUT).unwrap_err().to_string();
    assert!(err.contains("bad JSON"), "{err}");
    server.join().unwrap();
}
