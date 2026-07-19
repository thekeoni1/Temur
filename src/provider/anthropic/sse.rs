//! Incremental SSE parsing over any blocking `BufRead` — the live HTTPS
//! response body (M2) and fixture files (tests) go through the same code.
//!
//! Format handled: `event:` / `data:` / `id:` / `retry:` fields, `:` comment
//! lines, blank-line event termination, multiple `data:` lines joined with
//! '\n'. The event *name* line is ignored — Anthropic repeats the type
//! inside the JSON payload, which is what we dispatch on.

use super::types::SseEvent;
use std::io::BufRead;

#[derive(thiserror::Error, Debug)]
pub enum SseError {
    #[error("sse io: {0}")]
    Io(#[from] std::io::Error),
    #[error("sse json: {0}")]
    Json(String),
}

pub struct SseReader<R: BufRead> {
    reader: R,
    done: bool,
}

impl<R: BufRead> SseReader<R> {
    pub fn new(reader: R) -> Self {
        SseReader {
            reader,
            done: false,
        }
    }
}

impl<R: BufRead> Iterator for SseReader<R> {
    type Item = Result<SseEvent, SseError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let mut data = String::new();
        loop {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => {
                    // EOF: emit a final pending event if the stream didn't
                    // end with a blank line.
                    self.done = true;
                    if data.is_empty() {
                        return None;
                    }
                    return Some(parse_data(&data));
                }
                Ok(_) => {}
                Err(e) => {
                    self.done = true;
                    return Some(Err(e.into()));
                }
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if data.is_empty() {
                    continue; // stray blank line between events
                }
                return Some(parse_data(&data));
            }
            if let Some(rest) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            }
            // "event:", "id:", "retry:", ":" comments — ignored by design.
        }
    }
}

fn parse_data(data: &str) -> Result<SseEvent, SseError> {
    serde_json::from_str::<SseEvent>(data).map_err(|e| {
        let snippet: String = data.chars().take(120).collect();
        SseError::Json(format!("{e} (data: {snippet})"))
    })
}
