//! Anthropic SSE event parsing: the shared line-level framing
//! (`crate::provider::sse`) plus per-event JSON dispatch on the payload's
//! `type` field — the live HTTPS response body and fixture files go through
//! the same code. The event *name* line is ignored by the framing layer;
//! Anthropic repeats the type inside the JSON payload, which is what we
//! dispatch on.

use super::types::SseEvent;
use crate::provider::sse::SseFrames;
use std::io::BufRead;

#[derive(thiserror::Error, Debug)]
pub enum SseError {
    #[error("sse io: {0}")]
    Io(#[from] std::io::Error),
    #[error("sse json: {0}")]
    Json(String),
}

pub struct SseReader<R: BufRead> {
    frames: SseFrames<R>,
}

impl<R: BufRead> SseReader<R> {
    pub fn new(reader: R) -> Self {
        SseReader {
            frames: SseFrames::new(reader),
        }
    }
}

impl<R: BufRead> Iterator for SseReader<R> {
    type Item = Result<SseEvent, SseError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.frames.next()? {
            Ok(data) => Some(parse_data(&data)),
            Err(e) => Some(Err(e.into())),
        }
    }
}

fn parse_data(data: &str) -> Result<SseEvent, SseError> {
    serde_json::from_str::<SseEvent>(data).map_err(|e| {
        let snippet: String = data.chars().take(120).collect();
        SseError::Json(format!("{e} (data: {snippet})"))
    })
}
