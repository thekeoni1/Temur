//! The tools-drop verdict, shared by `temur doctor` and a session's start
//! (T69 1d).
//!
//! llama.cpp's `--jinja` mode drops the tools array when the model's chat
//! template has no tool support, and says nothing. Doctor has probed for
//! that since T31: the same one-token completion twice, once bare and once
//! with temur's tool definitions, and a comparison of the two prompt-token
//! counts. A session that never ran doctor used to learn nothing, and the
//! all-models dogfood of 2026-11-16 had three models at 0/24 that way.
//!
//! This module holds the one classification of the two answers
//! ([`verdict`]), which doctor words into its report, and the session's
//! use of it ([`Watch`]): the same two requests, sent once at session
//! start on a keyless openai-compat endpoint, off the main thread, with
//! one warning line when the tools are dropped and nothing otherwise.

use crate::config::ResolvedProfile;
use crate::provider::{ProbeOutcome, ToolDef};

/// What the two tools-drop probe answers mean together.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Both answered with the same prompt-token count: the tools array
    /// went nowhere.
    Dropped { tokens: u64 },
    /// Both answered and the counts differ: the template rendered the
    /// tools.
    Rendered { bare: u64, with_tools: u64 },
    /// The bare request answered and the server refused the one carrying
    /// the tools, in its own words.
    Rejected { status: u16, message: String },
    /// The bare request answered and the one carrying the tools never did.
    Unanswered,
    /// Anything else: no usable prompt-token count to compare.
    NoSignal,
}

/// Classify the two probe answers. Pure; doctor's report and the session
/// warning both read this and nothing else.
pub fn verdict(bare: ProbeOutcome, with_tools: ProbeOutcome) -> Verdict {
    match (bare, with_tools) {
        (ProbeOutcome::Ok(a), ProbeOutcome::Ok(b)) if a == b => Verdict::Dropped { tokens: a },
        (ProbeOutcome::Ok(a), ProbeOutcome::Ok(b)) => Verdict::Rendered {
            bare: a,
            with_tools: b,
        },
        (ProbeOutcome::Ok(_), ProbeOutcome::HttpError { status, message }) => {
            Verdict::Rejected { status, message }
        }
        (ProbeOutcome::Ok(_), ProbeOutcome::Unreachable) => Verdict::Unanswered,
        _ => Verdict::NoSignal,
    }
}

/// Where the probe may run: an openai-compat selection with no key file.
/// The same condition doctor's tools-drop check tests, less its
/// `--no-network` flag, which a session does not have.
pub fn wanted(p: &ResolvedProfile) -> bool {
    p.provider == "openai-compat" && p.api_key_file.is_none()
}

/// The bound on ONE of the session probe's two requests, connect
/// included. Doctor allows its own pair
/// [`crate::provider::TOOLS_DROP_PROBE_TIMEOUT_SECS`] each, which is
/// longer than a chat request waits for its response head, so the session
/// takes the chat's bound: a server too slow for this is one the first
/// chat request would time out on as well.
pub const SESSION_PROBE_TIMEOUT_SECS: u64 =
    crate::provider::transport::CHAT_RESPONSE_HEAD_TIMEOUT_SECS;

/// How long an interactive start waits for the probe before it shows the
/// prompt. A probe still running after this is collected before the
/// first chat request instead.
pub const SESSION_PROBE_START_GRACE_MS: u64 = 2000;

/// The session's one line, from doctor's WARN for the same finding.
pub fn session_warning(base_url: &str, model: &str, tokens: u64) -> String {
    format!(
        "the server at {base_url} appears to drop tool definitions for \"{model}\" (prompt_tokens {tokens} with and without temur's tools): the chat template has no tool support, so the model cannot call tools in this session; `temur doctor` has the full report"
    )
}

/// One session-start probe, running on its own thread.
///
/// The thread sends the bare request and, only when that answered with a
/// count, the one carrying the tools: without the first count there is
/// nothing to compare the second against. No retry, no stored state. A
/// probe that cannot connect, times out, or gets an answer that is not
/// the drop yields nothing.
pub struct Watch {
    rx: Option<std::sync::mpsc::Receiver<Option<String>>>,
}

impl Watch {
    /// A watch that never has anything to say.
    pub fn idle() -> Self {
        Watch { rx: None }
    }

    /// Start the probe for `p` when [`wanted`] says so, else [`Watch::idle`].
    /// `defs` are the definitions the session sends.
    pub fn start(p: &ResolvedProfile, defs: Vec<ToolDef>) -> Self {
        Self::start_with(
            p,
            defs,
            std::time::Duration::from_secs(SESSION_PROBE_TIMEOUT_SECS),
        )
    }

    /// [`Watch::start`] with the per-request bound supplied, so a test
    /// can watch a timeout without waiting a minute for it.
    pub fn start_with(
        p: &ResolvedProfile,
        defs: Vec<ToolDef>,
        timeout: std::time::Duration,
    ) -> Self {
        if !wanted(p) {
            return Self::idle();
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let base_url = p.base_url.clone();
        let model = p.model.clone();
        std::thread::spawn(move || {
            let bare =
                crate::provider::probe_prompt_tokens(&base_url, &model, None, None, timeout);
            let line = if matches!(bare, ProbeOutcome::Ok(_)) {
                let with_tools = crate::provider::probe_prompt_tokens(
                    &base_url,
                    &model,
                    Some(&defs),
                    None,
                    timeout,
                );
                match verdict(bare, with_tools) {
                    Verdict::Dropped { tokens } => {
                        Some(session_warning(&base_url, &model, tokens))
                    }
                    _ => None,
                }
            } else {
                None
            };
            // The receiver may be gone (the session ended first).
            let _ = tx.send(line);
        });
        Watch { rx: Some(rx) }
    }

    /// Whether a probe was started and has not been collected yet.
    pub fn pending(&self) -> bool {
        self.rx.is_some()
    }

    /// The warning, once. `wait` bounds how long to wait for a probe
    /// that is still running; `None` waits until it ends, which the
    /// probe's own bounds guarantee. After the probe has answered, and on
    /// an idle watch, this returns `None` at once.
    pub fn take(&mut self, wait: Option<std::time::Duration>) -> Option<String> {
        let rx = self.rx.as_ref()?;
        let got = match wait {
            None => rx.recv().ok(),
            Some(d) => match rx.recv_timeout(d) {
                Ok(line) => Some(line),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return None,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => None,
            },
        };
        self.rx = None;
        got.flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn profile(provider: &str, base_url: &str, keyed: bool) -> ResolvedProfile {
        ResolvedProfile {
            provider: provider.into(),
            model: "m".into(),
            base_url: base_url.into(),
            api_key_file: keyed.then(|| "/nonexistent/key".to_string()),
            max_tokens: 4096,
            context_window: Some(8192),
            prompt_profile: crate::tools::PromptProfile::Compact,
            prompt_profile_source: crate::config::PromptProfileSource::Auto,
            price_input_per_mtok: None,
            price_output_per_mtok: None,
            max_tokens_parameter: None,
        }
    }

    fn defs() -> Vec<ToolDef> {
        crate::tools::Registry::standard()
            .with_profile(crate::tools::PromptProfile::Compact)
            .definitions()
    }

    /// A server that counts connections and answers every POST with a
    /// usage block: `bare` tokens without a tools array, `with_tools`
    /// with one. `stall_tools` holds the with-tools connection open and
    /// never answers it.
    fn server(bare: u64, with_tools: u64, stall_tools: bool) -> (String, Arc<AtomicU64>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicU64::new(0));
        let seen = Arc::clone(&hits);
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                seen.fetch_add(1, Ordering::SeqCst);
                use std::io::{Read, Write};
                let mut req = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            req.extend_from_slice(&buf[..n]);
                            let Some(h) = req.windows(4).position(|w| w == b"\r\n\r\n") else {
                                continue;
                            };
                            let head = String::from_utf8_lossy(&req[..h]).to_ascii_lowercase();
                            let len: usize = head
                                .split("content-length:")
                                .nth(1)
                                .and_then(|s| s.split(['\r', '\n']).next())
                                .and_then(|s| s.trim().parse().ok())
                                .unwrap_or(0);
                            if req.len() >= h + 4 + len {
                                break;
                            }
                        }
                    }
                }
                let text = String::from_utf8_lossy(&req);
                let has_tools = text.contains("\"tools\"");
                if has_tools && stall_tools {
                    held.push(stream);
                    continue;
                }
                let n = if has_tools { with_tools } else { bare };
                let body = format!(r#"{{"usage":{{"prompt_tokens":{n}}}}}"#);
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        (format!("http://127.0.0.1:{port}/v1"), hits)
    }

    #[test]
    fn verdict_covers_doctors_five_arms() {
        use ProbeOutcome as P;
        assert_eq!(verdict(P::Ok(10), P::Ok(10)), Verdict::Dropped { tokens: 10 });
        assert_eq!(
            verdict(P::Ok(10), P::Ok(31)),
            Verdict::Rendered { bare: 10, with_tools: 31 }
        );
        assert_eq!(
            verdict(P::Ok(10), P::HttpError { status: 400, message: "no".into() }),
            Verdict::Rejected { status: 400, message: "no".into() }
        );
        assert_eq!(verdict(P::Ok(10), P::Unreachable), Verdict::Unanswered);
        for (a, b) in [
            (P::Ok(10), P::NoUsage),
            (P::NoUsage, P::Ok(10)),
            (P::Unreachable, P::Ok(10)),
            (P::Unreachable, P::Unreachable),
            (P::HttpError { status: 500, message: "x".into() }, P::Ok(10)),
        ] {
            assert_eq!(verdict(a, b), Verdict::NoSignal);
        }
    }

    #[test]
    fn warns_once_when_the_counts_match() {
        let (base, hits) = server(10, 10, false);
        let mut w = Watch::start(&profile("openai-compat", &base, false), defs());
        assert_eq!(
            w.take(None),
            Some(format!(
                "the server at {base} appears to drop tool definitions for \"m\" (prompt_tokens 10 with and without temur's tools): the chat template has no tool support, so the model cannot call tools in this session; `temur doctor` has the full report"
            ))
        );
        assert_eq!(w.take(None), None, "said once");
        assert_eq!(hits.load(Ordering::SeqCst), 2, "the bare request and the one with tools");
    }

    #[test]
    fn silent_when_the_tools_are_rendered() {
        let (base, hits) = server(10, 31, false);
        let mut w = Watch::start(&profile("openai-compat", &base, false), defs());
        assert_eq!(w.take(None), None);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn silent_and_prompt_on_a_dead_endpoint() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let base = format!("http://127.0.0.1:{port}/v1");
        let started = Instant::now();
        let mut w = Watch::start(&profile("openai-compat", &base, false), defs());
        assert_eq!(w.take(None), None);
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    }

    /// The with-tools request never answers. A bounded wait comes back
    /// empty-handed while the probe runs, and the unbounded one ends at
    /// the probe's own bound, with nothing to say.
    #[test]
    fn silent_on_a_timeout_and_never_blocks_past_the_bound() {
        let (base, hits) = server(10, 10, true);
        let started = Instant::now();
        let mut w = Watch::start_with(
            &profile("openai-compat", &base, false),
            defs(),
            Duration::from_secs(1),
        );
        assert_eq!(w.take(Some(Duration::from_millis(50))), None, "still running");
        assert!(w.pending(), "a bounded wait that ran out leaves the probe collectable");
        assert!(started.elapsed() < Duration::from_millis(900), "{:?}", started.elapsed());
        assert_eq!(w.take(None), None, "timed out: nothing to say");
        assert!(!w.pending());
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(900), "{waited:?}");
        assert!(waited < Duration::from_secs(10), "{waited:?}");
        assert_eq!(hits.load(Ordering::SeqCst), 2, "no retry");
    }

    #[test]
    fn a_bare_request_that_fails_skips_the_one_with_tools() {
        // HTTP 200 with no usage block: nothing to compare against.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}/v1", listener.local_addr().unwrap().port());
        let hits = Arc::new(AtomicU64::new(0));
        let seen = Arc::clone(&hits);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                seen.fetch_add(1, Ordering::SeqCst);
                use std::io::{Read, Write};
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                );
            }
        });
        let mut w = Watch::start(&profile("openai-compat", &base, false), defs());
        assert_eq!(w.take(None), None);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn never_sent_with_a_key_or_on_the_anthropic_transport() {
        let (base, hits) = server(10, 10, false);
        for p in [
            profile("openai-compat", &base, true),
            profile("anthropic", &base, false),
            profile("anthropic", &base, true),
        ] {
            assert!(!wanted(&p), "{p:?}");
            let mut w = Watch::start(&p, defs());
            assert_eq!(w.take(None), None);
        }
        // Give a request that should not exist time to arrive.
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(hits.load(Ordering::SeqCst), 0, "no request left the process");
        assert!(wanted(&profile("openai-compat", &base, false)));
    }
}
