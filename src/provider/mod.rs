//! Provider layer. `Provider` is the seam a second provider (e.g. Gemini)
//! implements later; the agent core and UI speak only these types. The
//! Anthropic wire format stays inside `anthropic`.

pub mod anthropic;

use serde::Serialize;
use serde_json::Value;

// The core conversation vocabulary. These are owned by the provider layer;
// today they serialize 1:1 to Anthropic's format, and a future non-Anthropic
// provider converts them at its own boundary.
pub use anthropic::types::{ContentBlock, ResponseMessage, Role, StopReason, Usage};

/// One turn in the conversation history (request side).
#[derive(Debug, Clone, Serialize)]
pub struct RequestMessage {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

/// A tool made available to the model.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub max_tokens: u32,
    pub system: Option<String>,
    /// Adaptive thinking (off by default in v1).
    pub thinking: bool,
    pub messages: Vec<RequestMessage>,
    pub tools: Vec<ToolDef>,
}

/// Incremental events surfaced to the UI while a response streams.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolUseStarted { name: String },
}

#[derive(thiserror::Error, Debug)]
pub enum ProviderError {
    /// The API answered with an error (HTTP error body, or a mid-stream
    /// `error` event — then `status` is the HTTP status the stream ran on).
    #[error("api error (HTTP {status}) {kind}: {message}")]
    Api {
        status: u16,
        kind: String,
        message: String,
    },
    #[error("network: {0}")]
    Network(String),
    #[error("stream: {0}")]
    Stream(String),
    #[error("stream ended without a complete message")]
    Incomplete,
}

pub trait Provider {
    /// Send one request; invoke `on_event` for each incremental UI event;
    /// return the fully assembled assistant message.
    fn stream(
        &self,
        req: &ChatRequest,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> Result<ResponseMessage, ProviderError>;
}
