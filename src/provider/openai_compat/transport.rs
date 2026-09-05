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
