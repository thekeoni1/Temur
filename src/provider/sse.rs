//! Line-level SSE framing shared by every provider. Handles the wire
//! grammar only — `data:` / `event:` / `id:` / `retry:` fields, `:` comment
//! lines, blank-line event termination, multiple `data:` lines joined with
//! '\n' — and yields each event's raw joined data payload as a `String`.
//! Interpreting the payload is per-provider: Anthropic dispatches on the
//! JSON `type` field; an OpenAI-compatible stream is uniform chunks plus a
//! `data: [DONE]` terminator.
//!
//! The event *name* line is deliberately ignored: Anthropic repeats the
//! type inside the JSON payload, and OpenAI-compatible streams don't use
//! named events at all.

use std::io::BufRead;

/// Iterator over the raw data payloads of an SSE stream.
pub struct SseFrames<R: BufRead> {
    reader: R,
    done: bool,
}

impl<R: BufRead> SseFrames<R> {
    pub fn new(reader: R) -> Self {
        SseFrames {
            reader,
            done: false,
        }
    }
}

impl<R: BufRead> Iterator for SseFrames<R> {
    type Item = Result<String, std::io::Error>;

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
                    return Some(Ok(data));
                }
                Ok(_) => {}
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if data.is_empty() {
                    continue; // stray blank line between events
                }
                return Some(Ok(data));
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
