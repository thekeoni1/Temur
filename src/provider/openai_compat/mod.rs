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
use crate::provider::{
    ChatRequest, MaxTokensParam, Provider, ProviderError, ResponseMessage, StreamEvent,
};
use std::io::BufReader;
use types::ChunkAccumulator;

pub struct OpenAiCompatProvider {
    base_url: String,
    /// Empty string = keyless (no auth header). Never logged.
    api_key: String,
    /// Which wire key carries the token cap (T25 F7); validated upstream.
    max_tokens_parameter: MaxTokensParam,
    transport: Box<dyn Transport>,
}

impl OpenAiCompatProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        max_tokens_parameter: MaxTokensParam,
        transport: Box<dyn Transport>,
    ) -> Self {
        OpenAiCompatProvider {
            base_url: base_url.into(),
            api_key: api_key.unwrap_or_default(),
            max_tokens_parameter,
            transport,
        }
    }

    pub fn with_http(
        base_url: impl Into<String>,
        api_key: Option<String>,
        max_tokens_parameter: MaxTokensParam,
    ) -> Self {
        Self::new(
            base_url,
            api_key,
            max_tokens_parameter,
            Box::new(transport::HttpTransport::new()),
        )
    }

    fn build_body(&self, req: &ChatRequest) -> Result<String, ProviderError> {
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
            "stream": true,
            // Opt in to final-chunk usage. Local servers that predate
            // stream_options ignore unknown fields; absent usage stays None.
            "stream_options": {"include_usage": true},
            "messages": messages,
        });
        // T25 F7: the token cap under whichever of the two names this
        // profile configured. The classic max_tokens stays the default and
        // every existing config keeps sending a byte-identical body;
        // max_completion_tokens exists because OpenAI-proper's gpt-5 era
        // ids reject the classic name outright, while llama.cpp, Ollama,
        // OpenRouter and DeepSeek only ever learned it. The value is the
        // same u32 either way, and exactly one of the two keys is ever
        // present, so nothing downstream has to reconcile a pair.
        body[self.max_tokens_parameter.wire_key()] = serde_json::json!(req.max_tokens);
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
            if let Ok(env) = serde_json::from_str::<types::ErrorPayload>(&data) {
                if let Some(err) = env.into_error() {
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
        let body = self.build_body(req)?;
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
            // ErrorPayload, not ErrorEnvelope: Google answers with the
            // envelope wrapped in a one-element array (T13 F9).
            let parsed = serde_json::from_str::<types::ErrorPayload>(&body)
                .ok()
                .and_then(types::ErrorPayload::into_error)
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
        // T50: the ordinary turn-error path, same as any other network
        // failure. Control returns, the session stays intact, and no
        // string pinned by T21/T43 changes.
        TransportError::Timeout { phase, .. } => {
            ProviderError::Network(format!("timed out waiting for {phase} from the server"))
        }
    }
}
