//! OpenAI-compatible Chat Completions provider: one implementation covering
//! OpenAI, Groq, OpenRouter, Together, DeepSeek, Gemini's compat endpoint,
//! and — the reason it exists — local servers (llama.cpp, Ollama, vLLM,
//! LM Studio). Request building, the shared retry policy, and driving the
//! chunk stream into a completed neutral message.
//!
//! Keyless operation is first-class: a `None` API key sends no auth header
//! at all, which is exactly what local endpoints expect. Keyed use follows
//! the same by-path secret rule as every provider.

pub mod transport;
pub mod types;

use crate::cancel::CancelToken;
use crate::provider::sse::SseFrames;
use crate::provider::transport::{Transport, TransportError};
use crate::provider::{ChatRequest, Provider, ProviderError, ResponseMessage, StreamEvent};
use std::io::BufReader;
use types::ChunkAccumulator;

pub struct OpenAiCompatProvider {
    base_url: String,
    /// Empty string = keyless (no auth header). Never logged.
    api_key: String,
    transport: Box<dyn Transport>,
}

impl OpenAiCompatProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        transport: Box<dyn Transport>,
    ) -> Self {
        OpenAiCompatProvider {
            base_url: base_url.into(),
            api_key: api_key.unwrap_or_default(),
            transport,
        }
    }

    pub fn with_http(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        Self::new(base_url, api_key, Box::new(transport::HttpTransport::new()))
    }

    fn build_body(req: &ChatRequest) -> Result<String, ProviderError> {
        // Neutral history → wire messages, explicitly, at this boundary
        // only. The system prompt is a plain leading system message here
        // (no cache_control: prompt caching is Anthropic-specific wire
        // surface; compat servers cache — or don't — on their own).
        let mut messages = vec![];
        if let Some(system) = &req.system {
            messages.push(serde_json::to_value(types::RequestMessage {
                role: "system",
                content: Some(system.clone()),
                tool_calls: vec![],
                tool_call_id: None,
            })
            .map_err(|e| ProviderError::Stream(format!("serialize system: {e}")))?);
        }
        for m in types::convert_history(&req.messages) {
            messages.push(
                serde_json::to_value(&m)
                    .map_err(|e| ProviderError::Stream(format!("serialize message: {e}")))?,
            );
        }
        let mut body = serde_json::json!({
            "model": req.model,
            // The classic wire name, deliberately: OpenAI-proper deprecated
            // it for max_completion_tokens, but llama.cpp/Ollama/OpenRouter/
            // DeepSeek — the compat universe this provider targets — all
            // speak max_tokens; several never learned the new name.
            "max_tokens": req.max_tokens,
            "stream": true,
            // Opt in to final-chunk usage. Local servers that predate
            // stream_options ignore unknown fields; absent usage stays None.
            "stream_options": {"include_usage": true},
            "messages": messages,
        });
        if !req.tools.is_empty() {
            let tools: Vec<types::ToolDef> = req.tools.iter().map(Into::into).collect();
            body["tools"] = serde_json::to_value(&tools)
                .map_err(|e| ProviderError::Stream(format!("serialize tools: {e}")))?;
        }
        // req.thinking has no mapping on this wire (OpenAI's reasoning
        // controls are a different, model-gated surface); deliberately
        // ignored rather than guessed at.
        if let Some(t) = req.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        if let Some(p) = req.top_p {
            body["top_p"] = serde_json::json!(p);
        }
        // Sorted-key serialization: byte-identical to the pre-T15 wire (see
        // to_sorted_json_string).
        crate::provider::to_sorted_json_string(&body)
            .map_err(|e| ProviderError::Stream(format!("serialize request: {e}")))
    }

    fn drive(
        &self,
        req: &ChatRequest,
        reader: Box<dyn std::io::Read>,
        on_event: &mut dyn FnMut(StreamEvent),
        cancel: &CancelToken,
    ) -> Result<ResponseMessage, ProviderError> {
        let mut acc = ChunkAccumulator::new();
        for frame in SseFrames::new(BufReader::new(reader)) {
            let data = match frame {
                Ok(data) => data,
                // A read error while the user has already cancelled is the
                // cancellation, not a failure: keep the accumulated partial
                // (F5) instead of throwing away already-streamed content.
                Err(_) if cancel.is_set() => break,
                Err(e) => return Err(ProviderError::Stream(e.to_string())),
            };
            if data.trim() == "[DONE]" {
                break;
            }
            // An error envelope can replace a chunk mid-stream; record it
            // and keep reading, like the Anthropic path does.
            if let Ok(env) = serde_json::from_str::<types::ErrorEnvelope>(&data) {
                if let Some(err) = env.error {
                    acc.error = Some(err.into_body());
                    continue;
                }
            }
            let chunk: types::Chunk = match serde_json::from_str::<types::Chunk>(&data) {
                Ok(chunk) => chunk,
                // Same rule for a chunk cut mid-JSON by the cancel race.
                Err(_) if cancel.is_set() => break,
                Err(e) => {
                    let snippet: String = data.chars().take(120).collect();
                    return Err(ProviderError::Stream(format!("{e} (data: {snippet})")));
                }
            };
            acc.push(&chunk, on_event);
            // Cooperative cancel, checked once per received frame — AFTER the
            // frame is accumulated, so everything fully received is kept and
            // the outcome never depends on read buffering. A fully stalled
            // read blocks in the iterator and cannot observe the token
            // (documented residual; force-quit remains the escape hatch).
            if cancel.is_set() {
                break;
            }
        }
        if let Some(err) = acc.error {
            return Err(ProviderError::Api {
                status: 200, // stream was accepted; the error arrived mid-stream
                kind: err.kind_label(),
                message: err.message,
            });
        }
        acc.into_message(&req.model).ok_or(ProviderError::Incomplete)
    }
}

impl Provider for OpenAiCompatProvider {
    fn stream(
        &self,
        req: &ChatRequest,
        on_event: &mut dyn FnMut(StreamEvent),
        cancel: &CancelToken,
    ) -> Result<ResponseMessage, ProviderError> {
        // Cancelled before anything was sent: nothing to keep.
        if cancel.is_set() {
            return Err(ProviderError::Incomplete);
        }
        // base_url includes the version prefix by SDK convention
        // (https://api.openai.com/v1, http://127.0.0.1:8080/v1, …).
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let body = Self::build_body(req)?;
        match crate::provider::transport::post_stream_with_retries(
            self.transport.as_ref(),
            &url,
            &self.api_key,
            &body,
            cancel,
        ) {
            Ok(reader) => self.drive(req, reader, on_event, cancel),
            Err(e) => Err(transport_error_to_provider(e)),
        }
    }
}

fn transport_error_to_provider(e: TransportError) -> ProviderError {
    match e {
        TransportError::Status { code, body, .. } => {
            let parsed = serde_json::from_str::<types::ErrorEnvelope>(&body)
                .ok()
                .and_then(|env| env.error)
                .map(types::WireError::into_body);
            match parsed {
                Some(err) => ProviderError::Api {
                    status: code,
                    kind: err.kind_label(),
                    message: err.message,
                },
                None => ProviderError::Api {
                    status: code,
                    kind: "http_error".into(),
                    message: format!("HTTP {code}"),
                },
            }
        }
        TransportError::Io(msg) => ProviderError::Network(msg),
    }
}
