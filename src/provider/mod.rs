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

/// The ONE live-provider construction path (T8): startup and `/model`
/// switches both come through here, so there is a single place where
/// credentials are read — by path, at activation time, never cached across
/// switches — and a single mapping from a resolved selection onto a
/// provider. Replay (`--mock`) and capture transports are startup-only
/// concerns and stay in main.
pub fn build_live(
    p: &crate::config::ResolvedProfile,
) -> Result<Box<dyn Provider>, crate::error::Error> {
    Ok(build_live_with_key(p)?.0)
}

/// [`build_live`] plus the credential it read, for T18 redaction: the tool
/// layer registers the ACTIVE key so tool output can never echo it. NO
/// additional key read happens here: the returned string is the very one
/// activation loaded (`None` for a keyless selection, which is also what
/// CLEARS a previously registered key on a switch to keyless).
#[allow(clippy::type_complexity)]
pub fn build_live_with_key(
    p: &crate::config::ResolvedProfile,
) -> Result<(Box<dyn Provider>, Option<String>), crate::error::Error> {
    if p.provider == "openai-compat" {
        // Keyless is first-class for local endpoints; a keyed endpoint reads
        // its credential BY PATH — the same isolation rule as
        // APP_SECRET_FILE, never env/argv.
        let key = match &p.api_key_file {
            Some(path) => Some(crate::secret::load_api_key_from(std::path::Path::new(path))?),
            None => None,
        };
        Ok((
            Box::new(openai_compat::OpenAiCompatProvider::with_http(
                p.base_url.clone(),
                key.clone(),
            )),
            key,
        ))
    } else {
        // Credential BY PATH: the profile's api_key_file when set, else
        // APP_SECRET_FILE (appsvc launcher). Deliberately never
        // ANTHROPIC_API_KEY.
        let key = match &p.api_key_file {
            Some(path) => crate::secret::load_api_key_from(std::path::Path::new(path))?,
            None => crate::secret::load_api_key()?,
        };
        Ok((
            Box::new(anthropic::AnthropicProvider::with_http(
                p.base_url.clone(),
                key.clone(),
            )),
            Some(key),
        ))
    }
}

/// The `/models` listing GET (T9). Follows [`build_live`]'s construction
/// rules exactly: credentials by path at call time, never cached, never
/// echoed. Anthropic: GET `{base}/v1/models` with `x-api-key` (profile key
/// file, else `APP_SECRET_FILE`) + `anthropic-version`. OpenAI-compat: GET
/// `{base}/models` (the base carries `/v1` by SDK convention) with
/// `Authorization: Bearer` only when a key file is configured — keyless
/// local endpoints send no auth header at all. Body read capped at 64 KiB
/// like the streaming transports; non-2xx is a clean error naming the
/// status, never echoing headers.
pub fn list_models_live(
    p: &crate::config::ResolvedProfile,
) -> Result<Vec<String>, crate::error::Error> {
    use std::io::Read;
    rustls::crypto::ring::default_provider().install_default().ok();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .new_agent();
    let (url, result) = if p.provider == "openai-compat" {
        let url = format!("{}/models", p.base_url.trim_end_matches('/'));
        let mut req = agent.get(&url);
        if let Some(path) = &p.api_key_file {
            let key = crate::secret::load_api_key_from(std::path::Path::new(path))?;
            req = req.header("authorization", &format!("Bearer {key}"));
        }
        (url, req.call())
    } else {
        let key = match &p.api_key_file {
            Some(path) => crate::secret::load_api_key_from(std::path::Path::new(path))?,
            None => crate::secret::load_api_key()?,
        };
        let url = format!("{}/v1/models", p.base_url.trim_end_matches('/'));
        let req = agent
            .get(&url)
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01");
        (url, req.call())
    };
    let res = result
        .map_err(|e| crate::error::Error::Models(format!("model listing GET {url}: {e}")))?;
    let status = res.status().as_u16();
    let mut body = String::new();
    let _ = res
        .into_body()
        .into_reader()
        .take(64 * 1024)
        .read_to_string(&mut body);
    if !(200..300).contains(&status) {
        return Err(crate::error::Error::Models(format!(
            "model listing GET {url}: HTTP {status}"
        )));
    }
    parse_models_json(&body)
}

/// Serialize a request body with recursively SORTED object keys — the
/// exact byte order this wire has had since T1, when bodies were built on
/// serde_json's default BTreeMap and keys serialized alphabetically. T15
/// enabled serde_json's preserve_order feature (so `/model --save` keeps
/// the user's config key order), which would silently flip request bodies
/// to insertion order; sorting at this boundary pins the historical bytes
/// instead, and the request_golden suite keeps enforcing them.
pub fn to_sorted_json_string(v: &Value) -> Result<String, serde_json::Error> {
    fn sorted(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                let mut out = serde_json::Map::new();
                for k in keys {
                    out.insert(k.clone(), sorted(&m[k.as_str()]));
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    serde_json::to_string(&sorted(v))
}

/// Seconds of global timeout on a keyless listing GET: long enough for a
/// LAN model server, short enough that a wedged one cannot stall the init
/// wizard or a doctor report.
pub const KEYLESS_LISTING_TIMEOUT_SECS: u64 = 3;

/// The ONE listing request `init` and `doctor` are allowed to make (T15):
/// an UNAUTHENTICATED GET of `{base}/models`, meant only for KEYLESS
/// openai-compat endpoints. By construction it takes just a base URL, so it
/// can never attach an auth header or touch a key file — the T15 security
/// amendment in one signature. Unlike [`list_models_live`]'s agent, this
/// one sets a global timeout: a wizard or report must not hang on a dead
/// server. Body cap and error shapes match `list_models_live`.
pub fn list_models_keyless(
    base_url: &str,
    timeout: std::time::Duration,
) -> Result<Vec<String>, crate::error::Error> {
    use std::io::Read;
    rustls::crypto::ring::default_provider().install_default().ok();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build()
        .new_agent();
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let res = agent
        .get(&url)
        .call()
        .map_err(|e| crate::error::Error::Models(format!("model listing GET {url}: {e}")))?;
    let status = res.status().as_u16();
    let mut body = String::new();
    let _ = res
        .into_body()
        .into_reader()
        .take(64 * 1024)
        .read_to_string(&mut body);
    if !(200..300).contains(&status) {
        return Err(crate::error::Error::Models(format!(
            "model listing GET {url}: HTTP {status}"
        )));
    }
    parse_models_json(&body)
}

/// Extract `data[].id` from a model-listing body — the envelope BOTH wires
/// share (Anthropic `GET /v1/models` and OpenAI-compat `GET /models`).
/// Pure, so the parsing is unit-testable offline against literal JSON.
/// Entries without a string `id` are skipped; an empty `data` array is a
/// valid empty listing.
pub fn parse_models_json(body: &str) -> Result<Vec<String>, crate::error::Error> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| crate::error::Error::Models(format!("model listing: bad JSON: {e}")))?;
    let Some(data) = v.get("data").and_then(|d| d.as_array()) else {
        return Err(crate::error::Error::Models(
            "model listing: no \"data\" array in the response".into(),
        ));
    };
    Ok(data
        .iter()
        .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(str::to_string))
        .collect())
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
