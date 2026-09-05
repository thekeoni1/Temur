//! Anthropic HTTP transport. The provider-neutral `Transport` seam, the
//! replay/capture implementations, and the retry policy live in
//! `crate::provider::transport` (re-exported here so pre-T2 import paths
//! keep working); this module owns only the real HTTPS transport with
//! Anthropic's headers.

pub use crate::provider::transport::{
    CaptureTransport, ReplayTransport, Transport, TransportError,
};

use std::io::Read;

pub struct HttpTransport {
    agent: ureq::Agent,
}

impl HttpTransport {
    pub fn new() -> Self {
        HttpTransport {
            agent: crate::provider::transport::chat_agent(),
        }
    }

    /// T50: the same transport with the three bounds supplied, so a test
    /// can drive a real socket without waiting out the production
    /// constants.
    pub fn with_timeouts(
        connect: std::time::Duration,
        response_head: std::time::Duration,
        stream_idle: std::time::Duration,
    ) -> Self {
        HttpTransport {
            agent: crate::provider::transport::chat_agent_with(
                connect,
                response_head,
                stream_idle,
            ),
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
            .map_err(crate::provider::transport::classify_send_error)?;

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
