//! OpenAI-compatible Chat Completions **wire** types, plus the explicit
//! conversions between them and the neutral vocabulary in
//! [`crate::provider::types`]. These serialize/deserialize against the
//! `/chat/completions` JSON dialect spoken by OpenAI, Groq, OpenRouter,
//! Together, DeepSeek, and — the niche — llama.cpp/Ollama/vLLM/LM Studio.
//! They never leave this provider; the rest of temur speaks only the
//! neutral types.
//!
//! Tolerance policy mirrors the Anthropic provider's, tuned for local
//! servers whose compatibility is approximate: unknown fields are ignored,
//! absent usage stays `None` (never zero), absent tool-call IDs are
//! synthesized, `finish_reason` values we don't know map to `Unknown`,
//! assembled tool calls mean tool use whatever `finish_reason` says or
//! fails to say unless it says refusal (T13 F10), an error body
//! wrapped in a JSON array is unwrapped (T13 F9), and a `total_tokens`
//! larger than the sum of the named counts folds its gap into the output
//! count, which is where an unreported thinking spend belongs (T25 F11).

use crate::provider::types as neutral;
use crate::provider::StreamEvent;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Request side (serialized into the /chat/completions body).
// ---------------------------------------------------------------------------

/// One wire message. Unlike Anthropic's block model, roles are flat and a
/// neutral message can fan out into several wire messages (tool results
/// become individual `role:"tool"` messages).
#[derive(Debug, Clone, Serialize)]
pub struct RequestMessage {
    pub role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl RequestMessage {
    fn text(role: &'static str, content: String) -> Self {
        RequestMessage {
            role,
            content: Some(content),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }
}

/// A completed tool call on an assistant message. `arguments` is a JSON
/// *string* — the leak point the neutral types deliberately don't model.
#[derive(Debug, Clone, Serialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str, // always "function"
    pub function: FunctionBody,
    /// Provider round-trip state, echoed back exactly as received (T13
    /// F12). Gemini puts `{"google":{"thought_signature":"..."}}` here and
    /// 400s the next request without it. Skipped when absent, so every
    /// other server sees the byte-identical body it saw before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_content: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionBody {
    pub name: String,
    pub arguments: String,
}

/// Wire tool definition: the neutral schema nested under `function`.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub kind: &'static str, // always "function"
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl From<&crate::provider::ToolDef> for ToolDef {
    fn from(t: &crate::provider::ToolDef) -> Self {
        ToolDef {
            kind: "function",
            function: ToolFunction {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.input_schema.clone(),
            },
        }
    }
}

/// Neutral history → wire messages, at this boundary only. Not 1:1: a user
/// message carrying tool results fans out into `role:"tool"` messages (which
/// must directly follow the assistant message that made the calls) followed
/// by a `role:"user"` message for any text. Thinking and redacted-thinking
/// blocks are provider round-trip state we don't own — dropped, per the
/// neutral contract ("others ignore them"). `is_error` has no wire flag
/// here; the result text itself carries the error report.
pub fn convert_history(messages: &[neutral::RequestMessage]) -> Vec<RequestMessage> {
    let mut out = Vec::with_capacity(messages.len());
    for msg in messages {
        match msg.role {
            neutral::Role::Assistant => {
                let mut text = String::new();
                let mut tool_calls = vec![];
                for block in &msg.content {
                    match block {
                        neutral::ContentBlock::Text { text: t } => {
                            if !text.is_empty() {
                                text.push_str("\n\n");
                            }
                            text.push_str(t);
                        }
                        // input_raw is deliberately dropped: raw unparseable
                        // arguments never reach any wire. provider_state is
                        // the opposite case: it came FROM this wire and must
                        // go back to it verbatim (T13 F12).
                        neutral::ContentBlock::ToolUse {
                            id,
                            name,
                            input,
                            input_raw: _,
                            provider_state,
                        } => {
                            tool_calls.push(ToolCall {
                                id: id.clone(),
                                kind: "function",
                                function: FunctionBody {
                                    name: name.clone(),
                                    arguments: input.to_string(),
                                },
                                extra_content: provider_state.clone(),
                            });
                        }
                        _ => {} // thinking / redacted / tool_result-in-wrong-role / unknown
                    }
                }
                out.push(RequestMessage {
                    role: "assistant",
                    // Omit content entirely for pure tool-call messages; an
                    // assistant message with neither gets an empty string to
                    // stay wire-legal.
                    content: match (text.is_empty(), tool_calls.is_empty()) {
                        (false, _) => Some(text),
                        (true, false) => None,
                        (true, true) => Some(String::new()),
                    },
                    tool_calls,
                    tool_call_id: None,
                });
            }
            neutral::Role::User => {
                let mut text = String::new();
                for block in &msg.content {
                    match block {
                        neutral::ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error: _,
                        } => {
                            out.push(RequestMessage {
                                role: "tool",
                                content: Some(content.clone()),
                                tool_calls: vec![],
                                tool_call_id: Some(tool_use_id.clone()),
                            });
                        }
                        neutral::ContentBlock::Text { text: t } => {
                            if !text.is_empty() {
                                text.push_str("\n\n");
                            }
                            text.push_str(t);
                        }
                        _ => {}
                    }
                }
                if !text.is_empty() {
                    out.push(RequestMessage::text("user", text));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Response side (streamed chunks).
// ---------------------------------------------------------------------------

/// One `data:` chunk. Every field is defaulted: local servers omit freely,
/// and per the tolerance policy an unrecognized payload parses to an empty
/// chunk and is ignored rather than killing the stream.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Chunk {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub choices: Vec<Choice>,
    /// Sent only with `stream_options.include_usage`, and many local
    /// servers never send it at all. NOT final-chunk-only: Gemini repeats
    /// an identical usage object on every chunk, including a chunk whose
    /// `choices` array is non-empty (T25 F11, captured live 2026-08-10 at
    /// t13-live/evidence/t25-gemini.0.sse). Assembly is last-wins rather
    /// than additive, which is what makes the repetition harmless; summing
    /// would have doubled that turn's count.
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Choice {
    #[serde(default)]
    pub delta: Delta,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Delta {
    /// Sent in the first chunk by OpenAI; some local servers repeat it in
    /// every chunk. Tolerated and ignored either way.
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    /// Structured-output refusals (SDK-fixture-confirmed): the refusal text
    /// streams here instead of `content`, with `finish_reason:"stop"`.
    #[serde(default)]
    pub refusal: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallDelta>,
}

/// A tool-call fragment. OpenAI addresses fragments by `index` and sends
/// `id`/`name` only in the first one; quirky local servers omit `index`
/// (whole-call-in-one-chunk) or `id` (synthesized at assembly).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ToolCallDelta {
    #[serde(default)]
    pub index: Option<u32>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<FunctionDelta>,
    /// Opaque provider state riding along with the call (T13 F12). Gemini
    /// sends it in the same fragment as the name; nothing here reads it.
    #[serde(default)]
    pub extra_content: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FunctionDelta {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: Option<u64>,
    #[serde(default)]
    pub completion_tokens: Option<u64>,
    /// The server's own total (T25 F11). Parsed because on some wires it is
    /// the ONLY place the thinking spend is reported; see the conversion
    /// below for what the gap means and why folding it is safe.
    #[serde(default)]
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct PromptTokensDetails {
    #[serde(default)]
    pub cached_tokens: Option<u64>,
}

impl From<Usage> for neutral::Usage {
    fn from(u: Usage) -> Self {
        // T25 F11, captured live 2026-08-05 from gemini-3.6-flash and kept
        // at t13-live/evidence/f12-nostream.txt: the response ends
        // "usage":{"completion_tokens":19,"prompt_tokens":48,
        // "total_tokens":103}. 48 + 19 is 67, not 103. The missing 36 is
        // the thinking spend, which Gemini bills and counts in its total
        // but reports in NEITHER named field, so reading completion_tokens
        // alone understates a thinking turn by most of what it cost.
        //
        // Fold the gap into output_tokens, which is where the thinking
        // spend belongs and how it is priced. Safe on every other wire we
        // know of, because the fold is a no-op unless a server both reports
        // a total and reports one larger than its own parts:
        //   - OpenAI counts reasoning INSIDE completion_tokens, so its
        //     total is exactly the sum and the gap is zero.
        //   - llama.cpp and friends sum exactly, same zero gap.
        //   - a server that omits total_tokens is untouched.
        // Anything missing a field, or a total that fails to exceed the
        // sum, keeps the old value byte for byte. The T24 cost estimate
        // therefore tightens on Gemini rather than distorting anywhere:
        // pricing the gap at the output rate is what Google actually
        // charges for it.
        let output_tokens = match (u.prompt_tokens, u.completion_tokens, u.total_tokens) {
            (Some(prompt), Some(completion), Some(total)) => {
                let gap = total.saturating_sub(prompt).saturating_sub(completion);
                Some(completion.saturating_add(gap))
            }
            _ => u.completion_tokens,
        };
        neutral::Usage {
            input_tokens: u.prompt_tokens,
            output_tokens,
            // No such concept on this wire; stays "not reported".
            cache_creation_input_tokens: None,
            cache_read_input_tokens: u.prompt_tokens_details.and_then(|d| d.cached_tokens),
        }
    }
}

/// `finish_reason` → neutral. Documented per ROADMAP §2: no
/// OpenAI-compatible endpoint can emit `PauseTurn`, `StopSequence`, or
/// `ModelContextWindowExceeded`; `content_filter` is the closest thing to a
/// refusal and carries no details.
pub fn map_finish_reason(s: &str) -> neutral::StopReason {
    match s {
        "stop" => neutral::StopReason::EndTurn,
        "length" => neutral::StopReason::MaxTokens,
        "tool_calls" | "function_call" => neutral::StopReason::ToolUse,
        "content_filter" => neutral::StopReason::Refusal,
        _ => neutral::StopReason::Unknown,
    }
}

/// Error body: OpenAI's `{"error":{"message","type","code",...}}`. Some
/// servers put a bare string under `error` instead; [`WireError`] absorbs
/// both shapes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ApiErrorBody {
    #[serde(default)]
    pub message: String,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub code: Option<Value>,
    /// Google's compat surface names the error class here and leaves `type`
    /// absent, with a NUMERIC `code` (T13 F9, captured live 2026-08-05).
    #[serde(default)]
    pub status: Option<String>,
}

impl ApiErrorBody {
    /// Best label available: `type`, else a string `code`, else `status`,
    /// else generic. `status` is last so every shape that already produced
    /// a label keeps producing the same one.
    pub fn kind_label(&self) -> String {
        self.kind
            .clone()
            .or_else(|| {
                self.code
                    .as_ref()
                    .and_then(|c| c.as_str().map(String::from))
            })
            .or_else(|| self.status.clone())
            .unwrap_or_else(|| "api_error".into())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum WireError {
    Body(ApiErrorBody),
    Message(String),
}

impl WireError {
    pub fn into_body(self) -> ApiErrorBody {
        match self {
            WireError::Body(b) => b,
            WireError::Message(message) => ApiErrorBody {
                message,
                kind: None,
                code: None,
                status: None,
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErrorEnvelope {
    #[serde(default)]
    pub error: Option<WireError>,
}

/// The error body as it actually arrives (T13 F9). OpenAI and every local
/// server send the bare envelope; Google's compat surface wraps that same
/// envelope in a ONE-ELEMENT JSON ARRAY, which the object-only shape failed
/// to parse, so a live 404 on a retired model id printed
/// `api error (HTTP 404) api_error:` with no message at all. Captured live
/// 2026-08-05 during T13 acceptance.
///
/// Deliberately untagged and permissive: an ordinary chunk still parses as
/// `One` with no error, which is what the mid-stream check relies on.
///
/// VARIANT ORDER IS LOAD-BEARING. serde can build a struct from a sequence
/// positionally, so an `One`-first enum matches `[{"error":{...}}]` as an
/// ErrorEnvelope whose `error` field is the whole first element, silently
/// reproducing the empty-message bug this type exists to fix. Sequence
/// first, object second.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ErrorPayload {
    Many(Vec<ErrorEnvelope>),
    One(ErrorEnvelope),
}

impl ErrorPayload {
    /// The first error carried by the payload, whatever shape wrapped it.
    /// No observed server sends more than one element; scanning past a
    /// leading element that has no `error` is free tolerance.
    pub fn into_error(self) -> Option<WireError> {
        match self {
            ErrorPayload::One(e) => e.error,
            ErrorPayload::Many(v) => v.into_iter().find_map(|e| e.error),
        }
    }
}

// ---------------------------------------------------------------------------
// Stream assembly.
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct PendingCall {
    id: Option<String>,
    name: String,
    arguments: String,
    /// T13 F12: opaque state to echo back, kept exactly as it arrived.
    extra_content: Option<Value>,
}

/// Assembles a complete neutral [`neutral::ResponseMessage`] from a stream
/// of chunks, emitting UI events as fragments arrive. The OpenAI wire has
/// no `message_start`, so "did the stream say anything at all" is tracked
/// explicitly: an empty stream stays `None` (→ `Incomplete`).
#[derive(Debug, Default)]
pub struct ChunkAccumulator {
    started: bool,
    id: Option<String>,
    model: Option<String>,
    text: String,
    refusal: String,
    calls: Vec<PendingCall>,
    finish_reason: Option<String>,
    usage: Option<Usage>,
    /// Set if the stream carried an error envelope instead of a chunk.
    pub error: Option<ApiErrorBody>,
}

impl ChunkAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &Chunk, on_event: &mut dyn FnMut(StreamEvent)) {
        self.started = true;
        if self.id.is_none() {
            self.id = chunk.id.clone().filter(|s| !s.is_empty());
        }
        if self.model.is_none() {
            self.model = chunk.model.clone().filter(|s| !s.is_empty());
        }
        if let Some(u) = chunk.usage {
            self.usage = Some(u);
        }
        let Some(choice) = chunk.choices.first() else {
            return; // usage-only final chunk has an empty choices array
        };
        if let Some(text) = &choice.delta.content {
            if !text.is_empty() {
                self.text.push_str(text);
                on_event(StreamEvent::TextDelta(text.clone()));
            }
        }
        if let Some(refusal) = &choice.delta.refusal {
            // Not surfaced as streaming text: the agent's refusal path
            // discards refused output and shows a notice, exactly like the
            // Anthropic pre-output refusal.
            self.refusal.push_str(refusal);
        }
        for frag in &choice.delta.tool_calls {
            self.push_tool_fragment(frag, on_event);
        }
        if choice.finish_reason.is_some() {
            self.finish_reason = choice.finish_reason.clone();
        }
    }

    fn push_tool_fragment(
        &mut self,
        frag: &ToolCallDelta,
        on_event: &mut dyn FnMut(StreamEvent),
    ) {
        // Addressing: `index` when present (OpenAI proper); without it — the
        // local-server quirk — an id or name opens a new call and bare
        // argument fragments append to the last one.
        let idx = match frag.index {
            Some(i) => {
                let i = i as usize;
                while self.calls.len() <= i {
                    self.calls.push(PendingCall::default());
                }
                i
            }
            None => {
                let opens_new = frag.id.is_some()
                    || frag
                        .function
                        .as_ref()
                        .is_some_and(|f| f.name.as_deref().is_some_and(|n| !n.is_empty()));
                if opens_new || self.calls.is_empty() {
                    self.calls.push(PendingCall::default());
                }
                self.calls.len() - 1
            }
        };
        let call = &mut self.calls[idx];
        if call.id.is_none() {
            call.id = frag.id.clone().filter(|s| !s.is_empty());
        }
        // Same first-wins rule as the id: a later fragment must not blank
        // out state an earlier one carried (T13 F12).
        if call.extra_content.is_none() {
            call.extra_content = frag.extra_content.clone();
        }
        if let Some(f) = &frag.function {
            if let Some(name) = &f.name {
                if call.name.is_empty() && !name.is_empty() {
                    call.name = name.clone();
                    on_event(StreamEvent::ToolUseStarted { name: name.clone() });
                }
            }
            if let Some(args) = &f.arguments {
                call.arguments.push_str(args);
            }
        }
    }

    pub fn into_message(self, fallback_model: &str) -> Option<neutral::ResponseMessage> {
        if !self.started {
            return None;
        }
        let mut content = vec![];
        if !self.text.is_empty() {
            content.push(neutral::ContentBlock::Text { text: self.text });
        }
        let had_calls = !self.calls.is_empty();
        for (i, call) in self.calls.into_iter().enumerate() {
            // Quirk: servers that omit IDs get deterministic synthesized
            // ones; the request converter round-trips whatever is here, so
            // tool results match up either way.
            let id = call.id.unwrap_or_else(|| format!("call_{i}"));
            let (input, input_raw) = if call.arguments.trim().is_empty() {
                (serde_json::json!({}), None)
            } else {
                match serde_json::from_str::<Value>(&call.arguments) {
                    Ok(v) => (v, None),
                    Err(_) => {
                        log::warn!("tool call {i} arguments are not valid JSON");
                        // Preserve what the model actually emitted so the
                        // agent can repair or report it; input stays {}.
                        (serde_json::json!({}), Some(call.arguments))
                    }
                }
            };
            content.push(neutral::ContentBlock::ToolUse {
                id,
                name: call.name,
                input,
                input_raw,
                provider_state: call.extra_content,
            });
        }
        // T13 F10, generalizing the older "absent finish_reason" quirk to
        // "absent OR wrong": a stream that assembled tool calls means
        // "execute the tools", whatever finish_reason says or fails to say.
        // Gemini's compat surface reports "tool_calls" on the NON-streaming
        // response and "stop" on the STREAMING one for the identical request
        // with the identical calls attached (pinned by curl, live
        // 2026-08-05). Mapping that faithfully made the agent loop drop
        // real, well-formed tool calls in silence, since it dispatches only
        // on ToolUse and the prose-recovery fallback is guarded by "no
        // ToolUse block present". ToolUse therefore wins over every other
        // mapped reason and over absence, with ONE exception.
        //
        // Refusal is excluded (P3.5 amendment). Executing side-effectful
        // calls out of a FILTERED completion is the dangerous direction of
        // this rule, and a refusal is nothing like the silence F10 fixed:
        // the agent's Refusal arm prints a notice, discards the output,
        // never auto-retries, and breaks WITHOUT pushing the assistant
        // turn, so no dangling tool_use is saved either. It also keeps this
        // wire's two refusal signals consistent, since the `delta.refusal`
        // override further down already beats ToolUse.
        let mapped = self.finish_reason.as_deref().map(map_finish_reason);
        let truncated = mapped == Some(neutral::StopReason::MaxTokens);
        let stop_reason = match mapped {
            Some(neutral::StopReason::Refusal) => mapped,
            _ if had_calls => Some(neutral::StopReason::ToolUse),
            _ => mapped,
        };
        // "length" is the user's problem whether or not calls were
        // assembled, so the truncation rides in stop_details and survives
        // the override above; the agent reports it AND dispatches. A
        // trailing call the cut mangled is caught by the agent's lossless
        // repair guard (T4) and fed back as an error rather than executed.
        let stop_details = truncated.then(|| neutral::StopDetails {
            kind: "max_tokens".into(),
            category: None,
            explanation: None,
        });
        // Structured-output refusal: the wire says finish_reason "stop" but
        // streams the text into `refusal`. Map it to the neutral refusal
        // shape (reason + explanation), same as Anthropic's stop_details.
        let (stop_reason, stop_details) = if self.refusal.is_empty() {
            (stop_reason, stop_details) // content_filter carries no details
        } else {
            (
                Some(neutral::StopReason::Refusal),
                Some(neutral::StopDetails {
                    kind: "refusal".into(),
                    category: None,
                    explanation: Some(self.refusal),
                }),
            )
        };
        Some(neutral::ResponseMessage {
            id: self.id.unwrap_or_default(),
            model: self.model.unwrap_or_else(|| fallback_model.to_string()),
            role: neutral::Role::Assistant,
            content,
            stop_reason,
            stop_details,
            usage: self.usage.map(Into::into).unwrap_or_default(),
        })
    }
}
