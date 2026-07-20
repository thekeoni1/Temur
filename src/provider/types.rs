//! Provider-neutral conversation vocabulary, owned by this layer.
//!
//! Every provider converts these to/from its own wire shapes at its own
//! boundary (`anthropic::types` holds the Anthropic side). The serde derives
//! here exist for temur's OWN use only — e.g. future session persistence
//! (T5) — they are NOT a wire format, and no provider may put them on the
//! network directly.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

/// Content blocks, used both in request history and assembled responses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// Model reasoning. `signature` is opaque round-trip state for providers
    /// that verify their own thinking blocks (Anthropic); others ignore it.
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// Opaque provider round-trip state; carried back verbatim, never shown.
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
        /// The raw argument string as the provider received it, populated
        /// ONLY when it failed to parse as JSON (`input` stays `{}` then).
        /// Dropped at every neutral→wire request conversion — this never
        /// reaches any network, it exists so the agent can see WHAT the
        /// model actually emitted instead of a generic missing-field error.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_raw: Option<String>,
    },
    /// Request-only (sent back by us after executing tools).
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
    /// Content the provider didn't recognize. Never echoed back to any API.
    #[serde(other)]
    Unknown,
}

/// Neutral superset of stop reasons. Every provider maps its own vocabulary
/// into this enum at its boundary; no provider emits all variants.
/// Anthropic can emit any of them; OpenAI-compatible endpoints map
/// `stop`→`EndTurn`, `length`→`MaxTokens`, `tool_calls`→`ToolUse`,
/// `content_filter`→`Refusal` (T2).
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

/// Extra stop context, populated only on refusals today.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StopDetails {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub explanation: Option<String>,
}

/// Best-effort token accounting. Providers differ in what they report and
/// local servers may report nothing, so `None` means "not reported" — never
/// zero. The Anthropic provider populates every field, exactly as it always
/// did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
}

impl Usage {
    /// Accumulate another report. Absent fields stay absent instead of
    /// materializing as zero, so "no provider ever reported this" remains
    /// distinguishable from a genuine 0.
    pub fn add(&mut self, other: &Usage) {
        fn acc(a: &mut Option<u64>, b: Option<u64>) {
            if let Some(v) = b {
                *a = Some(a.unwrap_or(0) + v);
            }
        }
        acc(&mut self.input_tokens, other.input_tokens);
        acc(&mut self.output_tokens, other.output_tokens);
        acc(
            &mut self.cache_creation_input_tokens,
            other.cache_creation_input_tokens,
        );
        acc(
            &mut self.cache_read_input_tokens,
            other.cache_read_input_tokens,
        );
    }
}

/// One turn in the conversation history (request side).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestMessage {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

/// The completed assistant message a provider returns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
