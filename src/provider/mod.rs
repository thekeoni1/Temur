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

/// Which wire key carries [`ChatRequest::max_tokens`] on the
/// OpenAI-compatible wire (T25 F7). Validated when the config is resolved,
/// so holding one is proof the operator wrote one of the two names that
/// wire actually accepts, and exactly one of them ever appears in a body.
/// Anthropic's wire is not configurable here: it uses `max_tokens`
/// natively and nothing in this type reaches it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MaxTokensParam {
    /// The classic name, and the default: llama.cpp, Ollama, OpenRouter,
    /// DeepSeek and friends all speak it, and several never learned
    /// anything else.
    #[default]
    MaxTokens,
    /// What OpenAI-proper's gpt-5 era ids require instead, rejecting
    /// `max_tokens` outright (T13 acceptance run of 2026-08-05).
    MaxCompletionTokens,
}

impl MaxTokensParam {
    /// Config spelling to value; the accepted spellings are the wire keys
    /// themselves. `None` = not one of the two, which callers turn into a
    /// config error naming both.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "max_tokens" => Some(MaxTokensParam::MaxTokens),
            "max_completion_tokens" => Some(MaxTokensParam::MaxCompletionTokens),
            _ => None,
        }
    }

    /// The request key name to emit.
    pub fn wire_key(self) -> &'static str {
        match self {
            MaxTokensParam::MaxTokens => "max_tokens",
            MaxTokensParam::MaxCompletionTokens => "max_completion_tokens",
        }
    }

    /// The other of the two. D19: a server that rejects one of these names
    /// is telling us to send the other, and there are only ever two, so the
    /// retry target is a total function rather than a search.
    pub fn other(self) -> Self {
        match self {
            MaxTokensParam::MaxTokens => MaxTokensParam::MaxCompletionTokens,
            MaxTokensParam::MaxCompletionTokens => MaxTokensParam::MaxTokens,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    /// Response token cap. Neutral name — providers map it to their own
    /// field. Anthropic always calls it `max_tokens` on the wire. The
    /// OpenAI-compatible provider defaults to that same classic name, which
    /// the compat universe speaks universally, but emits
    /// `max_completion_tokens` instead when the profile asks for it (T25
    /// F7): OpenAI-proper's gpt-5 era ids reject the classic name. See
    /// [`MaxTokensParam`]; the value carried is identical either way.
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
    /// Something the user should read about the request itself, not about
    /// its content: raised by a provider that had to adapt and wants to say
    /// so once (D19's token-cap-name retry). Deliberately a stream event
    /// rather than a return value, because it is emitted BEFORE the
    /// response it explains starts arriving.
    Notice(String),
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
                p.effective_max_tokens_parameter(),
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
) -> Result<Vec<ModelEntry>, crate::error::Error> {
    list_models_live_with_timeout(p, None)
}

/// [`list_models_live`] with an optional GLOBAL timeout, for callers that
/// must not hang on a slow endpoint.
///
/// `/models` passes `None` and keeps the untimed behaviour it has had
/// since T9: the user typed the command and is waiting for its answer. T56
/// gave doctor the keyed model check, and a report that stalls on one
/// hosted endpoint is a worse report, so doctor passes the same short
/// bound the keyless listing uses.
pub fn list_models_live_with_timeout(
    p: &crate::config::ResolvedProfile,
    timeout: Option<std::time::Duration>,
) -> Result<Vec<ModelEntry>, crate::error::Error> {
    use std::io::Read;
    rustls::crypto::ring::default_provider().install_default().ok();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(timeout)
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
    parse_models_entries(&body)
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

/// Seconds of global timeout on ONE tools-drop probe POST.
///
/// Much longer than its listing siblings, and measured rather than
/// guessed. The T34 probe sends temur's real tool definitions, which is
/// ~29KB of JSON on the full prompt profile, and the server must PREFILL
/// all of it before it can report a single token of usage. Measured
/// 2026-08-18 against llama.cpp b10438 on this CPU-only machine,
/// Phi-4-mini at ctx 8192: 4814 new prompt tokens at 22.6 ms/token, 106
/// seconds wall clock. The 3-second listing timeout turned that into a
/// silent "no usable prompt_tokens" NOTE.
///
/// This is not a cost the probe invents: it is exactly the prefill the
/// session's FIRST REAL TURN would pay, for exactly the same bytes, and
/// llama.cpp's prompt cache means paying it here warms it for that turn.
/// A second doctor run against the same server returns quickly.
pub const TOOLS_DROP_PROBE_TIMEOUT_SECS: u64 = 300;

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

/// Derive the llama.cpp `/props` URL from an openai-compat base URL. The
/// endpoint lives at the server ROOT, not under the SDK-conventional
/// `/v1`, so a trailing `/v1` is stripped. Pure, unit-tested.
pub fn props_url(base_url: &str) -> String {
    let root = base_url.trim_end_matches('/');
    let root = root.strip_suffix("/v1").unwrap_or(root);
    format!("{root}/props")
}

/// Extract `default_generation_settings.n_ctx` from a llama.cpp `/props`
/// body: the server's ACTUAL context allocation (its `-c` flag), which is
/// the true local limit whatever the model's trained context is. `None`
/// for anything else (bad JSON, missing fields, zero). Pure, unit-tested
/// against canned JSON.
pub fn parse_props_context(body: &str) -> Option<u64> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get("default_generation_settings")?
        .get("n_ctx")?
        .as_u64()
        .filter(|&n| n > 0)
}

/// The SECOND (and last) keyless request init and doctor may make (T22),
/// under the same amendment contract as [`list_models_keyless`]: an
/// unauthenticated GET of `{root}/props`, taking only a base URL, so it
/// can never attach an auth header or touch a key file by construction.
/// Same global timeout discipline as the keyless listing. Returns the
/// server's context allocation, or `None` for ANY problem (network, HTTP
/// status, unparseable body): non-llama.cpp servers 404 or answer
/// something else here, and that is normal, not an error.
pub fn probe_props_context(base_url: &str, timeout: std::time::Duration) -> Option<u64> {
    use std::io::Read;
    rustls::crypto::ring::default_provider().install_default().ok();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build()
        .new_agent();
    let res = agent.get(&props_url(base_url)).call().ok()?;
    if !(200..300).contains(&res.status().as_u16()) {
        return None;
    }
    let mut body = String::new();
    let _ = res
        .into_body()
        .into_reader()
        .take(64 * 1024)
        .read_to_string(&mut body);
    parse_props_context(&body)
}

/// The probe body for the T31 tools-drop check: a one-word completion
/// capped at one generated token, sent once bare (`tools` = `None`) and
/// once carrying the caller's REAL tool definitions. Pure, so the wire
/// shape is unit-testable without a server.
///
/// T34: this used to send one synthetic minimal tool, and that made the
/// check answer a question nobody asked. A server can render a toy schema
/// perfectly and still reject everything temur actually sends: on
/// 2026-08-17 the Hermes-2-Pro template probed PASS (221 -> 290
/// prompt_tokens) while every real request 400d on the `skill` tool's
/// union type. Probing with the definitions the session will really send
/// makes the probe answer the question doctor is being asked.
///
/// The definitions go out through the SAME openai-compat mapping the
/// provider uses ([`openai_compat::types::ToolDef`]), never a second
/// hand-rolled copy, so what the probe measures is what a turn would send.
///
/// Non-streaming on purpose: doctor wants the `usage` block, not deltas,
/// and every compat server reports usage on a non-streamed response.
///
/// T41 added `system`, for the prompt-floor probe: the same request shape
/// with the session's real system prompt as a leading system message, so
/// the count that comes back is the real floor rather than the tools half
/// of it. `None` (what both tools-drop probes pass) produces a
/// BYTE-IDENTICAL body to the pre-T41 one, which matters because the
/// tools-drop comparison is a published T34 baseline.
pub fn tools_drop_probe_body(
    model: &str,
    tools: Option<&[ToolDef]>,
    system: Option<&str>,
) -> String {
    let messages = match system {
        None => serde_json::json!([{"role": "user", "content": "hi"}]),
        Some(s) => serde_json::json!([
            {"role": "system", "content": s},
            {"role": "user", "content": "hi"},
        ]),
    };
    let mut body = serde_json::json!({
        "model": model,
        "stream": false,
        "max_tokens": 1,
        "messages": messages,
    });
    if let Some(defs) = tools {
        let wire: Vec<openai_compat::types::ToolDef> =
            defs.iter().map(openai_compat::types::ToolDef::from).collect();
        body["tools"] = serde_json::to_value(wire).unwrap_or_else(|_| serde_json::json!([]));
    }
    to_sorted_json_string(&body).unwrap_or_else(|_| body.to_string())
}

/// How much of a server's error message a probe carries back. Long enough
/// for the diagnostic line that matters (llama.cpp's template failure names
/// the macro, the line, and the column), short enough that doctor stays one
/// readable line per check.
const PROBE_ERROR_MESSAGE_CAP: usize = 300;

/// The server's error message from a non-2xx probe response, as one line.
///
/// Reuses the same [`openai_compat::types::ErrorPayload`] the streaming
/// path parses, so every envelope shape already handled there (bare object,
/// bare string, Google's one-element array) is handled here too. A body
/// that is not a recognizable error envelope degrades to the raw text,
/// which for a 404 HTML page is still more useful than nothing.
/// Whitespace is collapsed because the message worth quoting is often a
/// multi-line template traceback. Pure, unit-tested.
pub fn parse_probe_error_message(body: &str) -> String {
    let raw = serde_json::from_str::<openai_compat::types::ErrorPayload>(body)
        .ok()
        .and_then(openai_compat::types::ErrorPayload::into_error)
        .map(openai_compat::types::WireError::into_body)
        .map(|b| b.message)
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| body.to_string());
    let one_line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > PROBE_ERROR_MESSAGE_CAP {
        let cut: String = one_line.chars().take(PROBE_ERROR_MESSAGE_CAP).collect();
        format!("{cut}...")
    } else {
        one_line
    }
}

/// Extract `usage.prompt_tokens` from a non-streamed completion body.
/// `None` for anything else: servers that report no usage are a known part
/// of the world, and the caller degrades to a NOTE. Pure, unit-tested.
pub fn parse_prompt_tokens(body: &str) -> Option<u64> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get("usage")?
        .get("prompt_tokens")?
        .as_u64()
}

/// What one tools-drop probe request found out.
///
/// T34: this used to be `Option<u64>`, which threw away the only thing
/// that distinguishes "the server is not talking to us" from "the server
/// looked at temur's tool definitions and refused them". That second case
/// is the one worth a diagnosis: it means every real turn will fail the
/// same way, for a reason the server already stated.
#[derive(Debug, Clone, PartialEq)]
pub enum ProbeOutcome {
    /// The server answered 2xx and reported `usage.prompt_tokens`.
    Ok(u64),
    /// The server answered, and said no. `message` is its own words,
    /// already collapsed to one line (see [`parse_probe_error_message`]).
    HttpError { status: u16, message: String },
    /// 2xx, but no usable `usage.prompt_tokens`: a known part of the world
    /// (some servers report no usage at all), and nothing to compare.
    NoUsage,
    /// Never got an answer: connect, TLS, or timeout.
    Unreachable,
}

/// The THIRD (and last) keyless request doctor may make (T31), under the
/// same amendment contract as [`list_models_keyless`] and
/// [`probe_props_context`]: it takes a base URL, a model id, and a
/// serialized set of tool definitions, and NOTHING else, so it can never
/// attach an auth header or touch a key file by construction. Unlike its
/// two GET siblings this one POSTs, which is why it is capped at ONE
/// generated token: the cost of a call is a handful of prompt tokens plus
/// a single token of output, on a local server.
///
/// T34 widened only WHAT is sent (the caller's real definitions instead of
/// one synthetic tool) and what comes back (a [`ProbeOutcome`] instead of
/// an `Option`). T41 added `system`, for doctor's prompt-floor
/// measurement. The contract above is unchanged and still assertable:
/// there is still no credential-shaped parameter to pass, and adding one
/// would be visible in this signature.
pub fn probe_prompt_tokens(
    base_url: &str,
    model: &str,
    tools: Option<&[ToolDef]>,
    system: Option<&str>,
    timeout: std::time::Duration,
) -> ProbeOutcome {
    use std::io::Read;
    rustls::crypto::ring::default_provider().install_default().ok();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build()
        .new_agent();
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let res = match agent
        .post(&url)
        .header("content-type", "application/json")
        .send(tools_drop_probe_body(model, tools, system))
    {
        Ok(res) => res,
        Err(_) => return ProbeOutcome::Unreachable,
    };
    let status = res.status().as_u16();
    let mut body = String::new();
    let _ = res
        .into_body()
        .into_reader()
        .take(64 * 1024)
        .read_to_string(&mut body);
    if !(200..300).contains(&status) {
        return ProbeOutcome::HttpError {
            status,
            message: parse_probe_error_message(&body),
        };
    }
    match parse_prompt_tokens(&body) {
        Some(n) => ProbeOutcome::Ok(n),
        None => ProbeOutcome::NoUsage,
    }
}

/// One row of a model listing (T22): the id both wires share, plus the
/// context window where a wire reports one. The Anthropic listing carries
/// a per-model `max_input_tokens` ("maximum input context window size in
/// tokens"); 0 or absent is unknown. OpenAI-compat listings have no such
/// field, so their windows are always `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelEntry {
    pub id: String,
    pub context_window: Option<u64>,
}

/// Extract `data[].id` (+ `max_input_tokens` where present) from a
/// model-listing body, the envelope BOTH wires share (Anthropic
/// `GET /v1/models` and OpenAI-compat `GET /models`). Pure, so the
/// parsing is unit-testable offline against literal JSON. Entries without
/// a string `id` are skipped; an empty `data` array is a valid empty
/// listing.
pub fn parse_models_entries(body: &str) -> Result<Vec<ModelEntry>, crate::error::Error> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| crate::error::Error::Models(format!("model listing: bad JSON: {e}")))?;
    let Some(data) = v.get("data").and_then(|d| d.as_array()) else {
        return Err(crate::error::Error::Models(
            "model listing: no \"data\" array in the response".into(),
        ));
    };
    Ok(data
        .iter()
        .filter_map(|m| {
            let id = m.get("id").and_then(|i| i.as_str())?;
            let window = m
                .get("max_input_tokens")
                .and_then(Value::as_u64)
                .filter(|&w| w > 0);
            Some(ModelEntry { id: id.to_string(), context_window: window })
        })
        .collect())
}

/// [`parse_models_entries`] reduced to bare ids: what the keyless
/// listing (init, doctor) consumes; windows are a `/models`-command
/// concern only.
pub fn parse_models_json(body: &str) -> Result<Vec<String>, crate::error::Error> {
    Ok(parse_models_entries(body)?.into_iter().map(|e| e.id).collect())
}

/// Is `id` the DATED form of the alias `model`, i.e. `<model>-YYYYMMDD`?
///
/// The one rule that decides "absent from a listing is not the same as
/// invalid". Anthropic's `/v1/models` lists only dated ids, so a live
/// alias like `claude-haiku-4-5` is missing from the listing that serves
/// it, while `claude-haiku-4-5-20251001` is there. Eight ASCII digits
/// after a `-`, nothing looser: a date is what distinguishes a real alias
/// from a different model whose name happens to start the same way.
///
/// T13 discovered this for the `/models` context-window notice
/// ([`crate::commands`]); T56 gave doctor's keyed model check the same
/// question to answer. It lives here, called by both, so the two can
/// never drift into judging the same id differently.
pub fn is_dated_alias(model: &str, id: &str) -> bool {
    id.strip_prefix(model)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|d| d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()))
}

/// The dated id an alias would be served by: the NEWEST `<model>-YYYYMMDD`
/// in the listing, or `None` when nothing matches. The candidates share
/// the alias prefix and end in eight digits, so lexicographic order IS
/// date order.
pub fn newest_dated_alias<'a>(model: &str, ids: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    ids.filter(|id| is_dated_alias(model, id)).max()
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

/// What [`endpoint_label`] prints when a base URL has no clean host and port.
pub const ENDPOINT_FALLBACK: &str = "the configured endpoint";

/// The `host:port` of a base URL, for naming an endpoint in an error message
/// (T63 P3). Hand-rolled like the other base-URL helpers: the scheme picks
/// the default port, and userinfo, path, query and fragment are dropped, so a
/// credential or a query token in the URL never reaches an error. Anything
/// left that is not a host or port character yields "the configured
/// endpoint" instead of a partial string.
pub fn endpoint_label(base_url: &str) -> String {
    const FALLBACK: &str = ENDPOINT_FALLBACK;
    let (default_port, rest) = if let Some(r) = base_url.strip_prefix("https://") {
        ("443", r)
    } else if let Some(r) = base_url.strip_prefix("http://") {
        ("80", r)
    } else {
        ("", base_url)
    };
    // An '@' after the first '/', '?' or '#' cannot be told apart from a
    // password that contains one of those raw, and naming what precedes it
    // could print part of that password. Name nothing instead.
    if let (Some(at), Some(delim)) = (rest.rfind('@'), rest.find(['/', '?', '#'])) {
        if delim < at {
            return FALLBACK.to_string();
        }
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);
    let clean = !host_port.is_empty()
        && host_port
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']' | '_' | '%'));
    if !clean {
        return FALLBACK.to_string();
    }
    // A bracketed IPv6 literal has colons of its own; its port, if any,
    // follows the closing bracket.
    let port_part = match host_port.rfind(']') {
        Some(close) => host_port[close..].rsplit_once(':').map(|(_, p)| p),
        None => host_port.rsplit_once(':').map(|(_, p)| p),
    };
    let has_port = match port_part {
        Some(p) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => true,
        Some(_) => return FALLBACK.to_string(),
        None => false,
    };
    if has_port || default_port.is_empty() {
        host_port.to_string()
    } else {
        format!("{host_port}:{default_port}")
    }
}

#[cfg(test)]
mod endpoint_label_tests {
    use super::endpoint_label;

    #[test]
    fn it_is_host_and_port_with_the_scheme_default() {
        assert_eq!(endpoint_label("http://127.0.0.1:8080/v1"), "127.0.0.1:8080");
        assert_eq!(endpoint_label("https://api.openai.com/v1"), "api.openai.com:443");
        assert_eq!(endpoint_label("http://localhost/v1"), "localhost:80");
        assert_eq!(endpoint_label("https://api.anthropic.com"), "api.anthropic.com:443");
        assert_eq!(endpoint_label("http://[::1]:8080/v1"), "[::1]:8080");
        assert_eq!(endpoint_label("http://[::1]/v1"), "[::1]:80");
    }

    #[test]
    fn it_never_carries_userinfo_a_query_or_a_fragment() {
        let out = endpoint_label("https://user:secret@example.com:8443/v1?key=abc#frag");
        assert_eq!(out, "example.com:8443");
        assert_eq!(endpoint_label("https://example.com?token=x"), "example.com:443");
        // Malformed: a space, no host, a raw '/', '?' or '#' inside the
        // password, a non-numeric port. Each names nothing rather than part of the URL.
        for bad in [
            "http://ex ample.com/v1",
            "http:///v1",
            "http://user:pa/ss@host/v1",
            "http://user:1234?x@host/v1",
            "http://user:12#34@host/v1",
            "http://host:pa/v1",
        ] {
            assert_eq!(endpoint_label(bad), "the configured endpoint", "{bad:?}");
        }
    }
}

/// Where an openai-compat base URL points (T63 P4 D26, widened by T65 P3,
/// review finding 5).
///
/// THREE kinds, not two. `Local` is this machine or an address only this
/// machine's network stack can reach. `LocalNetwork` is a box on the user's
/// own LAN reached by a name rather than an address, `gpu-box` or
/// `nas.home`, which T63 P4 had to call hosted for want of a third answer:
/// it costs nothing and it is not this machine. `Hosted` is everything else,
/// and stays the answer whenever the endpoint cannot be named at all, since
/// hosted is the side that can cost money and the safer thing to say when
/// unsure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locality {
    Local,
    LocalNetwork,
    Hosted,
}

/// The [`Locality`] of a base URL.
///
/// Order matters. The unnameable URL is rejected FIRST, exactly as
/// [`is_local_endpoint`] did before this, so a URL with an embedded slash or
/// no host cannot fall through to the bare-name rule below and be called
/// local network. Then today's `Local` set, unchanged to the character.
/// Then the LAN rule: a host with no `.` in it that is not an IP literal
/// (`gpu-box`), or one of the four suffixes a home or office network hands
/// out. Anything with a dot that is not one of those is hosted, which keeps
/// `b.test`, `api.openai.com` and a bare `8.8.8.8` where they were.
pub fn endpoint_kind(base_url: &str) -> Locality {
    let label = endpoint_label(base_url);
    if label == ENDPOINT_FALLBACK {
        return Locality::Hosted;
    }
    let host = host_part(&label);
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let lower = bare.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".local") {
        return Locality::Local;
    }
    if let Ok(ip) = bare.parse::<std::net::Ipv4Addr>() {
        return if ip.is_loopback() || ip.is_private() || ip.is_link_local() {
            Locality::Local
        } else {
            Locality::Hosted
        };
    }
    if let Ok(ip) = bare.parse::<std::net::Ipv6Addr>() {
        return if ip.is_loopback() || (ip.segments()[0] & 0xffc0) == 0xfe80 {
            Locality::Local
        } else {
            Locality::Hosted
        };
    }
    let lan_suffix = [".lan", ".home", ".internal", ".home.arpa"]
        .iter()
        .any(|s| lower.ends_with(s));
    if lan_suffix || !lower.contains('.') {
        return Locality::LocalNetwork;
    }
    Locality::Hosted
}

/// Whether a base URL costs nothing to call: this machine OR the user's own
/// network. Since T65 P3 that is two of the three [`Locality`] kinds, which
/// is why doctor.rs's hosted cost NOTE is not printed for a LAN box and why
/// that site did not have to change.
pub fn is_local_endpoint(base_url: &str) -> bool {
    matches!(endpoint_kind(base_url), Locality::Local | Locality::LocalNetwork)
}

/// The locality label an openai-compat selection shows (T63 P4 D26, a third
/// form since T65 P3): `local host:port`, `local network host:port` or
/// `hosted host`. Built on [`endpoint_label`], so it never carries userinfo,
/// a path or a query. The `local` and `hosted` forms are unchanged.
pub fn endpoint_locality(base_url: &str) -> String {
    let label = endpoint_label(base_url);
    match endpoint_kind(base_url) {
        Locality::Local => format!("local {label}"),
        Locality::LocalNetwork => format!("local network {label}"),
        Locality::Hosted if label == ENDPOINT_FALLBACK => "hosted endpoint".to_string(),
        Locality::Hosted => format!("hosted {}", host_part(&label)),
    }
}

/// The host of an [`endpoint_label`] `host:port`, brackets kept on IPv6.
fn host_part(host_port: &str) -> &str {
    if host_port.starts_with('[') {
        return host_port.find(']').map_or(host_port, |close| &host_port[..=close]);
    }
    match host_port.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => h,
        _ => host_port,
    }
}

/// T63 P4 (D25, Ruling T63-7): what a llama.cpp listing says about context
/// in the CONFIGURED model's `meta` block, as `(served, trained)` from
/// `n_ctx` and `n_ctx_train`. Each is `None` when absent, zero or
/// unparseable. Pure, unit-tested against canned JSON.
///
/// Which entry (T65 P3, review finding 4). Reading `data[0]` was wrong the
/// moment a server listed more than one model: a multi-model proxy would
/// report the first model's window for whatever the user had configured.
/// The rule now:
///
/// - exactly one entry: that entry, whatever its `id`. A single-model server
///   serves the model it was started with, and llama.cpp lists its gguf PATH
///   rather than the configured name, so matching on `id` here would break
///   the common case. This keeps the primary path byte-identical.
/// - several entries: the one whose `id` equals `model` exactly.
/// - several entries and none matches: `(None, None)`, so served falls to
///   `/props` as before and trained stays unknown. Guessing an entry would
///   be worse than not answering.
pub fn parse_models_context(body: &str, model: &str) -> (Option<u64>, Option<u64>) {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return (None, None);
    };
    let data = v.get("data").and_then(Value::as_array);
    let entry = match data {
        Some(list) if list.len() == 1 => list.first(),
        Some(list) => list
            .iter()
            .find(|e| e.get("id").and_then(Value::as_str) == Some(model)),
        None => None,
    };
    let meta = entry.and_then(|m| m.get("meta"));
    let field = |k: &str| meta.and_then(|m| m.get(k)).and_then(Value::as_u64).filter(|&n| n > 0);
    (field("n_ctx"), field("n_ctx_train"))
}

/// Served and trained context for a keyless endpoint (T63 P4, D25).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerContext {
    /// The server's context allocation.
    pub served: Option<u64>,
    /// The model's trained context, known only from a listing `meta` block.
    pub trained: Option<u64>,
    /// Which request produced `served`: `/v1/models` or `/props`.
    pub served_from: &'static str,
}

/// What a caller needs from [`probe_server_context`] (T65 P3, review
/// finding 8).
///
/// An enum with named variants rather than a bool, for P2's reason: at the
/// call site `ServedAndTrained` says what is wanted, where `true` would say
/// nothing and the reader would have to find the parameter's name. The two
/// cases are not symmetric either, so neither polarity of a bool reads
/// correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wanted {
    /// The caller has no configured window and will adopt the served size,
    /// so a missing served size is worth a second request. Today's
    /// behaviour, `/props` fallback included.
    ServedAndTrained,
    /// The caller already has a window and only needs the trained size, for
    /// the wording of a notice. ONE request, never `/props`: the fallback
    /// can only supply served, which this caller would discard.
    TrainedOnly,
}

/// T63 P4 (D25, Ruling T63-7): ONE bounded keyless GET of `{base}/models`
/// reads served and trained from the configured model's `meta`. When `want`
/// is [`Wanted::ServedAndTrained`] and that yields no served size (an older
/// llama.cpp, a server with no `meta`, or any failure),
/// [`probe_props_context`] supplies served alone and trained stays `None`,
/// so a server sees at most two requests. When `want` is
/// [`Wanted::TrainedOnly`] there is exactly one request and `served_from` is
/// `/v1/models` whatever the listing said, because nothing will read served
/// (T65 P3, review finding 8). Same base-URL-only signature as the other
/// keyless probes, so it cannot attach auth, and the same "None on any
/// problem" contract.
pub fn probe_server_context(
    base_url: &str,
    model: &str,
    want: Wanted,
    timeout: std::time::Duration,
) -> ServerContext {
    use std::io::Read;
    rustls::crypto::ring::default_provider().install_default().ok();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build()
        .new_agent();
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let (served, trained) = match agent.get(&url).call() {
        Ok(res) if (200..300).contains(&res.status().as_u16()) => {
            let mut body = String::new();
            let _ = res
                .into_body()
                .into_reader()
                .take(64 * 1024)
                .read_to_string(&mut body);
            parse_models_context(&body, model)
        }
        _ => (None, None),
    };
    if served.is_some() || want == Wanted::TrainedOnly {
        return ServerContext { served, trained, served_from: "/v1/models" };
    }
    ServerContext {
        served: probe_props_context(base_url, timeout),
        trained,
        served_from: "/props",
    }
}

#[cfg(test)]
mod locality_and_server_context_tests {
    use super::*;

    #[test]
    fn local_means_this_machine_or_its_private_network() {
        for local in [
            "http://127.0.0.1:8080/v1",
            "http://localhost:8080/v1",
            "http://10.0.0.5:8080/v1",
            "http://172.16.3.4/v1",
            "http://192.168.1.20:1234/v1",
            "http://169.254.10.1/v1",
            "http://gpu-box.local:8080/v1",
            "http://[::1]:8080/v1",
            "http://[fe80::1]/v1",
        ] {
            assert!(is_local_endpoint(local), "{local}");
            // T65 P4 (item 6, Ruling T65-11 (B)): starts_with("local ") also
            // accepts "local network ", so on its own it cannot tell the two
            // local kinds apart. The kind is asserted outright beside it.
            assert_eq!(endpoint_kind(local), Locality::Local, "{local}");
            assert!(endpoint_locality(local).starts_with("local "), "{local}");
        }
        for hosted in [
            "https://api.openai.com/v1",
            "http://8.8.8.8/v1",
            "http://172.32.0.1/v1",
            "http://b.test/v1",
            "http://ex ample.com/v1",
        ] {
            assert!(!is_local_endpoint(hosted), "{hosted}");
            assert_eq!(endpoint_kind(hosted), Locality::Hosted, "{hosted}");
            assert!(endpoint_locality(hosted).starts_with("hosted "), "{hosted}");
        }
    }

    #[test]
    fn the_locality_label_is_short_and_names_nothing_but_the_host() {
        assert_eq!(endpoint_locality("http://127.0.0.1:8080/v1"), "local 127.0.0.1:8080");
        assert_eq!(endpoint_locality("https://api.openai.com/v1"), "hosted api.openai.com");
        assert_eq!(endpoint_locality("http://b.test/v1"), "hosted b.test");
        assert_eq!(
            endpoint_locality("https://user:secret@api.example.com/v1?key=abc"),
            "hosted api.example.com"
        );
        assert_eq!(endpoint_locality("http://[::1]:8080/v1"), "local [::1]:8080");
        assert_eq!(endpoint_locality("http://user:pa/ss@host/v1"), "hosted endpoint");
    }

    #[test]
    fn models_meta_gives_served_and_trained() {
        // T65 P3: the model argument is new; every assertion below is the one
        // T63 P4 pinned. "configured-model" names no entry in any of these,
        // which is deliberate: each single-entry listing is read whatever its
        // id says, so these cases pass with any model at all.
        let m = "configured-model";
        let both = r#"{"object":"list","data":[{"id":"/model.gguf","meta":{"n_ctx":12288,"n_ctx_train":262144,"n_vocab":151936}}]}"#;
        assert_eq!(parse_models_context(both, m), (Some(12288), Some(262144)));
        let served_only = r#"{"data":[{"id":"m","meta":{"n_ctx":8192}}]}"#;
        assert_eq!(parse_models_context(served_only, m), (Some(8192), None));
        let no_meta = r#"{"data":[{"id":"served-a"},{"id":"served-b"}]}"#;
        assert_eq!(parse_models_context(no_meta, m), (None, None));
        let zero = r#"{"data":[{"id":"m","meta":{"n_ctx":0,"n_ctx_train":0}}]}"#;
        assert_eq!(parse_models_context(zero, m), (None, None));
        assert_eq!(parse_models_context("not json", m), (None, None));
        assert_eq!(parse_models_context(r#"{"data":[]}"#, m), (None, None));
    }

    /// T65 P3 (review finding 4): which entry of a multi-model listing is
    /// read. The single-entry path is the one llama.cpp takes and must not
    /// depend on the id at all; a real multi-model proxy is matched by id;
    /// no match is no answer rather than a guess.
    #[test]
    fn a_listing_is_read_at_the_configured_model() {
        let two = r#"{"object":"list","data":[
            {"id":"small","meta":{"n_ctx":4096,"n_ctx_train":32768}},
            {"id":"big","meta":{"n_ctx":12288,"n_ctx_train":262144}}]}"#;
        assert_eq!(parse_models_context(two, "big"), (Some(12288), Some(262144)));
        assert_eq!(parse_models_context(two, "small"), (Some(4096), Some(32768)));
        assert_eq!(
            parse_models_context(two, "neither"),
            (None, None),
            "no match is (None, None), so served falls to /props and trained stays unknown"
        );
        // The primary path: llama.cpp lists the gguf PATH, never the name the
        // user configured, so a single entry is read whatever its id says.
        let one = r#"{"data":[{"id":"/models/Qwen3-4B.gguf","meta":{"n_ctx":8192,"n_ctx_train":262144}}]}"#;
        assert_eq!(parse_models_context(one, "qwen3-4b"), (Some(8192), Some(262144)));
    }

    /// T65 P3 (review finding 5): a box on the user's own LAN, named rather
    /// than addressed, is neither this machine nor a hosted provider.
    #[test]
    fn a_lan_box_is_local_network_and_costs_nothing() {
        assert!(is_local_endpoint("http://gpu-box:8080/v1"));
        assert_eq!(endpoint_kind("http://gpu-box:8080/v1"), Locality::LocalNetwork);
        assert_eq!(endpoint_locality("http://gpu-box:8080/v1"), "local network gpu-box:8080");
        for lan in [
            "http://gpu-box.lan:8080/v1",
            "http://nas.home/v1",
            "http://llm.internal:8000/v1",
            "http://box.home.arpa/v1",
        ] {
            assert_eq!(endpoint_kind(lan), Locality::LocalNetwork, "{lan}");
            assert!(is_local_endpoint(lan), "{lan}");
        }
        for hosted in ["http://b.test/v1", "https://api.openai.com/v1", "http://8.8.8.8/v1"] {
            assert_eq!(endpoint_kind(hosted), Locality::Hosted, "{hosted}");
            assert!(!is_local_endpoint(hosted), "{hosted}");
        }
        // `.local` is mDNS, this machine's own network stack, and stays Local
        // with the label it has always had.
        assert_eq!(endpoint_kind("http://gpu-box.local:8080/v1"), Locality::Local);
        assert_eq!(
            endpoint_locality("http://gpu-box.local:8080/v1"),
            "local gpu-box.local:8080"
        );
    }

    #[test]
    fn an_unreachable_server_gives_nothing_and_says_where_it_looked_last() {
        // Nothing listens on port 1: both GETs fail inside the bound.
        let ctx = probe_server_context(
            "http://127.0.0.1:1/v1",
            "m",
            Wanted::ServedAndTrained,
            std::time::Duration::from_secs(2),
        );
        assert_eq!(ctx, ServerContext { served: None, trained: None, served_from: "/props" });
    }
}
