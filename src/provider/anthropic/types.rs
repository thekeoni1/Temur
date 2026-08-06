//! Anthropic Messages API **wire** types, plus the explicit conversions
//! between them and the neutral vocabulary in [`crate::provider::types`].
//! These serialize/deserialize 1:1 against Anthropic's JSON and never leave
//! this provider; the rest of temur speaks only the neutral types.
//!
//! Tolerance policy (per Anthropic's versioning guidance): unknown event
//! types, block types, delta types, stop reasons, and JSON fields must never
//! be fatal. Enums carry an `Unknown` catch-all; structs ignore extra fields.

use crate::provider::types as neutral;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

/// Content blocks, used both in requests (serialized) and responses (parsed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// Request-only (sent back by us after executing tools).
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    PauseTurn,
    Refusal,
    ModelContextWindowExceeded,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

/// Request-side wire message (serialized into the Messages API body).
#[derive(Debug, Clone, Serialize)]
pub struct RequestMessage {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

/// Wire tool definition (Anthropic's shape happens to match the neutral one
/// field-for-field today; the conversion stays explicit anyway).
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Populated only on refusals (SDK-fixture-confirmed shape:
/// `{"type":"refusal","category":"cyber","explanation":"..."}`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StopDetails {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub explanation: Option<String>,
}

/// The assistant message as it appears in `message_start` / non-streaming
/// responses. Unknown extra fields are ignored.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ResponseMessage {
    pub id: String,
    pub model: String,
    pub role: Role,
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub stop_reason: Option<StopReason>,
    #[serde(default)]
    pub stop_details: Option<StopDetails>,
    #[serde(default)]
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
    ThinkingDelta { thinking: String },
    SignatureDelta { signature: String },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct MessageDeltaBody {
    #[serde(default)]
    pub stop_reason: Option<StopReason>,
    #[serde(default)]
    pub stop_sequence: Option<String>,
    #[serde(default)]
    pub stop_details: Option<StopDetails>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ApiErrorBody {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub message: String,
}

/// One parsed SSE event. Unknown event types land in `Unknown`, never an error.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SseEvent {
    MessageStart {
        message: ResponseMessage,
    },
    ContentBlockStart {
        index: usize,
        content_block: ContentBlock,
    },
    ContentBlockDelta {
        index: usize,
        delta: Delta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        delta: MessageDeltaBody,
        #[serde(default)]
        usage: Option<Usage>,
    },
    MessageStop,
    Ping,
    Error {
        error: ApiErrorBody,
    },
    #[serde(other)]
    Unknown,
}

/// Assembles a complete `ResponseMessage` from a stream of events.
#[derive(Debug, Default)]
pub struct MessageAccumulator {
    message: Option<ResponseMessage>,
    /// Accumulated `input_json_delta` fragments per block index; parsed into
    /// the tool_use `input` at `content_block_stop`.
    pending_json: HashMap<usize, String>,
    /// Raw argument strings that failed to parse, by block index — attached
    /// as `input_raw` at the wire→neutral conversion, never on any wire.
    failed_json: HashMap<usize, String>,
    /// Set if the stream carried a mid-stream `error` event.
    pub error: Option<ApiErrorBody>,
}

impl MessageAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, ev: &SseEvent) {
        match ev {
            SseEvent::MessageStart { message } => self.message = Some(message.clone()),
            SseEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                if let Some(msg) = self.message.as_mut() {
                    // Indices arrive in order; pad defensively if they don't.
                    while msg.content.len() < *index {
                        msg.content.push(ContentBlock::Unknown);
                    }
                    if msg.content.len() == *index {
                        msg.content.push(content_block.clone());
                    }
                }
            }
            SseEvent::ContentBlockDelta { index, delta } => {
                let block = self
                    .message
                    .as_mut()
                    .and_then(|m| m.content.get_mut(*index));
                match (delta, block) {
                    (Delta::TextDelta { text }, Some(ContentBlock::Text { text: t })) => {
                        t.push_str(text)
                    }
                    (
                        Delta::ThinkingDelta { thinking },
                        Some(ContentBlock::Thinking { thinking: t, .. }),
                    ) => t.push_str(thinking),
                    (
                        Delta::SignatureDelta { signature },
                        Some(ContentBlock::Thinking { signature: s, .. }),
                    ) => *s = Some(signature.clone()),
                    (Delta::InputJsonDelta { partial_json }, _) => self
                        .pending_json
                        .entry(*index)
                        .or_default()
                        .push_str(partial_json),
                    // Unknown delta, unknown block, or mismatched pair: skip.
                    _ => {}
                }
            }
            SseEvent::ContentBlockStop { index } => {
                if let Some(json) = self.pending_json.remove(index) {
                    if let Some(ContentBlock::ToolUse { input, .. }) = self
                        .message
                        .as_mut()
                        .and_then(|m| m.content.get_mut(*index))
                    {
                        if !json.trim().is_empty() {
                            if let Ok(v) = serde_json::from_str::<Value>(&json) {
                                *input = v;
                            } else {
                                log::warn!("tool input at block {index} is not valid JSON");
                                self.failed_json.insert(*index, json);
                            }
                        }
                    }
                }
            }
            SseEvent::MessageDelta { delta, usage } => {
                if let Some(msg) = self.message.as_mut() {
                    if delta.stop_reason.is_some() {
                        msg.stop_reason = delta.stop_reason;
                    }
                    if delta.stop_details.is_some() {
                        msg.stop_details = delta.stop_details.clone();
                    }
                    if let Some(u) = usage {
                        // message_delta usage is cumulative for the response.
                        if u.output_tokens > 0 {
                            msg.usage.output_tokens = u.output_tokens;
                        }
                        if u.input_tokens > 0 {
                            msg.usage.input_tokens = u.input_tokens;
                        }
                        if u.cache_creation_input_tokens > 0 {
                            msg.usage.cache_creation_input_tokens = u.cache_creation_input_tokens;
                        }
                        if u.cache_read_input_tokens > 0 {
                            msg.usage.cache_read_input_tokens = u.cache_read_input_tokens;
                        }
                    }
                }
            }
            SseEvent::Error { error } => self.error = Some(error.clone()),
            SseEvent::MessageStop | SseEvent::Ping | SseEvent::Unknown => {}
        }
    }

    pub fn message(&self) -> Option<&ResponseMessage> {
        self.message.as_ref()
    }

    pub fn into_message(self) -> Option<ResponseMessage> {
        self.message
    }

    /// Leftover `pending_json` means the stream ended without a
    /// `content_block_stop` for that block (the max_tokens-mid-JSON case):
    /// record anything unparseable so it surfaces as `input_raw` too.
    fn drain_pending_json(&mut self) {
        let pending = std::mem::take(&mut self.pending_json);
        for (index, json) in pending {
            let is_tool_use = matches!(
                self.message.as_ref().and_then(|m| m.content.get(index)),
                Some(ContentBlock::ToolUse { .. })
            );
            if is_tool_use
                && !json.trim().is_empty()
                && serde_json::from_str::<Value>(&json).is_err()
            {
                self.failed_json.insert(index, json);
            }
        }
    }

    /// Wire → neutral, the streaming path's boundary crossing: converts the
    /// assembled message and attaches the raw unparseable argument strings
    /// as `input_raw` on their tool_use blocks.
    pub fn into_neutral_message(mut self) -> Option<neutral::ResponseMessage> {
        self.drain_pending_json();
        let failed = std::mem::take(&mut self.failed_json);
        self.message.map(|m| {
            let mut msg: neutral::ResponseMessage = m.into();
            for (index, raw) in failed {
                if let Some(neutral::ContentBlock::ToolUse { input_raw, .. }) =
                    msg.content.get_mut(index)
                {
                    *input_raw = Some(raw);
                }
            }
            msg
        })
    }
}

// ---------------------------------------------------------------------------
// Boundary conversions: neutral → wire (requests), wire → neutral (responses).
// This is THE seam — nothing outside `provider::anthropic` may touch the wire
// types, and nothing here may serialize a neutral type onto the network.
// ---------------------------------------------------------------------------

impl From<neutral::Role> for Role {
    fn from(r: neutral::Role) -> Self {
        match r {
            neutral::Role::User => Role::User,
            neutral::Role::Assistant => Role::Assistant,
        }
    }
}

impl From<Role> for neutral::Role {
    fn from(r: Role) -> Self {
        match r {
            Role::User => neutral::Role::User,
            Role::Assistant => neutral::Role::Assistant,
        }
    }
}

impl From<&neutral::ContentBlock> for ContentBlock {
    fn from(b: &neutral::ContentBlock) -> Self {
        match b {
            neutral::ContentBlock::Text { text } => ContentBlock::Text { text: text.clone() },
            neutral::ContentBlock::Thinking {
                thinking,
                signature,
            } => ContentBlock::Thinking {
                thinking: thinking.clone(),
                signature: signature.clone(),
            },
            neutral::ContentBlock::RedactedThinking { data } => {
                ContentBlock::RedactedThinking { data: data.clone() }
            }
            // input_raw is deliberately dropped: raw unparseable arguments
            // never reach any wire. provider_state is dropped for the same
            // reason openai-compat drops thinking signatures: it is another
            // wire's round-trip state and means nothing here (T13 F12).
            neutral::ContentBlock::ToolUse {
                id,
                name,
                input,
                input_raw: _,
                provider_state: _,
            } => ContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            },
            neutral::ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => ContentBlock::ToolResult {
                tool_use_id: tool_use_id.clone(),
                content: content.clone(),
                is_error: *is_error,
            },
            // The agent filters Unknown before it can reach a request; if one
            // slips through it still must not panic mid-turn.
            neutral::ContentBlock::Unknown => ContentBlock::Unknown,
        }
    }
}

impl From<ContentBlock> for neutral::ContentBlock {
    fn from(b: ContentBlock) -> Self {
        match b {
            ContentBlock::Text { text } => neutral::ContentBlock::Text { text },
            ContentBlock::Thinking {
                thinking,
                signature,
            } => neutral::ContentBlock::Thinking {
                thinking,
                signature,
            },
            ContentBlock::RedactedThinking { data } => {
                neutral::ContentBlock::RedactedThinking { data }
            }
            ContentBlock::ToolUse { id, name, input } => neutral::ContentBlock::ToolUse {
                id,
                name,
                input,
                // Attached (when applicable) by the accumulator's
                // into_neutral_message, which owns the failed-parse map.
                input_raw: None,
                // This wire has no such concept: Anthropic verifies thinking
                // blocks, not tool calls (T13 F12).
                provider_state: None,
            },
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => neutral::ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            },
            ContentBlock::Unknown => neutral::ContentBlock::Unknown,
        }
    }
}

impl From<&neutral::RequestMessage> for RequestMessage {
    fn from(m: &neutral::RequestMessage) -> Self {
        RequestMessage {
            role: m.role.into(),
            content: m.content.iter().map(Into::into).collect(),
        }
    }
}

impl From<&crate::provider::ToolDef> for ToolDef {
    fn from(t: &crate::provider::ToolDef) -> Self {
        ToolDef {
            name: t.name.clone(),
            description: t.description.clone(),
            input_schema: t.input_schema.clone(),
        }
    }
}

impl From<StopReason> for neutral::StopReason {
    fn from(s: StopReason) -> Self {
        match s {
            StopReason::EndTurn => neutral::StopReason::EndTurn,
            StopReason::ToolUse => neutral::StopReason::ToolUse,
            StopReason::MaxTokens => neutral::StopReason::MaxTokens,
            StopReason::StopSequence => neutral::StopReason::StopSequence,
            StopReason::PauseTurn => neutral::StopReason::PauseTurn,
            StopReason::Refusal => neutral::StopReason::Refusal,
            StopReason::ModelContextWindowExceeded => {
                neutral::StopReason::ModelContextWindowExceeded
            }
            StopReason::Unknown => neutral::StopReason::Unknown,
        }
    }
}

impl From<StopDetails> for neutral::StopDetails {
    fn from(d: StopDetails) -> Self {
        neutral::StopDetails {
            kind: d.kind,
            category: d.category,
            explanation: d.explanation,
        }
    }
}

impl From<Usage> for neutral::Usage {
    fn from(u: Usage) -> Self {
        // Anthropic always reports these counters; wire fields absent from a
        // response default to 0 during parsing, exactly as before T1. The
        // best-effort `None` states exist for providers that genuinely don't
        // report usage.
        neutral::Usage {
            input_tokens: Some(u.input_tokens),
            output_tokens: Some(u.output_tokens),
            cache_creation_input_tokens: Some(u.cache_creation_input_tokens),
            cache_read_input_tokens: Some(u.cache_read_input_tokens),
        }
    }
}

impl From<ResponseMessage> for neutral::ResponseMessage {
    fn from(m: ResponseMessage) -> Self {
        neutral::ResponseMessage {
            id: m.id,
            model: m.model,
            role: m.role.into(),
            content: m.content.into_iter().map(Into::into).collect(),
            stop_reason: m.stop_reason.map(Into::into),
            stop_details: m.stop_details.map(Into::into),
            usage: m.usage.into(),
        }
    }
}
