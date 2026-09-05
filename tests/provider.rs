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
                    provider_state: None,
                },
                ContentBlock::ToolUse {
                    id: "tu_2".into(),
                    name: "bash".into(),
                    input: serde_json::json!({"command": "ls /tmp"}),
                    input_raw: None,
                    provider_state: None,
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
        cost_rates: None,
        cost_advisory_step_usd: temur::config::DEFAULT_COST_ADVISORY_STEP_USD,
        auto_compact: false,
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

// ------------------------------------------- T22: llama.cpp /props probe

#[test]
fn props_url_strips_a_trailing_v1_only() {
    assert_eq!(props_url("http://127.0.0.1:8080/v1"), "http://127.0.0.1:8080/props");
    assert_eq!(props_url("http://127.0.0.1:8080/v1/"), "http://127.0.0.1:8080/props");
    assert_eq!(props_url("http://127.0.0.1:8080"), "http://127.0.0.1:8080/props");
    // A non-/v1 path is a deliberate choice; only the SDK-conventional
    // suffix is rewritten.
    assert_eq!(props_url("http://host:9/custom"), "http://host:9/custom/props");
    assert_eq!(props_url("http://host:9/v1beta"), "http://host:9/v1beta/props");
}

#[test]
fn parse_props_context_reads_n_ctx_and_rejects_everything_else() {
    // Canned llama.cpp /props shape (fields around n_ctx are real noise).
    let body = r#"{
        "default_generation_settings": {
            "id": 0, "n_ctx": 8192, "speculative": false,
            "params": {"n_predict": -1, "temperature": 0.8}
        },
        "total_slots": 1, "model_path": "/model.gguf",
        "chat_template": "..."
    }"#;
    assert_eq!(parse_props_context(body), Some(8192));
    // Zero is not a usable window.
    assert_eq!(
        parse_props_context(r#"{"default_generation_settings":{"n_ctx":0}}"#),
        None
    );
    // Missing field, wrong type, wrong shape, not JSON: all None.
    assert_eq!(parse_props_context(r#"{"default_generation_settings":{}}"#), None);
    assert_eq!(
        parse_props_context(r#"{"default_generation_settings":{"n_ctx":"big"}}"#),
        None
    );
    assert_eq!(parse_props_context(r#"{"n_ctx": 4096}"#), None);
    assert_eq!(parse_props_context("<html>404</html>"), None);
}

#[test]
fn props_probe_reads_n_ctx_from_the_root_and_sends_no_auth_header() {
    let (base, server) = one_shot_server(
        "HTTP/1.1 200 OK",
        r#"{"default_generation_settings":{"n_ctx":16384},"total_slots":1}"#,
    );
    assert_eq!(probe_props_context(&base, KEYLESS_TIMEOUT), Some(16384));
    let request = server.join().unwrap();
    let head = request.to_ascii_lowercase();
    // The ROOT path: the base URL's /v1 is stripped for this endpoint.
    assert!(head.starts_with("get /props "), "path: {request}");
    // The amendment contract, same assertion as the keyless listing:
    // NOTHING resembling credentials may be on this wire.
    assert!(!head.contains("authorization"), "{request}");
    assert!(!head.contains("x-api-key"), "{request}");
    assert!(!head.contains("bearer"), "{request}");
}

#[test]
fn props_probe_http_error_and_bad_body_and_refusal_are_all_none() {
    let (base, server) = one_shot_server("HTTP/1.1 404 Not Found", "not found");
    assert_eq!(probe_props_context(&base, KEYLESS_TIMEOUT), None);
    server.join().unwrap();

    let (base, server) = one_shot_server("HTTP/1.1 200 OK", "<html>gateway</html>");
    assert_eq!(probe_props_context(&base, KEYLESS_TIMEOUT), None);
    server.join().unwrap();

    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    assert_eq!(
        probe_props_context(&format!("http://127.0.0.1:{port}/v1"), KEYLESS_TIMEOUT),
        None
    );
}

// ------------------------------------------- T31: tools-drop probe (POST)

/// POST-aware sibling of [`one_shot_server`]: reads the whole declared body
/// before answering, so the captured request includes it.
fn one_shot_post_server(
    status_line: &str,
    body: &'static str,
) -> (String, std::thread::JoinHandle<String>) {
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
        loop {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            req.extend_from_slice(&buf[..n]);
            let Some(h) = req.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&req[..h]).to_ascii_lowercase();
            let len: usize = head
                .split("content-length:")
                .nth(1)
                .and_then(|s| s.split(['\r', '\n']).next())
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            if req.len() >= h + 4 + len {
                break;
            }
        }
        stream.write_all(response.as_bytes()).unwrap();
        String::from_utf8_lossy(&req).into_owned()
    });
    (format!("http://127.0.0.1:{port}/v1"), handle)
}

/// The definitions a real session sends, exactly as doctor builds them.
fn session_defs() -> Vec<temur::provider::ToolDef> {
    temur::tools::Registry::standard_with_skills(vec![]).definitions()
}

#[test]
fn tools_drop_probe_body_differs_only_by_the_tools_array() {
    let defs = session_defs();
    let bare = tools_drop_probe_body("my-model", None, None);
    let with = tools_drop_probe_body("my-model", Some(&defs), None);
    // Both are tiny, capped at one generated token, and non-streaming, so
    // the usage block comes back in one response.
    for b in [&bare, &with] {
        let v: serde_json::Value = serde_json::from_str(b).unwrap();
        assert_eq!(v["model"], "my-model");
        assert_eq!(v["max_tokens"], 1);
        assert_eq!(v["stream"], false);
        assert_eq!(v["messages"][0]["role"], "user");
    }
    assert!(!bare.contains("\"tools\""), "{bare}");
    let v: serde_json::Value = serde_json::from_str(&with).unwrap();
    let tools = v["tools"].as_array().unwrap();
    // T34: the REAL registry, in the openai-compat wire shape the provider
    // itself emits. A synthetic minimal tool is exactly what made this
    // probe PASS a server that then rejected every real turn.
    assert_eq!(tools.len(), defs.len(), "every registered tool is probed");
    for (wire, def) in tools.iter().zip(defs.iter()) {
        assert_eq!(wire["type"], "function");
        assert_eq!(wire["function"]["name"], def.name);
        assert_eq!(wire["function"]["description"], def.description);
        assert_eq!(wire["function"]["parameters"], def.input_schema);
    }
    assert!(tools.iter().any(|t| t["function"]["name"] == "bash"), "{with}");
    // The schema the T34 interop fix is about really rides this wire.
    let skill = tools
        .iter()
        .find(|t| t["function"]["name"] == "skill")
        .expect("skill tool is registered unconditionally");
    assert_eq!(skill["function"]["parameters"]["properties"]["section"]["type"], "string");
}

#[test]
fn parse_prompt_tokens_reads_usage_and_rejects_everything_else() {
    assert_eq!(
        parse_prompt_tokens(r#"{"choices":[],"usage":{"prompt_tokens":31,"completion_tokens":1}}"#),
        Some(31)
    );
    // Zero is a real answer here (unlike n_ctx): a server that says the
    // prompt cost nothing is still saying something comparable.
    assert_eq!(parse_prompt_tokens(r#"{"usage":{"prompt_tokens":0}}"#), Some(0));
    // No usage block, wrong type, wrong shape, not JSON: all None.
    assert_eq!(parse_prompt_tokens(r#"{"choices":[]}"#), None);
    assert_eq!(parse_prompt_tokens(r#"{"usage":{}}"#), None);
    assert_eq!(parse_prompt_tokens(r#"{"usage":{"prompt_tokens":"ten"}}"#), None);
    assert_eq!(parse_prompt_tokens(r#"{"prompt_tokens":10}"#), None);
    assert_eq!(parse_prompt_tokens("<html>404</html>"), None);
}

/// T34: the error message a rejected probe carries back to doctor. Every
/// envelope shape the streaming path already absorbs, plus the degraded
/// cases, plus the one-line collapse a template traceback needs.
#[test]
fn parse_probe_error_message_absorbs_every_envelope_shape() {
    // The real llama.cpp answer to the Hermes template failure, shortened
    // (archive: template-experiment-2026-08-17/E2/a1-hermes-root-cause.txt).
    assert_eq!(
        parse_probe_error_message(
            r#"{"error":{"code":500,"message":"Unable to generate parser for this template.\nError: Object key of unhashable type: Array","type":"server_error"}}"#
        ),
        "Unable to generate parser for this template. Error: Object key of unhashable type: Array"
    );
    // Bare string under `error`, and Google's one-element array wrapper.
    assert_eq!(parse_probe_error_message(r#"{"error":"nope"}"#), "nope");
    assert_eq!(
        parse_probe_error_message(r#"[{"error":{"message":"model not found"}}]"#),
        "model not found"
    );
    // Not an error envelope at all: the raw text still beats silence.
    assert_eq!(parse_probe_error_message("<html>404</html>"), "<html>404</html>");
    // An envelope with an empty message degrades the same way.
    assert_eq!(parse_probe_error_message(r#"{"error":{"message":""}}"#), r#"{"error":{"message":""}}"#);
    // Long messages are capped, so doctor stays one line per check.
    let long = format!(r#"{{"error":{{"message":"{}"}}}}"#, "x".repeat(1000));
    let out = parse_probe_error_message(&long);
    assert!(out.ends_with("..."), "{out}");
    assert!(out.chars().count() < 400, "{}", out.chars().count());
}

#[test]
fn tools_drop_probe_posts_to_chat_completions_and_sends_no_auth_header() {
    let (base, server) = one_shot_post_server(
        "HTTP/1.1 200 OK",
        r#"{"choices":[],"usage":{"prompt_tokens":31}}"#,
    );
    let defs = session_defs();
    assert_eq!(
        probe_prompt_tokens(&base, "m", Some(&defs), None, KEYLESS_TIMEOUT),
        ProbeOutcome::Ok(31)
    );
    let request = server.join().unwrap();
    let head = request.to_ascii_lowercase();
    assert!(head.starts_with("post /v1/chat/completions "), "path: {request}");
    // The amendment contract, same assertion as the two keyless GETs:
    // NOTHING resembling credentials may be on this wire. T34 widened what
    // the probe sends, not what it may attach.
    assert!(!head.contains("authorization"), "{request}");
    assert!(!head.contains("x-api-key"), "{request}");
    assert!(!head.contains("bearer"), "{request}");
    // The tools array really went out; the probe is worthless otherwise.
    assert!(request.contains("\"tools\""), "{request}");
    assert!(request.contains("\"skill\""), "{request}");
}

#[test]
fn tools_drop_probe_http_error_carries_the_status_and_the_servers_words() {
    // The Hermes shape: the server answers, and says exactly why.
    let (base, server) = one_shot_post_server(
        "HTTP/1.1 400 Bad Request",
        r#"{"error":{"message":"Object key of unhashable type: Array","type":"server_error"}}"#,
    );
    assert_eq!(
        probe_prompt_tokens(&base, "m", None, None, KEYLESS_TIMEOUT),
        ProbeOutcome::HttpError {
            status: 400,
            message: "Object key of unhashable type: Array".into()
        }
    );
    server.join().unwrap();

    let (base, server) = one_shot_post_server("HTTP/1.1 404 Not Found", "not found");
    assert_eq!(
        probe_prompt_tokens(&base, "m", None, None, KEYLESS_TIMEOUT),
        ProbeOutcome::HttpError {
            status: 404,
            message: "not found".into()
        }
    );
    server.join().unwrap();
}

#[test]
fn tools_drop_probe_unusable_body_is_no_usage_and_a_dead_port_is_unreachable() {
    let (base, server) = one_shot_post_server("HTTP/1.1 200 OK", "<html>gateway</html>");
    assert_eq!(
        probe_prompt_tokens(&base, "m", None, None, KEYLESS_TIMEOUT),
        ProbeOutcome::NoUsage
    );
    server.join().unwrap();

    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    assert_eq!(
        probe_prompt_tokens(&format!("http://127.0.0.1:{port}/v1"), "m", None, None, KEYLESS_TIMEOUT),
        ProbeOutcome::Unreachable
    );
}

// -------------------------- T22: listing entries carry max_input_tokens

#[test]
fn parse_models_entries_reads_max_input_tokens_zero_or_absent_is_unknown() {
    // Anthropic wire shape with the documented max_input_tokens field;
    // one entry carries 0 (unknown) and one omits it entirely.
    let body = r#"{
        "data": [
            {"type": "model", "id": "claude-sonnet-5", "display_name": "Claude Sonnet 5",
             "max_input_tokens": 200000},
            {"type": "model", "id": "claude-opus-4-8", "max_input_tokens": 0},
            {"type": "model", "id": "claude-haiku-4-5"}
        ],
        "has_more": false
    }"#;
    let entries = parse_models_entries(body).unwrap();
    assert_eq!(
        entries,
        vec![
            ModelEntry { id: "claude-sonnet-5".into(), context_window: Some(200_000) },
            ModelEntry { id: "claude-opus-4-8".into(), context_window: None },
            ModelEntry { id: "claude-haiku-4-5".into(), context_window: None },
        ]
    );
    // The id-only view is unchanged for the same body.
    assert_eq!(
        parse_models_json(body).unwrap(),
        vec!["claude-sonnet-5", "claude-opus-4-8", "claude-haiku-4-5"]
    );
    // OpenAI-compat shape: no such field, windows all None.
    let compat = r#"{"object":"list","data":[{"id":"/model.gguf","owned_by":"llamacpp"}]}"#;
    assert_eq!(
        parse_models_entries(compat).unwrap(),
        vec![ModelEntry { id: "/model.gguf".into(), context_window: None }]
    );
}

// ------------------------------- T50: chat transport timeouts (hermetic)

/// Accepts one connection and NEVER writes a byte, holding it open until
/// the returned sender is dropped. This is the shape the sandbox relay took
/// when it went quiet mid-session on 2026-09-05: the TCP handshake
/// completes, so nothing at the socket layer reports trouble, and the
/// client waits forever for a status line that never comes.
///
/// Held open deliberately. A listener that merely closed would surface as
/// an ordinary connection error and would prove nothing about timeouts.
fn silent_endpoint() -> (String, std::sync::mpsc::Sender<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _ = rx.recv();
        drop(stream);
    });
    (format!("http://127.0.0.1:{port}/v1/messages"), tx)
}

/// Run `body` on its own thread and fail if it does not finish inside
/// `bound`. The T48 P2 mechanism, for the T48 P2 reason: a hung call cannot
/// be unwound in place, and aborting the process would lose every other
/// result in the suite.
fn within<T: Send + 'static>(bound: std::time::Duration, what: &str, body: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(body());
    });
    match rx.recv_timeout(bound) {
        Ok(v) => v,
        Err(_) => panic!("{what} did not return within {bound:?}"),
    }
}

/// The short bounds every test below drives the REAL agent with. The
/// production constants are asserted separately; waiting out 60 real
/// seconds in the suite would buy nothing this does not already show.
const T: std::time::Duration = std::time::Duration::from_millis(900);

#[test]
fn a_silent_endpoint_cannot_hang_a_chat_turn() {
    // The regression. On the parent commit this call never returned: the
    // archived probe held it for 90.00s against this exact listener shape
    // before its watchdog fired.
    use temur::provider::anthropic::transport::HttpTransport;
    let (url, _keep) = silent_endpoint();
    let (elapsed, err) = within(std::time::Duration::from_secs(20), "post_stream", move || {
        let t = HttpTransport::with_timeouts(T, T, T);
        let start = std::time::Instant::now();
        let r = t.post_stream(&url, "test-key", "{}");
        (start.elapsed(), r.err())
    });
    let err = err.expect("a silent endpoint must be an error, not a hang");
    assert!(
        matches!(err, TransportError::Timeout { .. }),
        "a silent endpoint is a timeout, not a generic io error: {err:?}"
    );
    assert!(elapsed < std::time::Duration::from_secs(10), "returned in {elapsed:?}");
}

#[test]
fn the_openai_compat_transport_is_bounded_too() {
    // Both transports, one shared agent constructor: the point of sharing
    // is that this cannot be true of one provider and false of the other.
    use temur::provider::openai_compat::transport::HttpTransport;
    let (url, _keep) = silent_endpoint();
    let err = within(std::time::Duration::from_secs(20), "post_stream", move || {
        HttpTransport::with_timeouts(T, T, T)
            .post_stream(&url, "test-key", "{}")
            .err()
    })
    .expect("a silent endpoint must be an error, not a hang");
    assert!(matches!(err, TransportError::Timeout { .. }), "{err:?}");
}

#[test]
fn a_response_timeout_is_not_retried() {
    // The composition that would otherwise recreate the symptom this
    // milestone removes: retrying a 60s response timeout twice is 186s of
    // silence. A connect-phase timeout stays retryable, matching what a
    // refused connection already does.
    let recv = TransportError::Timeout {
        phase: "receive response".into(),
        retryable: false,
    };
    assert!(!recv.retryable(), "a silent endpoint must not be re-POSTed");
    assert_eq!(recv.retry_after(), None);
    let connect = TransportError::Timeout {
        phase: "connect".into(),
        retryable: true,
    };
    assert!(connect.retryable(), "an unreachable host is worth another try");
}

#[test]
fn a_timeout_renders_as_an_ordinary_turn_error() {
    // Control returns and the session stays intact: the timeout arrives on
    // the normal network-error path, not as a new error class the UIs would
    // have to learn.
    let (url, _keep) = silent_endpoint();
    // Built inside the thread: AnthropicProvider holds a Box<dyn Transport>,
    // which is not Send, so only the URL crosses the boundary.
    let err = within(std::time::Duration::from_secs(20), "turn", move || {
        let provider = AnthropicProvider::new(
            url.trim_end_matches("/v1/messages").to_string(),
            "test-key".into(),
            Box::new(temur::provider::anthropic::transport::HttpTransport::with_timeouts(T, T, T)),
        );
        let cancel = temur::cancel::CancelToken::new();
        provider
            .stream(&sample_request(), &mut |_| {}, &cancel)
            .err()
    })
    .expect("a silent endpoint must end the turn, not hang it");
    let msg = err.to_string();
    assert!(msg.starts_with("network: "), "ordinary network error: {msg}");
    assert!(msg.contains("timed out"), "{msg}");
}

#[test]
fn esc_lands_within_the_timeout_bound() {
    // ESC ACCEPTANCE. Esc sets the cancel token; the blocked read cannot be
    // interrupted in place (see the deliberate non-goal), so what makes Esc
    // land is that the read RETURNS within the bound and the retry loop
    // then sees the token. Asserting elapsed against a generous multiple of
    // the bound, not a tight one, so contention cannot make this flaky.
    let (url, _keep) = silent_endpoint();
    let cancel = temur::cancel::CancelToken::new();
    let c2 = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        c2.set(); // the Esc keypress
    });
    let (elapsed, err) = within(std::time::Duration::from_secs(20), "turn", move || {
        let t = temur::provider::anthropic::transport::HttpTransport::with_timeouts(T, T, T);
        let start = std::time::Instant::now();
        let r = temur::provider::transport::post_stream_with_retries(
            &t, &url, "test-key", "{}", &cancel,
        );
        (start.elapsed(), r.err())
    });
    assert!(err.is_some(), "the turn must end");
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "Esc landed only after {elapsed:?}"
    );
}

#[test]
fn the_production_bounds_are_the_documented_ones() {
    // The tests above drive short bounds; this is what ships. A change to
    // either number is a deliberate act that lands here.
    assert_eq!(temur::provider::transport::CHAT_CONNECT_TIMEOUT_SECS, 10);
    assert_eq!(temur::provider::transport::CHAT_RESPONSE_HEAD_TIMEOUT_SECS, 60);
    assert_eq!(temur::provider::transport::CHAT_STREAM_IDLE_TIMEOUT_SECS, 120);
}

/// Sends a response head, three quick SSE chunks, then PAUSES for `pause`
/// before a fourth chunk and close. The pause is the discriminator: it is
/// longer than the one-second tolerance a lapsed absolute deadline degrades
/// to, and shorter than a real idle bound.
fn slow_but_alive_then_pause(pause: std::time::Duration) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        use std::io::Write;
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let head = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
        if stream.write_all(head).is_err() {
            return;
        }
        let _ = stream.flush();
        for _ in 0..3 {
            std::thread::sleep(std::time::Duration::from_millis(150));
            if stream.write_all(b"data: {\"x\":1}\n\n").is_err() {
                return;
            }
            let _ = stream.flush();
        }
        std::thread::sleep(pause);
        let _ = stream.write_all(b"data: {\"x\":1}\n\n");
        let _ = stream.flush();
    });
    format!("http://127.0.0.1:{port}/v1/messages")
}

#[test]
fn a_healthy_stream_is_never_cut_off_however_long_it_runs() {
    // THE TEST THAT CAUGHT THE FIRST IMPLEMENTATION, and it took two
    // attempts to make it actually discriminate.
    //
    // Wiring the response-head bound to `timeout_recv_response` (the
    // obvious knob, and the one the brief named) leaks into the body,
    // because RecvBody checks RecvResponse as a preceeding phase. It does
    // NOT hard-cap the stream, which is what made the first version of this
    // test pass under the broken wiring and therefore prove nothing: once
    // an absolute deadline is in the past, ureq's `NextTimeout::not_zero`
    // degrades it to a ONE SECOND per-read timeout instead of failing. So
    // the real symptom is subtler than a cap: after the head bound elapses,
    // the stream silently tolerates only ~1s of silence per read.
    //
    // Hence these numbers. The head bound (300ms) expires early. Then a
    // 1500ms gap arrives, longer than the 1s degraded tolerance and shorter
    // than the real idle bound (4s). Correct wiring streams through it;
    // the naive wiring dies on it.
    use temur::provider::anthropic::transport::HttpTransport;
    let head = std::time::Duration::from_millis(300);
    let idle = std::time::Duration::from_secs(4);
    let url = slow_but_alive_then_pause(std::time::Duration::from_millis(1500));
    let read = within(std::time::Duration::from_secs(30), "slow stream", move || {
        let t = HttpTransport::with_timeouts(head, head, idle);
        let mut reader = t.post_stream(&url, "test-key", "{}").expect("head must arrive");
        let mut all = Vec::new();
        let mut buf = [0u8; 256];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => all.extend_from_slice(&buf[..n]),
                Err(e) => return Err(format!("{e:?}")),
            }
        }
        Ok(String::from_utf8_lossy(&all).into_owned())
    });
    let body = read.expect("a stream that keeps producing must not be cut off");
    assert_eq!(
        body.matches("data: ").count(),
        4,
        "every chunk must arrive, including the one after the long pause: {body:?}"
    );
}

#[test]
fn a_stalled_stream_is_bounded_by_the_idle_timeout() {
    // The other half of the same knob: silence mid-stream IS bounded, and
    // by the idle constant rather than by anything total.
    use temur::provider::anthropic::transport::HttpTransport;
    let (url, _keep) = stalls_after_headers();
    let (elapsed, err) = within(std::time::Duration::from_secs(30), "stalled stream", move || {
        let t = HttpTransport::with_timeouts(T, T, T);
        let mut reader = t.post_stream(&url, "test-key", "{}").expect("head must arrive");
        let start = std::time::Instant::now();
        let mut buf = [0u8; 256];
        let _ = reader.read(&mut buf); // the one real chunk
        let e = reader.read(&mut buf).err();
        (start.elapsed(), e)
    });
    assert!(err.is_some(), "a stalled stream must end, not hang");
    assert!(elapsed < std::time::Duration::from_secs(15), "ended only after {elapsed:?}");
}

/// Sends a complete response head and one SSE chunk, then goes silent while
/// holding the connection open.
fn stalls_after_headers() -> (String, std::sync::mpsc::Sender<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        use std::io::Write;
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let head = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
        let _ = stream.write_all(head);
        let _ = stream.write_all(b"data: {\"x\":1}\n\n");
        let _ = stream.flush();
        let _ = rx.recv();
        drop(stream);
    });
    (format!("http://127.0.0.1:{port}/v1/messages"), tx)
}
