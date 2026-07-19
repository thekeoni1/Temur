//! Transport seam: the one place real HTTPS happens. Tests (and later a
//! replay mode) implement `Transport` over fixture files, so the entire
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
    /// The credential goes into the `x-api-key` header only — it must never
    /// be part of `body`, logs, or error values.
    fn post_stream(
        &self,
        url: &str,
        api_key: &str,
        body: &str,
    ) -> Result<Box<dyn Read>, TransportError>;
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

pub struct HttpTransport {
    agent: ureq::Agent,
}

impl HttpTransport {
    pub fn new() -> Self {
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok(); // idempotent: fine if already installed
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build();
        HttpTransport {
            agent: config.new_agent(),
        }
    }
}

impl Default for HttpTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for HttpTransport {
    fn post_stream(
        &self,
        url: &str,
        api_key: &str,
        body: &str,
    ) -> Result<Box<dyn Read>, TransportError> {
        let res = self
            .agent
            .post(url)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .send(body)
            .map_err(|e| TransportError::Io(e.to_string()))?;

        let status = res.status().as_u16();
        if !(200..300).contains(&status) {
            let retry_after = res
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u64>().ok());
            let mut body_text = String::new();
            let _ = res
                .into_body()
                .into_reader()
                .take(64 * 1024)
                .read_to_string(&mut body_text);
            return Err(TransportError::Status {
                code: status,
                retry_after,
                body: body_text,
            });
        }
        Ok(Box::new(res.into_body().into_reader()))
    }
}
