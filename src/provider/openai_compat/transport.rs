//! OpenAI-compat HTTP transport: `Authorization: Bearer` auth, with the
//! header omitted entirely for keyless local endpoints. The neutral
//! `Transport` seam, replay/capture, and retry policy live in
//! `crate::provider::transport`.

pub use crate::provider::transport::{
    CaptureTransport, ReplayTransport, Transport, TransportError,
};

use std::io::Read;

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
        let mut req = self
            .agent
            .post(url)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream");
        if !api_key.is_empty() {
            req = req.header("authorization", &format!("Bearer {api_key}"));
        }
        let res = req
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
