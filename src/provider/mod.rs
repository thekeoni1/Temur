//! Provider layer. `Provider` is the seam a second provider (e.g. an
//! OpenAI-compatible endpoint) implements later; the agent core and UI speak
//! only the neutral types in [`types`]. Each provider owns its wire format
//! and converts at its own boundary — the Anthropic wire shapes live in
//! `anthropic::types`, never here.

pub mod anthropic;
pub mod openai_compat;
pub mod sse;
pub mod transport;
pub mod types;

use serde_json::Value;

pub use crate::cancel::CancelToken;
pub use types::{
    ContentBlock, RequestMessage, ResponseMessage, Role, StopDetails, StopReason, Usage,
};

/// A tool made available to the model. Providers serialize this into their
/// own tool-definition wire shape.
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    /// Response token cap. Neutral name — providers map it to their own
    /// field. (Both current providers happen to call it `max_tokens` on the
    /// wire: OpenAI-proper deprecated that name for `max_completion_tokens`,
    /// but the compat universe this provider targets — llama.cpp, Ollama,
    /// OpenRouter, DeepSeek, … — still speaks the classic name universally.)
    pub max_tokens: u32,
    pub system: Option<String>,
    /// Adaptive thinking (off by default in v1).
    pub thinking: bool,
    /// Sampling temperature. `None` = provider default: the field is simply
    /// absent from the request, exactly as before it existed here.
    pub temperature: Option<f64>,
    /// Nucleus sampling. `None` = provider default (field absent).
    pub top_p: Option<f64>,
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
    ///
    /// `cancel` is polled cooperatively — before the POST, at each retry
    /// backoff slice, and at each received stream frame. On cancellation the
    /// provider stops reading and returns `Ok` with whatever partial message
    /// has accumulated (the agent applies its landing policy), or
    /// `Err(Incomplete)` if nothing had started.
    fn stream(
        &self,
        req: &ChatRequest,
        on_event: &mut dyn FnMut(StreamEvent),
        cancel: &CancelToken,
    ) -> Result<ResponseMessage, ProviderError>;
}
