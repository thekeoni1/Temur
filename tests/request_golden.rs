//! T1 wire-freeze suite: the exact request body the Anthropic provider
//! produces for representative conversations, compared byte-for-byte against
//! golden captures taken BEFORE the provider-neutral refactor. Any diff means
//! the Anthropic wire output changed — which T1 forbids.
//!
//! Regenerate only as a deliberate wire-format decision (never to make a
//! failing test pass):  GOLDEN_REGEN=1 cargo test --test request_golden

use temur::provider::anthropic::transport::{Transport, TransportError};
use temur::provider::anthropic::AnthropicProvider;
use temur::provider::*;
use std::cell::RefCell;
use std::io::Read;
use std::rc::Rc;

/// Records every request body; always replies with the text_simple fixture.
struct RecordingTransport {
    bodies: Rc<RefCell<Vec<String>>>,
}

impl Transport for RecordingTransport {
    fn post_stream(
        &self,
        _url: &str,
        _api_key: &str,
        body: &str,
    ) -> Result<Box<dyn Read>, TransportError> {
        self.bodies.borrow_mut().push(body.to_string());
        let path = format!(
            "{}/tests/fixtures/text_simple.sse",
            env!("CARGO_MANIFEST_DIR")
        );
        Ok(Box::new(std::fs::File::open(path).unwrap()))
    }
}

fn body_for(req: &ChatRequest) -> String {
    let bodies = Rc::new(RefCell::new(vec![]));
    let provider = AnthropicProvider::new(
        "https://api.example.test",
        "test-key-not-a-secret".into(),
        Box::new(RecordingTransport {
            bodies: bodies.clone(),
        }),
    );
    provider.stream(req, &mut |_| {}, &CancelToken::new()).unwrap();
    let body = bodies.borrow()[0].clone();
    body
}

fn check_golden(name: &str, req: &ChatRequest) {
    let dir = format!("{}/tests/fixtures/golden", env!("CARGO_MANIFEST_DIR"));
    let path = format!("{dir}/{name}.request.json");
    let body = body_for(req);
    if std::env::var_os("GOLDEN_REGEN").is_some() {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, &body).unwrap();
    }
    let golden = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{path}: {e} (generate with GOLDEN_REGEN=1)"));
    assert_eq!(
        body, golden,
        "{name}: request body differs byte-for-byte from the pre-T1 golden — the wire format changed"
    );
}

fn user_text(t: &str) -> RequestMessage {
    RequestMessage {
        role: Role::User,
        content: vec![ContentBlock::Text { text: t.into() }],
    }
}

fn tool(name: &str) -> ToolDef {
    ToolDef {
        name: name.into(),
        description: format!("The {name} tool"),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"arg": {"type": "string"}},
            "required": ["arg"]
        }),
    }
}

fn base_request() -> ChatRequest {
    ChatRequest {
        model: "claude-sonnet-5".into(),
        max_tokens: 32_000,
        system: Some("You are a coding agent.".into()),
        thinking: false,
        temperature: None,
        top_p: None,
        messages: vec![user_text("hi")],
        tools: vec![tool("read")],
    }
}

#[test]
fn golden_simple() {
    // System + one tool + one user message, thinking off.
    check_golden("simple", &base_request());
}

#[test]
fn golden_bare() {
    // No system, no tools: both keys must be absent entirely.
    let mut req = base_request();
    req.system = None;
    req.tools = vec![];
    check_golden("bare", &req);
}

#[test]
fn golden_tool_history() {
    // Mid-turn agent shape: user → assistant(text + 2 tool_use) →
    // user(2 tool_results, one is_error) — covers tool_use input
    // serialization, is_error true (serialized) vs false (skipped), and the
    // moving cache breakpoint landing on the final tool_result.
    let mut req = base_request();
    req.tools = vec![tool("read"), tool("bash")];
    req.messages = vec![
        user_text("start"),
        RequestMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "working".into(),
                },
                ContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "read".into(),
                    input: serde_json::json!({"arg": "/tmp/a.txt"}),
                    input_raw: None,
                    provider_state: None,
                },
                ContentBlock::ToolUse {
                    id: "tu_2".into(),
                    name: "bash".into(),
                    input: serde_json::json!({"arg": "ls /tmp"}),
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
                    content: "boom".into(),
                    is_error: true,
                },
            ],
        },
    ];
    check_golden("tool_history", &req);
}

#[test]
fn golden_thinking_history() {
    // Thinking on; history carries thinking (with signature),
    // redacted_thinking, and text blocks round-tripped back to the API.
    let mut req = base_request();
    req.thinking = true;
    req.tools = vec![];
    req.messages = vec![
        user_text("solve it"),
        RequestMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: "Let me think.".into(),
                    signature: Some("EqQBCgIYAhIkSig=".into()),
                },
                ContentBlock::RedactedThinking {
                    data: "opaque-blob".into(),
                },
                ContentBlock::Text {
                    text: "The answer is 4.".into(),
                },
            ],
        },
        user_text("next step"),
    ];
    check_golden("thinking_history", &req);
}

#[test]
fn golden_pause_resume() {
    // pause_turn resume shape: history ends with the ASSISTANT partial whose
    // final block is thinking — the moving breakpoint must fall back to the
    // preceding text block, byte-stable.
    let mut req = base_request();
    req.tools = vec![];
    req.messages = vec![
        user_text("long task"),
        RequestMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "part one...".into(),
                },
                // Signed, as every Anthropic thinking block is: an unsigned
                // one came from another wire and is not sent (T66 P1 amend).
                ContentBlock::Thinking {
                    thinking: "continuing".into(),
                    signature: Some("EqQBCgIYAhIkSig=".into()),
                },
            ],
        },
    ];
    check_golden("pause_resume", &req);
}

// ------------------- T20 P3: prefix-stability invariant (Anthropic wire) --
//
// The cache economics of /compact and the context advisory rest on requests
// being APPEND-ONLY: growing the history must never rewrite what came
// before, or the provider-side prefix cache (and llama.cpp KV reuse) is
// silently useless. The moving cache breakpoint is the one legitimate
// difference: it leaves the block that is no longer last.

/// Remove every `cache_control` key, recursively.
fn strip_cache_control(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(m) => {
            m.remove("cache_control");
            for (_, val) in m.iter_mut() {
                strip_cache_control(val);
            }
        }
        serde_json::Value::Array(a) => {
            for val in a.iter_mut() {
                strip_cache_control(val);
            }
        }
        _ => {}
    }
}

/// Canonical bytes of a subvalue: the same sorted-key serialization the
/// whole request body already uses on this wire.
fn canon(v: &serde_json::Value) -> String {
    to_sorted_json_string(v).unwrap()
}

#[test]
fn prefix_stability_anthropic_requests_are_append_only() {
    // H: a realistic mid-conversation history covering text, a signed
    // thinking block, and a tool_use/tool_result pair. H+1: the exact same
    // history plus one appended exchange.
    let mut req_h = base_request();
    req_h.tools = vec![tool("read"), tool("bash")];
    req_h.messages = vec![
        user_text("start"),
        RequestMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: "plan".into(),
                    signature: Some("EqQBCgIYAhIkSig=".into()),
                },
                ContentBlock::Text {
                    text: "working".into(),
                },
                ContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "read".into(),
                    input: serde_json::json!({"arg": "/tmp/a.txt"}),
                    input_raw: None,
                    provider_state: None,
                },
            ],
        },
        RequestMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "tu_1".into(),
                content: "file contents".into(),
                is_error: false,
            }],
        },
    ];
    let mut req_h1 = req_h.clone();
    req_h1.messages.push(RequestMessage {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "done".into(),
        }],
    });
    req_h1.messages.push(user_text("next"));

    let b1: serde_json::Value = serde_json::from_str(&body_for(&req_h)).unwrap();
    let b2: serde_json::Value = serde_json::from_str(&body_for(&req_h1)).unwrap();

    // system and tools: byte-identical across the two requests.
    assert_eq!(canon(&b1["system"]), canon(&b2["system"]), "system block changed");
    assert_eq!(canon(&b1["tools"]), canon(&b2["tools"]), "tools block changed");

    // The first |H| serialized message elements: byte-identical MODULO
    // cache_control markers.
    let m1 = b1["messages"].as_array().unwrap();
    let m2 = b2["messages"].as_array().unwrap();
    assert_eq!(m1.len(), 3);
    assert_eq!(m2.len(), 5);
    for i in 0..m1.len() {
        let mut a = m1[i].clone();
        let mut b = m2[i].clone();
        strip_cache_control(&mut a);
        strip_cache_control(&mut b);
        assert_eq!(
            canon(&a),
            canon(&b),
            "message {i} was rewritten between H and H+1: the request is not append-only"
        );
    }

    // The cache_control difference really is just the ONE moving
    // message-level breakpoint per request.
    let marker_count = |v: &serde_json::Value| {
        serde_json::to_string(v).unwrap().matches("cache_control").count()
    };
    assert_eq!(marker_count(&b1["messages"]), 1);
    assert_eq!(marker_count(&b2["messages"]), 1);
}

#[test]
fn input_raw_never_reaches_the_wire() {
    // T4: two requests identical except one history tool_use carries
    // input_raw — the serialized bodies must be byte-identical, proving the
    // raw string is dropped at the neutral→wire conversion.
    let body_with = |input_raw: Option<String>| {
        let mut req = base_request();
        req.messages = vec![
            user_text("start"),
            RequestMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "read".into(),
                    input: serde_json::json!({}),
                    input_raw,
                    provider_state: None,
                }],
            },
            RequestMessage {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "tu_1".into(),
                    content: "arguments were not valid JSON".into(),
                    is_error: true,
                }],
            },
        ];
        body_for(&req)
    };
    assert_eq!(
        body_with(None),
        body_with(Some("{\"filePath\": \"trunc".into())),
        "input_raw changed the Anthropic request body"
    );
}

#[test]
fn provider_state_never_reaches_the_anthropic_wire() {
    // T13 F12: Gemini's thought signature is another wire's round-trip
    // state. The Anthropic converter drops it, exactly as openai-compat
    // drops Anthropic thinking signatures, so a history that switched
    // providers mid-session cannot leak one provider's opaque state into
    // the other's request. Byte-identical bodies is the assertion.
    let body_with = |provider_state: Option<serde_json::Value>| {
        let mut req = base_request();
        req.messages = vec![
            user_text("start"),
            RequestMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "read".into(),
                    input: serde_json::json!({"arg": "/tmp/a.txt"}),
                    input_raw: None,
                    provider_state,
                }],
            },
            RequestMessage {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "tu_1".into(),
                    content: "ok".into(),
                    is_error: false,
                }],
            },
        ];
        body_for(&req)
    };
    assert_eq!(
        body_with(None),
        body_with(Some(
            serde_json::json!({"google": {"thought_signature": "EsQBCsEB-opaque"}})
        )),
        "provider_state changed the Anthropic request body"
    );
}

// ------------------------------------------- T55: project instructions
//
// Not golden captures: these use this file's recording seam to prove that
// the assembled project block actually travels in the request's `system`
// field, which no in-process prompt test can show.

#[test]
fn the_project_instructions_block_reaches_the_wire_in_the_system_field() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("TEMUR.md"), "Never touch vendor/.").unwrap();
    let loaded = temur::project::load(
        dir.path(),
        &temur::tools::KeyGuard::empty(),
        temur::project::cap_chars(None),
        true,
    );
    let system = temur::prompt::assemble("BASE PROMPT", None, &loaded.block);

    let mut req = base_request();
    req.system = Some(system);
    let body = body_for(&req);

    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    // The Anthropic wire carries `system` as cache_control'd text blocks
    // (T28), not a bare string.
    let sent = v["system"][0]["text"].as_str().expect("system text block");
    assert!(sent.starts_with("BASE PROMPT"), "{sent}");
    assert!(
        sent.contains("<project_instructions source=\"TEMUR.md\">"),
        "opening delimiter on the wire: {sent}"
    );
    assert!(sent.contains("Never touch vendor/."), "file text: {sent}");
    assert!(sent.contains("</project_instructions>"), "closing: {sent}");
}

#[test]
fn two_turns_send_a_byte_identical_system_prefix() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("TEMUR.md"), "stable prefix").unwrap();
    let loaded = temur::project::load(
        dir.path(),
        &temur::tools::KeyGuard::empty(),
        temur::project::cap_chars(None),
        true,
    );
    let system = temur::prompt::assemble("BASE", None, &loaded.block);

    // T28 prefix stability: the block is captured once, so a second turn
    // carries the same bytes even though the file changed underneath.
    let mut req = base_request();
    req.system = Some(system.clone());
    let first = body_for(&req);
    std::fs::write(dir.path().join("TEMUR.md"), "CHANGED UNDERNEATH").unwrap();
    let second = body_for(&req);

    let a: serde_json::Value = serde_json::from_str(&first).unwrap();
    let b: serde_json::Value = serde_json::from_str(&second).unwrap();
    assert_eq!(a["system"], b["system"], "system prefix must not drift");
    assert!(!b["system"][0]["text"]
        .as_str()
        .unwrap()
        .contains("CHANGED UNDERNEATH"));
}

// ------------- T66 P1 amend: unsigned Thinking stays off the Anthropic wire

/// The assistant history an openai-compat reasoning turn leaves behind,
/// with the Thinking block's signature as given.
fn reasoned_history(signature: Option<&str>) -> Vec<RequestMessage> {
    vec![
        user_text("hello"),
        RequestMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: "local reasoning".into(),
                    signature: signature.map(Into::into),
                },
                ContentBlock::Text { text: "Hi.".into() },
            ],
        },
        user_text("again"),
    ]
}

/// T66 P1 amend (h): an unsigned Thinking block came from another wire, so
/// the Anthropic request carries the Text alone.
#[test]
fn unsigned_thinking_is_not_sent_to_anthropic() {
    let mut req = base_request();
    req.tools = vec![];
    req.messages = reasoned_history(None);
    let body = body_for(&req);
    assert!(!body.contains("local reasoning"), "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let content = v["messages"][1]["content"].as_array().unwrap();
    assert_eq!(content.len(), 1, "{body}");
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "Hi.");
}

/// T66 P1 amend (h), the control: a signed Thinking block is Anthropic's
/// own and still goes back verbatim.
#[test]
fn signed_thinking_is_still_sent_to_anthropic() {
    let mut req = base_request();
    req.tools = vec![];
    req.messages = reasoned_history(Some("EqQBCgIYAhIkSig="));
    let v: serde_json::Value = serde_json::from_str(&body_for(&req)).unwrap();
    let content = v["messages"][1]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "thinking");
    assert_eq!(content[0]["thinking"], "local reasoning");
    assert_eq!(content[0]["signature"], "EqQBCgIYAhIkSig=");
}

/// T66 P1 amend (j): an assistant reply that was only unsigned Thinking
/// leaves nothing to send, so it is omitted, and the user messages on
/// either side become one, blocks in order. No empty assistant message and
/// no two user messages in a row.
#[test]
fn a_thinking_only_reply_is_omitted_and_its_neighbours_merge() {
    let mut req = base_request();
    req.tools = vec![];
    req.messages = vec![
        RequestMessage {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "toolu_01".into(),
                    content: "result one".into(),
                    is_error: false,
                },
                ContentBlock::Text { text: "first question".into() },
            ],
        },
        RequestMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::Thinking {
                thinking: "thought, and nothing else".into(),
                signature: None,
            }],
        },
        user_text("second question"),
    ];
    let body = body_for(&req);
    assert!(!body.contains("thought, and nothing else"), "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let msgs = v["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1, "{body}");
    assert_eq!(msgs[0]["role"], "user");
    let blocks: Vec<(&str, &str)> = msgs[0]["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            let t = b["type"].as_str().unwrap();
            let s = b["text"].as_str().or(b["content"].as_str()).unwrap_or("");
            (t, s)
        })
        .collect();
    assert_eq!(
        blocks,
        vec![
            ("tool_result", "result one"),
            ("text", "first question"),
            ("text", "second question"),
        ]
    );
}

/// T66 P1 amend (j), the control: a reply that keeps its Text is sent, so
/// nothing is omitted and nothing merges.
#[test]
fn a_reply_with_text_is_neither_omitted_nor_merged() {
    let mut req = base_request();
    req.tools = vec![];
    req.messages = reasoned_history(None);
    let v: serde_json::Value = serde_json::from_str(&body_for(&req)).unwrap();
    let roles: Vec<&str> = v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, vec!["user", "assistant", "user"]);
}
