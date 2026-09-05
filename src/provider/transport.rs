//! Provider-neutral transport seam: the interface real HTTPS goes through,
//! plus the offline replay/capture implementations and the shared retry
//! policy. Each provider owns its own *real* HTTP transport (headers are
//! provider-specific: Anthropic's `x-api-key`, OpenAI-compat's
//! `Authorization: Bearer`); everything here is header-agnostic. Tests
//! implement `Transport` over fixture files, so the entire
//! request→stream→completion path is exercised offline.

use std::io::Read;

#[derive(thiserror::Error, Debug)]
pub enum TransportError {
    /// Non-2xx HTTP response; `body` is the (JSON) error body if readable.
    #[error("http status {code}")]
    Status {
        code: u16,
        retry_after: Option<u64>,
        body: String,
    },
    #[error("io/connect: {0}")]
    Io(String),
    /// A T50 timeout: the request never got past `phase` inside the bound
    /// that phase allows. Separate from [`TransportError::Io`] for one
    /// reason, and it is a behavioral one rather than a cosmetic one: this
    /// is the only transport error whose retryability depends on WHICH
    /// phase timed out. See `retryable` below.
    #[error("timed out: {phase}")]
    Timeout { phase: String, retryable: bool },
}

impl TransportError {
    pub fn retryable(&self) -> bool {
        match self {
            TransportError::Status { code, .. } => {
                matches!(code, 408 | 429) || *code >= 500
            }
            TransportError::Io(_) => true,
            TransportError::Timeout { retryable, .. } => *retryable,
        }
    }

    pub fn retry_after(&self) -> Option<u64> {
        match self {
            TransportError::Status { retry_after, .. } => *retry_after,
            TransportError::Io(_) => None,
            TransportError::Timeout { .. } => None,
        }
    }
}

pub trait Transport {
    /// POST `body` and return the raw (SSE) response body stream.
    /// The credential goes into the implementation's auth header only — it
    /// must never be part of `body`, logs, or error values. An empty
    /// `api_key` means "no credential" (keyless local endpoints): the
    /// implementation sends no auth header at all.
    fn post_stream(
        &self,
        url: &str,
        api_key: &str,
        body: &str,
    ) -> Result<Box<dyn Read>, TransportError>;
}

/// Seconds allowed to ESTABLISH a chat connection: opening the socket plus,
/// on HTTPS, the TLS handshake. Ten seconds is far above any healthy path,
/// including a cold handshake from 32-bit hardware on a slow link, and far
/// below the point where a user decides temur has died.
pub const CHAT_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Seconds allowed between the request being sent and the response STATUS
/// LINE and headers arriving. This is the constant that ends the observed
/// hang: an endpoint that completes the TCP handshake and then never
/// speaks.
///
/// Sixty seconds is generous on purpose. A hosted provider queueing a large
/// prompt, or a local llama.cpp prefilling ~29KB of tool definitions on a
/// CPU-only box, can legitimately take tens of seconds before the first
/// byte of the response head.
///
/// It is wired to `timeout_send_body`, NOT to `timeout_recv_response`, and
/// that choice is the whole design. See [`chat_agent_with`].
pub const CHAT_RESPONSE_HEAD_TIMEOUT_SECS: u64 = 60;

/// Seconds of SILENCE tolerated mid-stream, once the response head has
/// arrived. An IDLE bound, not a total: it resets on every chunk, so a
/// stream that keeps producing is never cut off no matter how long it runs.
///
/// Two minutes is far above any real inter-token gap (a slow local model on
/// this hardware streams in tens of milliseconds per token, and the longest
/// legitimate pause is a provider's own think-time between blocks) and far
/// below "the user has gone to make tea".
pub const CHAT_STREAM_IDLE_TIMEOUT_SECS: u64 = 120;

/// The ONE place both chat transports get their ureq agent.
///
/// Shared rather than copied because T47 paid for that lesson with
/// MD_OPTIONS: two hand-synchronized copies of a config are one edit away
/// from silently disagreeing, and here the disagreement would be a hang on
/// exactly one provider.
///
/// WHICH KNOB BOUNDS WHAT, and why it is not the obvious mapping. ureq 3
/// computes each deadline in `timings.rs::next_timeout` over the current
/// phase plus its PRECEEDING phases. For the phase being awaited right now
/// the deadline is `now + configured`, recomputed on every socket read, so
/// that knob behaves as a per-read IDLE bound. For a preceeding phase it is
/// `that_phase_completed_at + configured`, an absolute deadline that keeps
/// applying into later phases.
///
/// `RecvBody`'s preceeding set is `[RecvResponse]`, so
/// `timeout_recv_response`, the knob whose name says "headers", keeps
/// applying while the BODY is read. Measured, not inferred: a stream whose
/// headers arrived in 2ms and then went quiet died at 60.58s reporting
/// `Timeout(RecvResponse)` (`t50-gates/p1-midstream-probe.log`).
///
/// The precise symptom is subtler than a hard cap, and worth stating
/// because the imprecise version cost a test that proved nothing: once that
/// absolute deadline is in the PAST, `NextTimeout::not_zero` degrades it to
/// a ONE SECOND per-read timeout rather than failing outright. So the naive
/// wiring does not truncate every long stream; it silently drops the
/// tolerable mid-stream silence to about a second once the head bound has
/// elapsed. A model pausing 1.5s to think would end the turn.
/// `t50-gates/p1-skew-probe.log` is that failure, reproduced deliberately.
///
/// `SendBody`'s deadline IS checked while awaiting the response head
/// (`RecvResponse.preceeding()` contains it) and is NOT checked while
/// reading the body. That is exactly the shape wanted, so the response-head
/// bound is wired to `timeout_send_body` and `timeout_recv_response` is
/// left unset.
///
/// `timeout_recv_body` is the current phase during streaming, so it is a
/// true idle bound and cannot cap a long healthy stream.
/// `timeout_global` stays unset: it is a total by construction.
pub fn chat_agent() -> ureq::Agent {
    chat_agent_with(
        std::time::Duration::from_secs(CHAT_CONNECT_TIMEOUT_SECS),
        std::time::Duration::from_secs(CHAT_RESPONSE_HEAD_TIMEOUT_SECS),
        std::time::Duration::from_secs(CHAT_STREAM_IDLE_TIMEOUT_SECS),
    )
}

/// [`chat_agent`] with the three bounds supplied, so tests can drive the
/// real agent against a real socket in about a second instead of minutes.
pub fn chat_agent_with(
    connect: std::time::Duration,
    response_head: std::time::Duration,
    stream_idle: std::time::Duration,
) -> ureq::Agent {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok(); // idempotent: fine if already installed
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(connect))
        // The response-head bound. See the module note above for why this
        // is send_body and not recv_response.
        .timeout_send_body(Some(response_head))
        // The mid-stream idle bound. Reset on every read by construction.
        .timeout_recv_body(Some(stream_idle))
        .build()
        .new_agent()
}

/// Turn a ureq send error into a [`TransportError`], deciding retryability
/// at the one place the timeout PHASE is still known.
///
/// Why the phases differ, since this is the difference between a bounded
/// wait and a multiplied one:
/// - The response-head timeout (`SendBody`, per the wiring above) is NOT
///   retryable. An endpoint that accepted the connection and then said
///   nothing for a full minute is not having a transient blip, and
///   re-POSTing the same body at it buys another full minute for no reason.
///   Retrying would compose to 60 + 2 + 60 + 4 + 60 = 186s of silence,
///   which is the same order as the three-minute hang this milestone exists
///   to remove.
/// - Connect and Resolve stay retryable, matching what a refused connection
///   already does today (`Io` is retryable), because a failure to reach the
///   host genuinely can be transient. Composed worst case is
///   10 + 2 + 10 + 4 + 10 = 36s.
/// - Anything else ureq adds later falls through to the old `Io` behavior
///   rather than silently inheriting a rule written without it in mind
///   (`ureq::Timeout` is `#[non_exhaustive]`).
pub fn classify_send_error(e: ureq::Error) -> TransportError {
    match e {
        ureq::Error::Timeout(reason) => {
            let retryable = matches!(reason, ureq::Timeout::Connect | ureq::Timeout::Resolve);
            TransportError::Timeout {
                phase: reason.to_string(),
                retryable,
            }
        }
        other => TransportError::Io(other.to_string()),
    }
}

/// Extra attempts after the first failure. Shared by all providers.
pub const MAX_RETRIES: u32 = 2;

/// The shared retry policy: retryable transport errors (408/429/5xx, I/O)
/// are re-sent up to [`MAX_RETRIES`] times, honoring `Retry-After` when the
/// server sent one and exponential backoff otherwise.
///
/// `cancel` is polled before each POST and in ≤200 ms slices during backoff,
/// so an interrupt lands promptly even inside a long `Retry-After` wait; the
/// pending transport error is returned as-is (the agent treats any error
/// arriving with the token set as an interruption, not a failure).
pub fn post_stream_with_retries(
    transport: &dyn Transport,
    url: &str,
    api_key: &str,
    body: &str,
    cancel: &crate::cancel::CancelToken,
) -> Result<Box<dyn Read>, TransportError> {
    let mut attempt: u32 = 0;
    loop {
        if cancel.is_set() {
            return Err(TransportError::Io("interrupted by user".into()));
        }
        match transport.post_stream(url, api_key, body) {
            Ok(reader) => return Ok(reader),
            Err(e) => {
                if e.retryable() && attempt < MAX_RETRIES {
                    attempt += 1;
                    let delay = e.retry_after().unwrap_or(1u64 << attempt);
                    log::warn!(
                        "retryable transport error (attempt {attempt}): {e}; retrying in {delay}s"
                    );
                    // u64 millis: second-scale delays would overflow nothing,
                    // but keep byte/time math out of usize on 32-bit anyway.
                    let total_ms: u64 = delay.saturating_mul(1000);
                    let mut slept_ms: u64 = 0;
                    while slept_ms < total_ms {
                        if cancel.is_set() {
                            return Err(e);
                        }
                        let slice_ms = (total_ms - slept_ms).min(200);
                        std::thread::sleep(std::time::Duration::from_millis(slice_ms));
                        slept_ms += slice_ms;
                    }
                    continue;
                }
                return Err(e);
            }
        }
    }
}

/// Offline replay: serves pre-recorded SSE files in order, one per request.
/// Powers `--mock` mode and keeps live-API traffic impossible in test runs.
pub struct ReplayTransport {
    paths: Vec<std::path::PathBuf>,
    next: std::cell::Cell<usize>,
}

impl ReplayTransport {
    pub fn new(paths: Vec<std::path::PathBuf>) -> Self {
        ReplayTransport {
            paths,
            next: std::cell::Cell::new(0),
        }
    }
}

impl Transport for ReplayTransport {
    fn post_stream(
        &self,
        _url: &str,
        _api_key: &str,
        _body: &str,
    ) -> Result<Box<dyn Read>, TransportError> {
        let i = self.next.get();
        let path = self
            .paths
            .get(i)
            .ok_or_else(|| TransportError::Io(format!("mock replay exhausted after {i} responses")))?;
        self.next.set(i + 1);
        let file = std::fs::File::open(path)
            .map_err(|e| TransportError::Io(format!("{}: {e}", path.display())))?;
        Ok(Box::new(file))
    }
}

/// Wraps another transport and tees each response's raw SSE bytes to
/// `<base>.<n>.sse`. SSE bodies carry no credentials (the key exists only in
/// a request header, which is never written), so captures are safe to freeze
/// into `tests/fixtures/live/` as golden conformance fixtures.
pub struct CaptureTransport<T: Transport> {
    inner: T,
    base: std::path::PathBuf,
    counter: std::cell::Cell<u32>,
}

impl<T: Transport> CaptureTransport<T> {
    pub fn new(inner: T, base: std::path::PathBuf) -> Self {
        CaptureTransport {
            inner,
            base,
            counter: std::cell::Cell::new(0),
        }
    }
}

impl<T: Transport> Transport for CaptureTransport<T> {
    fn post_stream(
        &self,
        url: &str,
        api_key: &str,
        body: &str,
    ) -> Result<Box<dyn Read>, TransportError> {
        let reader = self.inner.post_stream(url, api_key, body)?;
        let n = self.counter.get();
        self.counter.set(n + 1);
        let path = self.base.with_extension(format!("{n}.sse"));
        let file = std::fs::File::create(&path)
            .map_err(|e| TransportError::Io(format!("capture file {}: {e}", path.display())))?;
        Ok(Box::new(TeeReader { inner: reader, out: file }))
    }
}

struct TeeReader {
    inner: Box<dyn Read>,
    out: std::fs::File,
}

impl Read for TeeReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            use std::io::Write;
            let _ = self.out.write_all(&buf[..n]);
        }
        Ok(n)
    }
}
