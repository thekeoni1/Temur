//! Anthropic Messages API provider: request building, retry/backoff, and
//! driving the SSE stream into a completed message.

pub mod sse;
pub mod transport;
pub mod types;

use crate::cancel::CancelToken;
use crate::provider::{
    ChatRequest, Provider, ProviderError, ResponseMessage, StreamEvent,
};
use sse::SseReader;
use std::io::BufReader;
use transport::{Transport, TransportError};
use types::{ContentBlock, Delta, MessageAccumulator, SseEvent};

pub struct AnthropicProvider {
    base_url: String,
    api_key: String,
    transport: Box<dyn Transport>,
}

impl AnthropicProvider {
    pub fn new(base_url: impl Into<String>, api_key: String, transport: Box<dyn Transport>) -> Self {
        AnthropicProvider {
            base_url: base_url.into(),
            api_key,
            transport,
        }
    }

    pub fn with_http(base_url: impl Into<String>, api_key: String) -> Self {
        Self::new(base_url, api_key, Box::new(transport::HttpTransport::new()))
    }

    fn build_body(req: &ChatRequest) -> Result<String, ProviderError> {
        // Neutral history → Anthropic wire shapes, explicitly, at this
        // boundary only. Today the JSON they produce is identical to what the
        // shared types produced pre-T1 (the request_golden suite pins that);
        // the conversion is the point, not the output.
        let messages: Vec<types::RequestMessage> = req.messages.iter().map(Into::into).collect();
        let mut body = serde_json::json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "stream": true,
            "messages": messages,
        });
        // Moving cache breakpoint: mark the last cacheable content block of
        // the last message, so each request reads the entire prior
        // conversation from cache (~0.1x input rate) and re-bills only the
        // new tail. Injected at serialization time on this request's JSON
        // only — history itself is never mutated — so exactly one
        // message-level breakpoint exists per request and it advances as the
        // conversation grows. Together with the static system breakpoint
        // below that is 2 of the 4 allowed breakpoints.
        // The cache's lookback is 20 content blocks per breakpoint; a normal
        // iteration adds ~2-6 blocks, but a single response with >8 parallel
        // tool calls could approach that window.
        Self::mark_last_cacheable_block(&mut body["messages"]);
        if let Some(system) = &req.system {
            // Static cache breakpoint on the last (only) system block caches
            // tools + system together; keep tool order deterministic.
            body["system"] = serde_json::json!([{
                "type": "text",
                "text": system,
                "cache_control": {"type": "ephemeral"},
            }]);
        }
        if !req.tools.is_empty() {
            let tools: Vec<types::ToolDef> = req.tools.iter().map(Into::into).collect();
            body["tools"] = serde_json::to_value(&tools)
                .map_err(|e| ProviderError::Stream(format!("serialize tools: {e}")))?;
        }
        if req.thinking {
            body["thinking"] = serde_json::json!({"type": "adaptive"});
        }
        // Neutral sampling knobs map 1:1 onto Anthropic's names; unset means
        // the key is absent and behavior is exactly pre-T1.
        if let Some(t) = req.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        if let Some(p) = req.top_p {
            body["top_p"] = serde_json::json!(p);
        }
        serde_json::to_string(&body)
            .map_err(|e| ProviderError::Stream(format!("serialize request: {e}")))
    }

    /// Set `cache_control: ephemeral` on the last content block that may
    /// legally carry it (text / tool_use / tool_result), scanning messages
    /// back-to-front. Thinking and redacted_thinking blocks cannot carry
    /// cache_control, so a thinking-final assistant message falls back to
    /// its previous block (or an earlier message).
    fn mark_last_cacheable_block(messages: &mut serde_json::Value) {
        let Some(msgs) = messages.as_array_mut() else {
            return;
        };
        for msg in msgs.iter_mut().rev() {
            let Some(blocks) = msg.get_mut("content").and_then(|c| c.as_array_mut()) else {
                continue;
            };
            for block in blocks.iter_mut().rev() {
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("text") | Some("tool_use") | Some("tool_result") => {
                        block["cache_control"] = serde_json::json!({"type": "ephemeral"});
                        return;
                    }
                    _ => {}
                }
            }
        }
    }

    fn drive(
        &self,
        reader: Box<dyn std::io::Read>,
        on_event: &mut dyn FnMut(StreamEvent),
        cancel: &CancelToken,
    ) -> Result<ResponseMessage, ProviderError> {
        let mut acc = MessageAccumulator::new();
        for item in SseReader::new(BufReader::new(reader)) {
            let ev = match item {
                Ok(ev) => ev,
                // A read/parse error while the user has already cancelled is
                // the cancellation, not a failure: stop reading and keep the
                // accumulated partial (F5) — an Err here would throw away
                // already-streamed content the landing policy wants.
                Err(_) if cancel.is_set() => break,
                Err(e) => return Err(ProviderError::Stream(e.to_string())),
            };
            match &ev {
                SseEvent::ContentBlockStart { content_block, .. } => {
                    if let ContentBlock::ToolUse { name, .. } = content_block {
                        on_event(StreamEvent::ToolUseStarted { name: name.clone() });
                    }
                }
                SseEvent::ContentBlockDelta { delta, .. } => match delta {
                    Delta::TextDelta { text } => on_event(StreamEvent::TextDelta(text.clone())),
                    Delta::ThinkingDelta { thinking } => {
                        on_event(StreamEvent::ThinkingDelta(thinking.clone()))
                    }
                    _ => {}
                },
                SseEvent::Unknown => log::debug!("ignoring unknown SSE event"),
                _ => {}
            }
            acc.push(&ev);
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
                kind: err.kind,
                message: err.message,
            });
        }
        // Wire → neutral at the boundary: the accumulator's message is the
        // last Anthropic-shaped value on this code path. This conversion
        // also attaches input_raw for tool arguments that failed to parse.
        acc.into_neutral_message().ok_or(ProviderError::Incomplete)
    }
}

impl Provider for AnthropicProvider {
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
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let body = Self::build_body(req)?;
        // Shared retry policy (crate::provider::transport); only the error
        // envelope parsing below is Anthropic-shaped.
        match crate::provider::transport::post_stream_with_retries(
            self.transport.as_ref(),
            &url,
            &self.api_key,
            &body,
            cancel,
        ) {
            Ok(reader) => self.drive(reader, on_event, cancel),
            Err(e) => Err(transport_error_to_provider(e)),
        }
    }
}

fn transport_error_to_provider(e: TransportError) -> ProviderError {
    match e {
        TransportError::Status { code, body, .. } => {
            // Anthropic error envelope: {"type":"error","error":{"type":..,"message":..}}
            #[derive(serde::Deserialize)]
            struct Envelope {
                error: Option<types::ApiErrorBody>,
            }
            let parsed: Option<types::ApiErrorBody> = serde_json::from_str::<Envelope>(&body)
                .ok()
                .and_then(|env| env.error);
            match parsed {
                Some(err) => ProviderError::Api {
                    status: code,
                    kind: err.kind,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_breakpoint_skips_thinking_blocks() {
        // Assistant message ending in thinking: the marker must land on the
        // preceding text block, never on thinking/redacted_thinking.
        let mut messages = serde_json::json!([
            {"role": "user", "content": [{"type": "text", "text": "hi"}]},
            {"role": "assistant", "content": [
                {"type": "text", "text": "part one"},
                {"type": "thinking", "thinking": "…", "signature": "sig"},
            ]},
        ]);
        AnthropicProvider::mark_last_cacheable_block(&mut messages);
        assert!(messages[1]["content"][1].get("cache_control").is_none());
        assert_eq!(
            messages[1]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        // Exactly one marker in the whole array.
        let count = messages
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|m| m["content"].as_array().unwrap())
            .filter(|b| b.get("cache_control").is_some())
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn moving_breakpoint_handles_empty_history() {
        let mut messages = serde_json::json!([]);
        AnthropicProvider::mark_last_cacheable_block(&mut messages); // no panic
        assert_eq!(messages.as_array().unwrap().len(), 0);
    }
}
