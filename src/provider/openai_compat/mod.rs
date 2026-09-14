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
use std::cell::Cell;
use std::io::BufReader;
use types::ChunkAccumulator;

pub struct OpenAiCompatProvider {
    base_url: String,
    /// Empty string = keyless (no auth header). Never logged.
    api_key: String,
    /// Which wire key carries the token cap (T25 F7); validated upstream.
    /// D19: this is where the NEXT request starts, not a constant. A server
    /// that rejects the configured name teaches this instance the other one
    /// for the rest of its life, so the wasted round trip is paid once per
    /// selection rather than once per turn. A `/model` or profile switch
    /// builds a fresh instance (T8's one construction path), so the memory
    /// resets with the selection, which is correct: the new model may
    /// differ. Nothing is written to the config.
    max_tokens_parameter: Cell<MaxTokensParam>,
    /// The flip is announced ONCE per instance, not once per turn.
    flip_announced: Cell<bool>,
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
            max_tokens_parameter: Cell::new(max_tokens_parameter),
            flip_announced: Cell::new(false),
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

    fn build_body(
        &self,
        req: &ChatRequest,
        max_tokens_parameter: MaxTokensParam,
    ) -> Result<String, ProviderError> {
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
        body[max_tokens_parameter.wire_key()] = serde_json::json!(req.max_tokens);
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
                Err(e) => {
                    return Err(ProviderError::Stream(format!(
                        "{e} (endpoint {})",
                        crate::provider::endpoint_label(&self.base_url)
                    )))
                }
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
        let sent = self.max_tokens_parameter.get();
        let body = self.build_body(req, sent)?;
        let first = crate::provider::transport::post_stream_with_retries(
            self.transport.as_ref(),
            &url,
            &self.api_key,
            &body,
            cancel,
        );
        let err = match first {
            Ok(reader) => return self.drive(req, reader, on_event, cancel),
            Err(e) => e,
        };
        // D19: the provider named the fix in the error body. Anything that is
        // not that exact rejection falls through with byte-identical text.
        if !is_token_cap_rejection(&err, sent) {
            return Err(transport_error_to_provider(err, &self.base_url));
        }
        // ONE retry with the other name and the SAME value, same URL, same
        // credential, same cancel token. post_stream_with_retries polls the
        // token before it posts, so a cancel landing between the 400 and here
        // means no second POST is ever made.
        let other = sent.other();
        let retry_body = self.build_body(req, other)?;
        match crate::provider::transport::post_stream_with_retries(
            self.transport.as_ref(),
            &url,
            &self.api_key,
            &retry_body,
            cancel,
        ) {
            Ok(reader) => {
                self.max_tokens_parameter.set(other);
                if !self.flip_announced.replace(true) {
                    // Before driving the response, which is where it
                    // naturally sits: the retry POST happens before any
                    // token arrives, so the notice cannot interleave with
                    // text that has already started.
                    on_event(StreamEvent::Notice(flip_notice(other)));
                }
                self.drive(req, reader, on_event, cancel)
            }
            // NEVER a third attempt. If the other name is rejected the same
            // way, both names are wrong for this endpoint and only the
            // operator can settle it, so say which knob settles it.
            Err(e2) if is_token_cap_rejection(&e2, other) => {
                Err(append_knob_hint(transport_error_to_provider(e2, &self.base_url)))
            }
            Err(e2) => Err(transport_error_to_provider(e2, &self.base_url)),
        }
    }
}

/// The one-sentence flip notice, raised once per provider instance.
fn flip_notice(now: MaxTokensParam) -> String {
    format!(
        "note: this model wants {}; using it for this session \
         (set \"max_tokens_parameter\" on the profile to skip the retry)",
        now.wire_key()
    )
}

/// D19's trigger, narrow by construction: a 400 whose body either names the
/// field we just sent AS UNSUPPORTED, or mentions both names in its message.
/// The second arm is required because the live observation carried only a
/// message; the `param` field is OpenAI's documented shape, not something we
/// captured. Neither key name is a substring of the other, so the message
/// test cannot fire on one name alone.
///
/// NAMING THE FIELD IS NOT ENOUGH ON ITS OWN. OpenAI sets `param` to
/// `max_tokens` for VALUE errors too ("max_tokens is too large"), and those
/// are not fixed by sending the same value under a different key: retrying
/// would waste a round trip and then blame the knob for a limit the operator
/// actually has to lower. Arm (a) therefore requires the error class as well,
/// which is exactly the class OpenAI documents for this case. Arm (b) needs
/// no such guard: a message carrying BOTH names is the server spelling out
/// the swap, and a value error has no reason to name the other key at all.
fn is_token_cap_rejection(e: &TransportError, sent: MaxTokensParam) -> bool {
    let TransportError::Status { code: 400, body, .. } = e else {
        return false;
    };
    let Some(err) = serde_json::from_str::<types::ErrorPayload>(body)
        .ok()
        .and_then(types::ErrorPayload::into_error)
        .map(types::WireError::into_body)
    else {
        return false;
    };
    let sent_key = sent.wire_key();
    let unsupported = err
        .code
        .as_ref()
        .and_then(|c| c.as_str())
        .is_some_and(|c| c == "unsupported_parameter");
    if unsupported && err.param.as_deref() == Some(sent_key) {
        return true;
    }
    err.message.contains(sent_key) && err.message.contains(sent.other().wire_key())
}

/// Both names refused: name the knob, keep the server's own words in front
/// of it so the operator still sees what the endpoint said.
fn append_knob_hint(e: ProviderError) -> ProviderError {
    match e {
        ProviderError::Api {
            status,
            kind,
            message,
        } => ProviderError::Api {
            status,
            kind,
            message: format!(
                "{message} (set \"max_tokens_parameter\" on the profile; temur tried both names)"
            ),
        },
        other => other,
    }
}

fn transport_error_to_provider(e: TransportError, base_url: &str) -> ProviderError {
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
        // T63 P3: a real network failure names the endpoint (host:port only).
        TransportError::Unreachable { refused: true, .. } => ProviderError::Network(format!(
            "nothing is listening at {}: is your model server running? (temur doctor checks reachability)",
            crate::provider::endpoint_label(base_url)
        )),
        TransportError::Unreachable { message, .. } => ProviderError::Network(format!(
            "{message} (endpoint {})",
            crate::provider::endpoint_label(base_url)
        )),
        // T50: the ordinary turn-error path, same as any other network
        // failure. Control returns and the session stays intact. The
        // "network: " prefix and the T21/T43 wording stay; T63 P3 appends
        // the endpoint.
        TransportError::Timeout { phase, .. } => ProviderError::Network(format!(
            "timed out waiting for {phase} from the server (endpoint {})",
            crate::provider::endpoint_label(base_url)
        )),
    }
}

#[cfg(test)]
mod endpoint_error_tests {
    use super::*;

    /// T63 P3: each network failure names host:port and never the
    /// userinfo or query of the base URL; the non-network `Io` values
    /// (an interrupt here) are left exactly as they were.
    #[test]
    fn a_network_error_names_the_endpoint_and_nothing_else_of_the_url() {
        let base = "http://user:secret@127.0.0.1:8080/v1?token=abc";
        let refused = transport_error_to_provider(
            TransportError::Unreachable { message: "io: Connection refused (os error 111)".into(), refused: true },
            base,
        )
        .to_string();
        assert_eq!(
            refused,
            "network: nothing is listening at 127.0.0.1:8080: is your model server running? (temur doctor checks reachability)"
        );
        let other = transport_error_to_provider(
            TransportError::Unreachable { message: "host not found".into(), refused: false },
            base,
        )
        .to_string();
        assert_eq!(other, "network: host not found (endpoint 127.0.0.1:8080)");
        let timeout = transport_error_to_provider(
            TransportError::Timeout { phase: "connect".into(), retryable: true },
            base,
        )
        .to_string();
        assert_eq!(timeout, "network: timed out waiting for connect from the server (endpoint 127.0.0.1:8080)");
        let interrupted =
            transport_error_to_provider(TransportError::Io("interrupted by user".into()), base).to_string();
        assert_eq!(interrupted, "network: interrupted by user");
        for m in [&refused, &other, &timeout] {
            assert!(!m.contains("secret") && !m.contains("token"), "{m}");
        }
    }
}
