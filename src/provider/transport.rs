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
}

impl TransportError {
    pub fn retryable(&self) -> bool {
        match self {
            TransportError::Status { code, .. } => {
                matches!(code, 408 | 429) || *code >= 500
            }
            TransportError::Io(_) => true,
        }
    }

    pub fn retry_after(&self) -> Option<u64> {
        match self {
            TransportError::Status { retry_after, .. } => *retry_after,
            TransportError::Io(_) => None,
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

/// Extra attempts after the first failure. Shared by all providers.
pub const MAX_RETRIES: u32 = 2;

/// The shared retry policy: retryable transport errors (408/429/5xx, I/O)
/// are re-sent up to [`MAX_RETRIES`] times, honoring `Retry-After` when the
/// server sent one and exponential backoff otherwise.
pub fn post_stream_with_retries(
    transport: &dyn Transport,
    url: &str,
    api_key: &str,
    body: &str,
) -> Result<Box<dyn Read>, TransportError> {
    let mut attempt: u32 = 0;
    loop {
        match transport.post_stream(url, api_key, body) {
            Ok(reader) => return Ok(reader),
            Err(e) => {
                if e.retryable() && attempt < MAX_RETRIES {
                    attempt += 1;
                    let delay = e.retry_after().unwrap_or(1u64 << attempt);
                    log::warn!(
                        "retryable transport error (attempt {attempt}): {e}; retrying in {delay}s"
                    );
                    std::thread::sleep(std::time::Duration::from_secs(delay));
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
