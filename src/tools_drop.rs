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

/// The session probe's deadline: both requests, connects and body reads
/// included, share this much time from the probe's start. A probe that
/// has not finished by then is dropped and nothing is said. Doctor allows
/// its own pair [`crate::provider::TOOLS_DROP_PROBE_TIMEOUT_SECS`] each;
/// a session start is not the place to wait that long for a server that
/// is slow to prefill the tools, and such a user still has doctor. A
/// ruled value (operator, 2026-11-17).
pub const SESSION_PROBE_DEADLINE_MS: u64 = 5000;

/// How long past the deadline a collector still waits for the probe
/// thread's answer before giving up on it. The thread's own requests end
/// at the deadline; this covers the hand-over.
const DEADLINE_SLACK_MS: u64 = 250;

/// How long an interactive start waits for the probe before it shows the
/// prompt. A probe still running after this is collected before the
/// first chat request instead.
pub const SESSION_PROBE_START_GRACE_MS: u64 = 2000;

/// The one tool definition the SESSION probe sends with its second
/// request (v0.41.0). Doctor's check still sends the session's full list
/// (`doctor::tools_drop_check`); a session start only asks whether the
/// template renders a tools array at all, and the full list made that
/// request about 3,200 prompt tokens, which a CPU-only llama.cpp server
/// decodes in one uncancellable batch: the first chat request then waited
/// behind it past the 60 s head bound (v0.40.0 to v0.41.0). The probe is
/// sent with `max_tokens` 1 and only its usage is read, so this tool is
/// never offered to a model that could call it.
pub const SESSION_PROBE_TOOL_NAME: &str = "echo_text";
/// [`SESSION_PROBE_TOOL_NAME`]'s one-line description.
pub const SESSION_PROBE_TOOL_DESCRIPTION: &str = "Return the given text unchanged.";
/// [`SESSION_PROBE_TOOL_NAME`]'s input schema: one short string parameter.
pub const SESSION_PROBE_TOOL_SCHEMA: &str =
    r#"{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}"#;

/// The session probe's tool list: exactly one definition, built from the
/// constants above.
pub fn session_probe_tools() -> Vec<ToolDef> {
    vec![ToolDef {
        name: SESSION_PROBE_TOOL_NAME.to_string(),
        description: SESSION_PROBE_TOOL_DESCRIPTION.to_string(),
        input_schema: serde_json::from_str(SESSION_PROBE_TOOL_SCHEMA)
            .expect("the session probe schema is valid JSON"),
    }]
}

/// The session's one line, from doctor's WARN for the same finding.
pub fn session_warning(base_url: &str, model: &str, tokens: u64) -> String {
    format!(
        "the server at {base_url} appears to drop tool definitions for \"{model}\" (prompt_tokens {tokens} with and without a tool definition): the chat template has no tool support, so the model cannot call tools in this session; `temur doctor` has the full report"
    )
}

/// One session-start probe, running on its own thread.
///
/// The thread sends the bare request and, only when that answered with a
/// count, the one carrying the tools: without the first count there is
/// nothing to compare the second against. No retry, no stored state. A
/// probe that cannot connect, runs past its deadline, or gets an answer
/// that is not the drop yields nothing.
///
/// The deadline is kept twice. Each request is given what is left of it
/// as its whole-call timeout, so the request ends there and its
/// connection is closed when the client drops it. And [`Watch::take`]
/// stops waiting shortly after the deadline whatever the thread is doing,
/// so a collector is never held by a request that did not honour it.
pub struct Watch {
    rx: Option<std::sync::mpsc::Receiver<Option<String>>>,
    /// When a collector gives up: the deadline plus a little slack.
    give_up: std::time::Instant,
}

impl Watch {
    /// A watch that never has anything to say.
    pub fn idle() -> Self {
        Watch {
            rx: None,
            give_up: std::time::Instant::now(),
        }
    }

    /// Start the probe for `p` when [`wanted`] says so, else [`Watch::idle`].
    /// The request with tools carries [`session_probe_tools`], one minimal
    /// definition, not the session's list.
    pub fn start(p: &ResolvedProfile) -> Self {
        Self::start_with(
            p,
            std::time::Duration::from_millis(SESSION_PROBE_DEADLINE_MS),
        )
    }

    /// [`Watch::start`] with the deadline supplied, so a test can watch
    /// one pass without waiting out the real one.
    pub fn start_with(p: &ResolvedProfile, deadline: std::time::Duration) -> Self {
        if !wanted(p) {
            return Self::idle();
        }
        let defs = session_probe_tools();
        let (tx, rx) = std::sync::mpsc::channel();
        let base_url = p.base_url.clone();
        let model = p.model.clone();
        let ends = std::time::Instant::now() + deadline;
        std::thread::spawn(move || {
            // What is left of the deadline, or `None` once it has passed.
            let left = || {
                let d = ends.saturating_duration_since(std::time::Instant::now());
                (!d.is_zero()).then_some(d)
            };
            let line = (|| {
                let bare =
                    crate::provider::probe_prompt_tokens(&base_url, &model, None, None, left()?);
                if !matches!(bare, ProbeOutcome::Ok(_)) {
                    return None;
                }
                let with_tools = crate::provider::probe_prompt_tokens(
                    &base_url,
                    &model,
                    Some(&defs),
                    None,
                    left()?,
                );
                match verdict(bare, with_tools) {
                    Verdict::Dropped { tokens } => {
                        Some(session_warning(&base_url, &model, tokens))
                    }
                    _ => None,
                }
            })();
            // The receiver may be gone: the session ended first, or the
            // collector gave up at the deadline.
            let _ = tx.send(line);
        });
        Watch {
            rx: Some(rx),
            give_up: ends + std::time::Duration::from_millis(DEADLINE_SLACK_MS),
        }
    }

    /// Whether a probe was started and has not been collected yet.
    pub fn pending(&self) -> bool {
        self.rx.is_some()
    }

    /// The warning, once. `wait` bounds how long to wait for a probe
    /// that is still running; `None` waits for what is left of the
    /// probe's deadline. Either way the wait never runs past the
    /// deadline: a probe that has not answered by then is dropped here
    /// and never heard. After the probe has answered or been dropped,
    /// and on an idle watch, this returns `None` at once.
    pub fn take(&mut self, wait: Option<std::time::Duration>) -> Option<String> {
        let rx = self.rx.as_ref()?;
        let left = self
            .give_up
            .saturating_duration_since(std::time::Instant::now());
        let got = match rx.recv_timeout(wait.map_or(left, |d| d.min(left))) {
            Ok(line) => line,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if std::time::Instant::now() < self.give_up {
                    return None; // the caller's own bound; still collectable
                }
                None
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => None,
        };
        self.rx = None;
        got
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

    /// v0.41.0: the session probe's request with tools carries exactly
    /// one definition, the fixed one, read back from the raw body bytes.
    #[test]
    fn the_session_probe_sends_one_fixed_tool() {
        let tools = session_probe_tools();
        let body = crate::provider::tools_drop_probe_body("m", Some(&tools), None);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let sent = v["tools"].as_array().expect("a tools array");
        assert_eq!(sent.len(), 1, "{body}");
        assert_eq!(sent[0]["function"]["name"], SESSION_PROBE_TOOL_NAME, "{body}");
        assert_eq!(body.matches("\"name\":").count(), 1, "one name on the wire: {body}");
        assert!(body.contains("\"name\":\"echo_text\""), "{body}");
        assert!(!body.contains("\"bash\""), "not the session's list: {body}");
        assert_eq!(v["max_tokens"], 1, "{body}");
        let params = &sent[0]["function"]["parameters"];
        assert_eq!(params["properties"].as_object().unwrap().len(), 1, "{body}");
        assert_eq!(params["properties"]["text"]["type"], "string", "{body}");
        assert!(!SESSION_PROBE_TOOL_DESCRIPTION.contains('\n'));
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
        let mut w = Watch::start(&profile("openai-compat", &base, false));
        assert_eq!(
            w.take(None),
            Some(format!(
                "the server at {base} appears to drop tool definitions for \"m\" (prompt_tokens 10 with and without a tool definition): the chat template has no tool support, so the model cannot call tools in this session; `temur doctor` has the full report"
            ))
        );
        assert_eq!(w.take(None), None, "said once");
        assert_eq!(hits.load(Ordering::SeqCst), 2, "the bare request and the one with tools");
    }

    #[test]
    fn silent_when_the_tools_are_rendered() {
        let (base, hits) = server(10, 31, false);
        let mut w = Watch::start(&profile("openai-compat", &base, false));
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
        let mut w = Watch::start(&profile("openai-compat", &base, false));
        assert_eq!(w.take(None), None);
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    }

    /// The with-tools request never answers. A bounded wait comes back
    /// empty-handed while the probe runs, and the unbounded one ends at
    /// the probe's deadline, with nothing to say.
    #[test]
    fn silent_on_a_timeout_and_never_blocks_past_the_bound() {
        let (base, hits) = server(10, 10, true);
        let started = Instant::now();
        let mut w = Watch::start_with(
            &profile("openai-compat", &base, false),
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
        let mut w = Watch::start(&profile("openai-compat", &base, false));
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
            let mut w = Watch::start(&p);
            assert_eq!(w.take(None), None);
        }
        // Give a request that should not exist time to arrive.
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(hits.load(Ordering::SeqCst), 0, "no request left the process");
        assert!(wanted(&profile("openai-compat", &base, false)));
    }

    /// A server that sends the response head and then stalls the body.
    /// The client's whole-call timeout covers the body read, so the
    /// request ends at the deadline and the watch has nothing to say.
    /// `closed` records that the server saw its connection end.
    #[test]
    fn a_stalled_body_is_dropped_at_the_deadline() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}/v1", listener.local_addr().unwrap().port());
        let closed = Arc::new(AtomicU64::new(0));
        let saw_close = Arc::clone(&closed);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let saw_close = Arc::clone(&saw_close);
                std::thread::spawn(move || {
                    use std::io::{Read, Write};
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf);
                    // The head promises 64 bytes; 9 arrive and no more.
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 64\r\n\r\n{\"usage\":",
                    );
                    let _ = stream.flush();
                    // Read until the client goes away: 0 bytes or an error.
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
                    loop {
                        match stream.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                    }
                    saw_close.fetch_add(1, Ordering::SeqCst);
                });
            }
        });
        let started = Instant::now();
        let mut w = Watch::start_with(
            &profile("openai-compat", &base, false),
            Duration::from_secs(1),
        );
        assert_eq!(w.take(None), None);
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(900), "{waited:?}");
        assert!(waited < Duration::from_secs(3), "{waited:?}");
        assert!(!w.pending());
        // The request ended at the deadline and its connection with it.
        let until = Instant::now() + Duration::from_secs(5);
        while closed.load(Ordering::SeqCst) == 0 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(closed.load(Ordering::SeqCst), 1, "the server saw the connection close");
    }

    /// The two requests share ONE deadline. The bare answer takes 0.6 s
    /// of a 1 s deadline, so the request with the tools has 0.4 s left
    /// and its connection must close about 1 s after the start. With a
    /// bound per request it would stay open until 1.6 s.
    #[test]
    fn the_request_with_the_tools_gets_only_what_is_left_of_the_deadline() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}/v1", listener.local_addr().unwrap().port());
        let closed_at = Arc::new(std::sync::Mutex::new(None::<Instant>));
        let record = Arc::clone(&closed_at);
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let mut buf = [0u8; 65536];
            // First connection: the bare request, answered 0.6 s late.
            let (mut bare, _) = listener.accept().unwrap();
            let _ = bare.read(&mut buf);
            std::thread::sleep(Duration::from_millis(600));
            let body = r#"{"usage":{"prompt_tokens":10}}"#;
            let _ = bare.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
            drop(bare);
            // Second connection: the request with the tools, never
            // answered. Note when the client lets go of it.
            let (mut tools, _) = listener.accept().unwrap();
            let _ = tools.set_read_timeout(Some(Duration::from_secs(20)));
            loop {
                match tools.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            *record.lock().unwrap() = Some(Instant::now());
        });
        let started = Instant::now();
        let mut w = Watch::start_with(
            &profile("openai-compat", &base, false),
            Duration::from_secs(1),
        );
        assert_eq!(w.take(None), None);
        let until = Instant::now() + Duration::from_secs(5);
        while closed_at.lock().unwrap().is_none() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        let closed = closed_at.lock().unwrap().expect("the server saw the connection close");
        let after = closed.duration_since(started);
        assert!(after >= Duration::from_millis(900), "{after:?}");
        assert!(after < Duration::from_millis(1400), "{after:?}");
    }

    /// A collector is released at the deadline even if the probe thread
    /// has not answered: here the thread is given far longer than the
    /// watch, which is how a request that ignored its timeout would look.
    #[test]
    fn a_collector_gives_up_at_the_deadline_whatever_the_thread_does() {
        let (base, _) = server(10, 10, true);
        let mut w = Watch::start_with(
            &profile("openai-compat", &base, false),
            Duration::from_secs(30),
        );
        w.give_up = Instant::now() + Duration::from_millis(300);
        let started = Instant::now();
        assert_eq!(w.take(None), None);
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        assert!(!w.pending(), "dropped, not left for a later collection");
    }
}
