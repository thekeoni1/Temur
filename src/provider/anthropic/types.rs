//! Anthropic Messages API wire types.
//!
//! Tolerance policy (per Anthropic's versioning guidance): unknown event
//! types, block types, delta types, stop reasons, and JSON fields must never
//! be fatal. Enums carry an `Unknown` catch-all; structs ignore extra fields.

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
}
