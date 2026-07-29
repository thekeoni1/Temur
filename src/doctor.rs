//! `temur doctor` (T14): read-only config and environment diagnosis.
//!
//! One PASS/WARN/FAIL line per check; exit SUCCESS iff no FAIL. Strictly
//! read-only: nothing is created, written, or fixed, key files are judged
//! by metadata only (existence, mode, size, mtime) and their contents are
//! never read, and the reachability probes send no HTTP request at all, just a
//! TCP connect plus, for https, a TLS handshake through the same
//! rustls(ring)+webpki-roots stack as tls-probe.

use crate::config::Config;
use std::io::Write;
use std::path::Path;

/// Seconds for connect and handshake I/O; generous for a LAN, short enough
/// that a dead hosted endpoint cannot stall the whole report.
const PROBE_TIMEOUT_SECS: u64 = 5;

struct Report<'a> {
    out: &'a mut dyn Write,
    warns: u32,
    fails: u32,
    passes: u32,
}

impl Report<'_> {
    fn pass(&mut self, msg: &str) -> std::io::Result<()> {
        self.passes += 1;
        writeln!(self.out, "PASS: {msg}")
    }
    fn warn(&mut self, msg: &str) -> std::io::Result<()> {
        self.warns += 1;
        writeln!(self.out, "WARN: {msg}")
    }
    fn fail(&mut self, msg: &str) -> std::io::Result<()> {
        self.fails += 1;
        writeln!(self.out, "FAIL: {msg}")
    }
}

/// Run every check against the config at `cfg_path`. Returns `Ok(true)`
/// iff no check FAILed (WARNs are healthy). `no_network` skips the
/// reachability probes.
pub fn run(
    cfg_path: &Path,
    no_network: bool,
    out: &mut dyn Write,
) -> std::io::Result<bool> {
    // The real sandbox probe (T18): local process spawns only, no network,
    // no writes; still read-only in every sense that matters here.
    run_with_sandbox_probe(cfg_path, no_network, out, &crate::tools::sandbox_available)
}

/// [`run`] with the bash-sandbox availability probe injected, so tests can
/// exercise the unavailable arm on hosts where the real probe passes.
fn run_with_sandbox_probe(
    cfg_path: &Path,
    no_network: bool,
    out: &mut dyn Write,
    sandbox_probe: &dyn Fn() -> bool,
) -> std::io::Result<bool> {
    let mut r = Report {
        out,
        warns: 0,
        fails: 0,
        passes: 0,
    };

    // Config presence, parse, and eager validation: the same accessors
    // startup runs, so doctor and the real thing cannot disagree.
    let cfg: Config = match Config::load_from_reporting(cfg_path) {
        Ok((_, false)) => {
            r.fail(&format!(
                "no config file at {}; run temur init to create a starter config (see README.md, section \"Configure\")",
                cfg_path.display()
            ))?;
            return finish(r);
        }
        Ok((cfg, true)) => {
            r.pass(&format!("config parsed: {}", cfg_path.display()))?;
            cfg
        }
        Err(e) => {
            r.fail(&format!("config unreadable: {e}"))?;
            return finish(r);
        }
    };

    if let Err(e) = cfg.prompt_profile() {
        r.fail(&format!("{e}"))?;
    }
    if let Err(e) = cfg.session_max_bytes() {
        r.fail(&format!("{e}"))?;
    }
    let profiles = match cfg.resolved_profiles() {
        Ok(p) => p,
        Err(e) => {
            r.fail(&format!("{e}"))?;
            return finish(r);
        }
    };
    let active = match cfg.startup_selection(&profiles) {
        Ok((name, resolved)) => {
            r.pass(&format!(
                "active selection: {}provider \"{}\", model \"{}\", {}",
                name.map(|n| format!("profile \"{n}\", ")).unwrap_or_default(),
                resolved.provider,
                resolved.model,
                resolved.base_url
            ))?;
            resolved
        }
        Err(e) => {
            r.fail(&format!("{e}"))?;
            return finish(r);
        }
    };

    // Credentials, active selection first (FAILs are startup blockers),
    // then every named profile's key file (WARN only: an unusable inactive
    // profile does not block startup, but the user should know).
    let rotate_days = cfg.key_rotate_warn_days;
    match (&active.provider[..], &active.api_key_file) {
        (_, Some(path)) => key_file_check(&mut r, "", Path::new(path), true, rotate_days)?,
        ("openai-compat", None) => {
            r.pass("credentials: keyless (no api_key_file configured)")?
        }
        (_, None) => match std::env::var_os("APP_SECRET_FILE") {
            Some(p) => {
                key_file_check(&mut r, "APP_SECRET_FILE ", Path::new(&p), true, rotate_days)?
            }
            None => r.fail(
                "provider \"anthropic\" needs a key: no api_key_file in the config and APP_SECRET_FILE is not set",
            )?,
        },
    }
    for (name, p) in &profiles {
        if let Some(path) = &p.api_key_file {
            if active.api_key_file.as_deref() != Some(path.as_str()) {
                let prefix = format!("profile \"{name}\" ");
                key_file_check(&mut r, &prefix, Path::new(path), false, rotate_days)?;
            }
        }
    }

    // T18: key isolation + bash sandbox status. The guard here is built by
    // the SAME construction rule as startup (KeyGuard::from_selection), so
    // this report cannot disagree with what the session enforces.
    let guard = crate::tools::KeyGuard::from_selection(&active, &profiles);
    if guard.is_empty() {
        r.pass("key isolation: keyless config, no key files to guard")?;
        writeln!(r.out, "NOTE: bash key sandbox: not needed (keyless config)")?;
    } else {
        r.pass(&format!(
            "key isolation: {} key file(s) guarded (tools cannot read them)",
            guard.protected_files().len()
        ))?;
        if sandbox_probe() {
            r.pass("bash key sandbox: available (unprivileged user namespaces)")?;
        } else if cfg.allow_bash_without_key_sandbox {
            r.warn(
                "bash key sandbox: unavailable on this kernel, and allow_bash_without_key_sandbox is true: bash will run WITHOUT the key sandbox (the other tools stay guarded)",
            )?;
        } else {
            r.warn(
                "bash key sandbox: unavailable on this kernel (no unprivileged user namespaces): bash will refuse to run; setting allow_bash_without_key_sandbox to true in config.json accepts running it unsandboxed (the other tools stay guarded)",
            )?;
        }
    }

    // Sessions dir: writable if present, creatable if not. access(2) only,
    // nothing is created.
    let sessions = crate::session_store::sessions_dir(cfg.sessions_dir.as_deref());
    sessions_dir_check(&mut r, &sessions)?;

    // Reachability: one probe per distinct base_url across the active
    // selection and every profile. TCP connect + TLS handshake only; no
    // request of any kind is sent.
    if no_network {
        writeln!(r.out, "SKIP: reachability probes (--no-network)")?;
        writeln!(r.out, "SKIP: model checks (--no-network)")?;
    } else {
        let mut urls: std::collections::BTreeSet<&str> =
            std::collections::BTreeSet::new();
        urls.insert(&active.base_url);
        urls.extend(profiles.values().map(|p| p.base_url.as_str()));
        for url in urls {
            match probe(url) {
                Ok(what) => r.pass(&format!("reachable: {url} ({what})"))?,
                Err(e) => r.fail(&format!("unreachable: {url}: {e}"))?,
            }
        }
        // Model checks (T15): is each configured model actually served?
        // KEYLESS openai-compat selections only, through the ONE listing
        // request doctor may make (list_models_keyless: unauthenticated by
        // construction, short timeout). Keyed selections get a SKIP line;
        // a failed listing is a plain note, never a FAIL, because the
        // probe above already reported connectivity.
        let mut listings: std::collections::BTreeMap<String, Result<Vec<String>, String>> =
            std::collections::BTreeMap::new();
        model_check(&mut r, "", &active, &mut listings)?;
        for (name, p) in &profiles {
            if *p == active {
                continue; // the active profile: already checked above
            }
            let prefix = format!("profile \"{name}\" ");
            model_check(&mut r, &prefix, p, &mut listings)?;
        }
    }

    finish(r)
}

/// How many server ids a model-mismatch WARN prints before folding the
/// rest into a count.
const MODEL_WARN_LIST_CAP: usize = 10;

/// One model check (T15). PASS when the configured model is in the
/// server's listing; WARN (never FAIL) when it is not, naming the model
/// and up to [`MODEL_WARN_LIST_CAP`] served ids, because servers alias
/// ids (Ollama tags, llama.cpp path names) and an exact match is only
/// advisory. Listings are cached per base_url so shared servers are asked
/// once.
fn model_check(
    r: &mut Report<'_>,
    prefix: &str,
    p: &crate::config::ResolvedProfile,
    listings: &mut std::collections::BTreeMap<String, Result<Vec<String>, String>>,
) -> std::io::Result<()> {
    if p.provider != "openai-compat" || p.api_key_file.is_some() {
        return writeln!(
            r.out,
            "SKIP: {prefix}model check would need an authenticated request; skipped"
        );
    }
    let outcome = listings.entry(p.base_url.clone()).or_insert_with(|| {
        crate::provider::list_models_keyless(
            &p.base_url,
            std::time::Duration::from_secs(crate::provider::KEYLESS_LISTING_TIMEOUT_SECS),
        )
        .map_err(|e| e.to_string())
    });
    match outcome {
        Ok(ids) if ids.iter().any(|i| i == &p.model) => r.pass(&format!(
            "{prefix}model \"{}\" is in the server listing at {}",
            p.model, p.base_url
        )),
        Ok(ids) if ids.is_empty() => r.warn(&format!(
            "{prefix}model \"{}\": the server at {} lists no models",
            p.model, p.base_url
        )),
        Ok(ids) => {
            let shown: Vec<&str> = ids
                .iter()
                .take(MODEL_WARN_LIST_CAP)
                .map(String::as_str)
                .collect();
            let more = if ids.len() > MODEL_WARN_LIST_CAP {
                format!(" and {} more", ids.len() - MODEL_WARN_LIST_CAP)
            } else {
                String::new()
            };
            r.warn(&format!(
                "{prefix}model \"{}\" is not in the server listing at {} (server lists: {}{more}; advisory only, servers may alias ids)",
                p.model,
                p.base_url,
                shown.join(", ")
            ))
        }
        Err(e) => writeln!(
            r.out,
            "NOTE: {prefix}model check at {} skipped: {e}",
            p.base_url
        ),
    }
}

fn finish(r: Report<'_>) -> std::io::Result<bool> {
    writeln!(
        r.out,
        "doctor: {} pass, {} warn, {} fail",
        r.passes, r.warns, r.fails
    )?;
    Ok(r.fails == 0)
}

/// Metadata-only inspection: what state is this key file in? Contents are
/// never read.
enum KeyState {
    Missing,
    NotAFile,
    Empty,
    /// Non-empty but group/other bits are set (the mode is carried).
    LooseMode(u32),
    /// Non-empty, tight mode.
    Good(u32),
}

fn inspect_key_file(path: &Path) -> KeyState {
    use std::os::unix::fs::PermissionsExt;
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return KeyState::Missing,
    };
    if !meta.is_file() {
        return KeyState::NotAFile;
    }
    if meta.len() == 0 {
        return KeyState::Empty;
    }
    let mode = meta.permissions().mode() & 0o7777;
    if mode & 0o077 != 0 {
        KeyState::LooseMode(mode)
    } else {
        KeyState::Good(mode)
    }
}

/// Emit one line for a key file. `blocking` (the active selection cannot
/// start without this file) makes problems FAILs; otherwise they are WARNs
/// (an unusable inactive profile does not block startup, but the user
/// should know before a /model switch hits it). A loose mode is advisory
/// either way: startup would still work.
fn key_file_check(
    r: &mut Report<'_>,
    prefix: &str,
    path: &Path,
    blocking: bool,
    rotate_days: u64,
) -> std::io::Result<()> {
    let label = format!("{prefix}key file {}", path.display());
    let (problem, msg) = match inspect_key_file(path) {
        KeyState::Missing => (
            true,
            format!("{label}: missing; create it and paste your key in with your editor"),
        ),
        KeyState::NotAFile => (true, format!("{label}: not a regular file")),
        KeyState::Empty => (
            true,
            format!("{label}: empty (by size); paste your key in with your editor"),
        ),
        KeyState::LooseMode(mode) => {
            r.warn(&format!(
                "{label}: mode {mode:o} allows group/other access; chmod 600 recommended"
            ))?;
            return key_rotation_check(r, prefix, path, rotate_days);
        }
        KeyState::Good(mode) => {
            r.pass(&format!(
                "{label}: present, non-empty (by size), mode {mode:o}"
            ))?;
            return key_rotation_check(r, prefix, path, rotate_days);
        }
    };
    debug_assert!(problem);
    if blocking {
        r.fail(&msg)
    } else {
        r.warn(&msg)
    }
}

/// Rotation reminder (T17 P4): WARN when a present, non-empty key file's
/// mtime is at least `rotate_days` old. mtime is metadata like mode and
/// size; contents are still never read, and the WARN never affects the
/// exit code. 0 disables; a future or unreadable mtime is a silent skip
/// (clock skew is not the user's key hygiene problem).
fn key_rotation_check(
    r: &mut Report<'_>,
    prefix: &str,
    path: &Path,
    rotate_days: u64,
) -> std::io::Result<()> {
    if rotate_days == 0 {
        return Ok(());
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(());
    };
    let Ok(mtime) = meta.modified() else {
        return Ok(());
    };
    let Ok(age) = std::time::SystemTime::now().duration_since(mtime) else {
        return Ok(());
    };
    let days = age.as_secs() / 86_400;
    if days >= rotate_days {
        r.warn(&format!(
            "{prefix}key file {} unchanged for {days} days; consider rotating the key at the provider and pasting the new one (temur init --add re-prompts)",
            path.display()
        ))?;
    }
    Ok(())
}

fn sessions_dir_check(r: &mut Report<'_>, dir: &Path) -> std::io::Result<()> {
    if dir.exists() {
        if !dir.is_dir() {
            return r.fail(&format!(
                "sessions dir {}: exists but is not a directory",
                dir.display()
            ));
        }
        if writable(dir) {
            r.pass(&format!("sessions dir {}: writable", dir.display()))
        } else {
            r.fail(&format!("sessions dir {}: not writable", dir.display()))
        }
    } else {
        // Nearest existing ancestor decides whether the first save's
        // create_dir_all can succeed.
        let mut ancestor = dir.parent();
        while let Some(a) = ancestor {
            if a.as_os_str().is_empty() {
                break;
            }
            if a.exists() {
                return if a.is_dir() && writable(a) {
                    r.pass(&format!(
                        "sessions dir {}: absent, will be created on first save",
                        dir.display()
                    ))
                } else {
                    r.fail(&format!(
                        "sessions dir {}: cannot be created ({} is not a writable directory)",
                        dir.display(),
                        a.display()
                    ))
                };
            }
            ancestor = a.parent();
        }
        r.fail(&format!(
            "sessions dir {}: no existing ancestor to create it under",
            dir.display()
        ))
    }
}

/// access(2) with W_OK: a real permission answer (root squash, ACLs, ro
/// mounts) without creating anything.
fn writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

/// Split a base_url into (https?, host, port). Hand-rolled: two schemes,
/// no dependency.
fn parse_base_url(url: &str) -> Result<(bool, String, u16), String> {
    let (https, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(format!("unsupported scheme in {url:?} (expected http/https)"));
    };
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() {
        return Err(format!("no host in {url:?}"));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
            let port: u16 = p
                .parse()
                .map_err(|_| format!("bad port {p:?} in {url:?}"))?;
            (h.to_string(), port)
        }
        _ => (
            authority.to_string(),
            if https { 443 } else { 80 },
        ),
    };
    if host.is_empty() {
        return Err(format!("no host in {url:?}"));
    }
    Ok((https, host, port))
}

/// TCP connect, plus a full TLS handshake for https. Returns a short
/// description of what was proven. Never sends an HTTP request.
fn probe(url: &str) -> Result<&'static str, String> {
    use std::net::ToSocketAddrs;
    let (https, host, port) = parse_base_url(url)?;
    let addrs: Vec<_> = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| format!("DNS resolution failed: {e}"))?
        .collect();
    let addr = addrs
        .first()
        .ok_or_else(|| "DNS resolved to no addresses".to_string())?;
    let timeout = std::time::Duration::from_secs(PROBE_TIMEOUT_SECS);
    let mut tcp = std::net::TcpStream::connect_timeout(addr, timeout)
        .map_err(|e| format!("TCP connect failed: {e}"))?;
    if !https {
        return Ok("TCP connect");
    }
    tcp.set_read_timeout(Some(timeout)).ok();
    tcp.set_write_timeout(Some(timeout)).ok();
    rustls::crypto::ring::default_provider().install_default().ok();
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let tls_cfg = std::sync::Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );
    let server_name = rustls::pki_types::ServerName::try_from(host.clone())
        .map_err(|e| format!("bad TLS server name {host:?}: {e}"))?;
    let mut conn = rustls::ClientConnection::new(tls_cfg, server_name)
        .map_err(|e| format!("TLS setup failed: {e}"))?;
    while conn.is_handshaking() {
        conn.complete_io(&mut tcp)
            .map_err(|e| format!("TLS handshake failed: {e}"))?;
    }
    Ok("TCP connect + TLS handshake")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_parsing_covers_schemes_ports_and_paths() {
        assert_eq!(
            parse_base_url("http://127.0.0.1:8080/v1").unwrap(),
            (false, "127.0.0.1".into(), 8080)
        );
        assert_eq!(
            parse_base_url("https://api.anthropic.com").unwrap(),
            (true, "api.anthropic.com".into(), 443)
        );
        assert_eq!(
            parse_base_url("https://api.openai.com/v1").unwrap(),
            (true, "api.openai.com".into(), 443)
        );
        assert_eq!(
            parse_base_url("http://10.0.0.2:11434/v1").unwrap(),
            (false, "10.0.0.2".into(), 11434)
        );
        assert!(parse_base_url("ftp://x").is_err());
        assert!(parse_base_url("https://").is_err());
    }

    // --------------------------------------- T15: model checks (hermetic)

    /// Canned HTTP server on 127.0.0.1 that answers every GET with `body`
    /// and tolerates request-less connections (the reachability probe is a
    /// bare TCP connect). The detached accept loop dies with the test
    /// process.
    fn canned_server(body: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                use std::io::{Read, Write};
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            req.extend_from_slice(&buf[..n]);
                            if req.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                    }
                }
                if req.starts_with(b"GET ") {
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                }
            }
        });
        format!("http://127.0.0.1:{port}/v1")
    }

    /// Run doctor over a literal config in a tempdir, capturing output.
    fn doctor_over(config: &str, no_network: bool) -> (bool, String) {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        std::fs::write(&cfg_path, config).unwrap();
        let mut out: Vec<u8> = Vec::new();
        let healthy = run(&cfg_path, no_network, &mut out).unwrap();
        (healthy, String::from_utf8(out).unwrap())
    }

    /// A keyless openai-compat config aimed at `base`.
    fn keyless_config(base: &str, model: &str) -> String {
        format!(
            r#"{{"provider":"openai-compat","openai_compat":{{"base_url":"{base}","model":"{model}"}}}}"#
        )
    }

    #[test]
    fn model_check_pass_when_served() {
        let base = canned_server(r#"{"data":[{"id":"served-a"},{"id":"served-b"}]}"#);
        let (healthy, out) = doctor_over(&keyless_config(&base, "served-b"), false);
        assert!(healthy, "{out}");
        assert!(
            out.contains(&format!("PASS: model \"served-b\" is in the server listing at {base}")),
            "{out}"
        );
    }

    #[test]
    fn model_check_warn_when_absent_names_model_and_ids_but_stays_healthy() {
        let base = canned_server(r#"{"data":[{"id":"real-1"},{"id":"real-2"}]}"#);
        let (healthy, out) = doctor_over(&keyless_config(&base, "bogus-model"), false);
        assert!(healthy, "WARN must not affect the exit code: {out}");
        assert!(out.contains("WARN: model \"bogus-model\" is not in the server listing"), "{out}");
        assert!(out.contains("real-1, real-2"), "{out}");
        assert!(out.contains("advisory"), "{out}");
    }

    #[test]
    fn model_check_bad_json_is_a_note_not_a_fail() {
        let base = canned_server("<html>gateway</html>");
        let (healthy, out) = doctor_over(&keyless_config(&base, "m"), false);
        assert!(healthy, "{out}");
        assert!(out.contains("NOTE: model check at") && out.contains("bad JSON"), "{out}");
    }

    #[test]
    fn model_check_refused_listing_is_a_note_while_the_probe_fails() {
        // Dead port: the reachability probe FAILs (existing behavior), the
        // model check adds only a note.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let base = format!("http://127.0.0.1:{port}/v1");
        let (healthy, out) = doctor_over(&keyless_config(&base, "m"), false);
        assert!(!healthy, "unreachable endpoint is a probe FAIL: {out}");
        assert!(out.contains(&format!("FAIL: unreachable: {base}")), "{out}");
        assert!(out.contains("NOTE: model check at"), "{out}");
        assert!(!out.contains("WARN: model"), "no model WARN without a listing: {out}");
    }

    #[test]
    fn keyed_selection_gets_a_skip_line_and_no_listing_request() {
        // Keyed compat profile: the canned server would answer a GET, so a
        // SKIP line + no PASS/WARN model line proves no request was made.
        let base = canned_server(r#"{"data":[{"id":"x"}]}"#);
        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("k");
        std::fs::write(&key, "value\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        let cfg = format!(
            r#"{{"provider":"openai-compat","openai_compat":{{"base_url":"{base}","model":"m","api_key_file":"{}"}}}}"#,
            key.display()
        );
        let (healthy, out) = doctor_over(&cfg, false);
        assert!(healthy, "{out}");
        assert!(
            out.contains("SKIP: model check would need an authenticated request; skipped"),
            "{out}"
        );
        assert!(
            !out.contains("server listing") && !out.contains("WARN: model"),
            "no model check line for keyed: {out}"
        );
    }

    #[test]
    fn named_keyless_profile_is_checked_with_its_prefix_and_active_dedup_holds() {
        let base = canned_server(r#"{"data":[{"id":"good"}]}"#);
        // Active selection IS profile "loc" (startup profile): exactly one
        // check line for it, unprefixed; sibling profile "other" gets its
        // own prefixed WARN.
        let cfg = format!(
            r#"{{"profiles":{{
                "loc":{{"provider":"openai-compat","base_url":"{base}","model":"good"}},
                "other":{{"provider":"openai-compat","base_url":"{base}","model":"gone"}}}},
                "profile":"loc"}}"#
        );
        let (healthy, out) = doctor_over(&cfg, false);
        assert!(healthy, "{out}");
        assert_eq!(
            out.matches("model \"good\" is in the server listing").count(),
            1,
            "active profile checked exactly once: {out}"
        );
        assert!(
            out.contains("WARN: profile \"other\" model \"gone\" is not in the server listing"),
            "{out}"
        );
    }

    // ------------------------------- T17 P4: key-rotation reminder (mtime)

    /// Pin a file's mtime to an absolute epoch second via touch -d @N.
    fn touch_at(path: &Path, t: std::time::SystemTime) {
        let secs = t
            .duration_since(std::time::UNIX_EPOCH)
            .expect("post-epoch")
            .as_secs();
        let status = std::process::Command::new("touch")
            .arg("-d")
            .arg(format!("@{secs}"))
            .arg(path)
            .status()
            .expect("touch runs");
        assert!(status.success());
    }

    /// A keyed openai-compat config over a non-empty mode-600 key file
    /// whose mtime is `days_off` days in the past (negative = future),
    /// with an optional key_rotate_warn_days field. Runs doctor with
    /// --no-network (rotation is offline).
    fn doctor_over_aged_key(days_off: i64, warn_days_field: Option<u64>) -> (bool, String) {
        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("k");
        std::fs::write(&key, "value\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        let offset = std::time::Duration::from_secs(days_off.unsigned_abs() * 86_400);
        let mtime = if days_off >= 0 {
            std::time::SystemTime::now() - offset
        } else {
            std::time::SystemTime::now() + offset
        };
        touch_at(&key, mtime);
        let field = warn_days_field
            .map(|d| format!(r#""key_rotate_warn_days":{d},"#))
            .unwrap_or_default();
        let cfg = format!(
            r#"{{{field}"provider":"openai-compat","openai_compat":{{"base_url":"http://127.0.0.1:1/v1","model":"m","api_key_file":"{}"}}}}"#,
            key.display()
        );
        let cfg_path = tmp.path().join("config.json");
        std::fs::write(&cfg_path, cfg).unwrap();
        let mut out: Vec<u8> = Vec::new();
        let healthy = run(&cfg_path, true, &mut out).unwrap();
        (healthy, String::from_utf8(out).unwrap())
    }

    #[test]
    fn rotation_warn_on_an_aged_key_file_stays_healthy() {
        let (healthy, out) = doctor_over_aged_key(91, None);
        assert!(healthy, "rotation WARN must not affect the exit code: {out}");
        assert!(
            out.contains("WARN: key file") && out.contains("unchanged for 91 days"),
            "{out}"
        );
        assert!(
            out.contains("rotating the key at the provider")
                && out.contains("temur init --add re-prompts"),
            "{out}"
        );
        // The ordinary presence PASS line is unchanged and still there.
        assert!(out.contains("present, non-empty (by size), mode 600"), "{out}");
    }

    #[test]
    fn rotation_boundary_fresh_files_stay_quiet() {
        // Below the default threshold: no reminder.
        let (healthy, out) = doctor_over_aged_key(89, None);
        assert!(healthy, "{out}");
        assert!(!out.contains("unchanged for"), "{out}");
        // At the threshold (a hair past 90 full days): reminder.
        let (_healthy, out) = doctor_over_aged_key(90, None);
        assert!(out.contains("unchanged for 90 days"), "{out}");
    }

    #[test]
    fn rotation_custom_threshold_and_zero_disables() {
        let (_healthy, out) = doctor_over_aged_key(6, Some(5));
        assert!(out.contains("unchanged for 6 days"), "{out}");
        let (healthy, out) = doctor_over_aged_key(400, Some(0));
        assert!(healthy, "{out}");
        assert!(!out.contains("unchanged for"), "0 disables: {out}");
    }

    #[test]
    fn rotation_future_mtime_is_a_silent_skip() {
        let (healthy, out) = doctor_over_aged_key(-2, None);
        assert!(healthy, "{out}");
        assert!(!out.contains("unchanged for"), "{out}");
    }

    #[test]
    fn no_network_skips_model_checks_too() {
        let (healthy, out) = doctor_over(
            r#"{"provider":"openai-compat","openai_compat":{"model":"m"}}"#,
            true,
        );
        assert!(healthy, "{out}");
        assert!(out.contains("SKIP: model checks (--no-network)"), "{out}");
        assert!(!out.contains("server listing"), "{out}");
    }

    // ------------------------- T18 P4: key isolation + sandbox status lines

    /// Doctor over a literal config with an injected sandbox probe,
    /// --no-network (these checks are offline).
    fn doctor_probed(config: &str, probe_ok: bool) -> (bool, String) {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        std::fs::write(&cfg_path, config).unwrap();
        let mut out: Vec<u8> = Vec::new();
        let healthy =
            run_with_sandbox_probe(&cfg_path, true, &mut out, &move || probe_ok).unwrap();
        (healthy, String::from_utf8(out).unwrap())
    }

    /// A keyed config over a real placeholder key file (mode 600), plus
    /// optional extra top-level fields.
    fn keyed_config(dir: &Path, extra: &str) -> String {
        let key = dir.join("k");
        std::fs::write(&key, "placeholder-not-a-real-key\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        format!(
            r#"{{{extra}"provider":"openai-compat","openai_compat":{{"base_url":"http://127.0.0.1:1/v1","model":"m","api_key_file":"{}"}}}}"#,
            key.display()
        )
    }

    #[test]
    fn keyless_config_reports_no_guard_and_a_sandbox_note() {
        let (healthy, out) = doctor_probed(
            r#"{"provider":"openai-compat","openai_compat":{"model":"m"}}"#,
            false, // probe result must not matter when keyless
        );
        assert!(healthy, "{out}");
        assert!(
            out.contains("PASS: key isolation: keyless config, no key files to guard"),
            "{out}"
        );
        assert!(out.contains("NOTE: bash key sandbox: not needed (keyless config)"), "{out}");
        assert!(!out.contains("WARN: bash key sandbox"), "{out}");
    }

    #[test]
    fn keyed_config_counts_guarded_files_and_passes_with_sandbox() {
        let tmp = tempfile::tempdir().unwrap();
        let (healthy, out) = doctor_probed(&keyed_config(tmp.path(), ""), true);
        assert!(healthy, "{out}");
        assert!(
            out.contains("PASS: key isolation: 1 key file(s) guarded (tools cannot read them)"),
            "{out}"
        );
        assert!(
            out.contains("PASS: bash key sandbox: available (unprivileged user namespaces)"),
            "{out}"
        );
    }

    #[test]
    fn keyed_config_without_sandbox_warns_naming_refusal_and_override() {
        let tmp = tempfile::tempdir().unwrap();
        let (healthy, out) = doctor_probed(&keyed_config(tmp.path(), ""), false);
        assert!(healthy, "WARN must not affect the exit code: {out}");
        assert!(out.contains("WARN: bash key sandbox: unavailable"), "{out}");
        assert!(out.contains("bash will refuse to run"), "{out}");
        assert!(out.contains("allow_bash_without_key_sandbox"), "{out}");
    }

    #[test]
    fn keyed_config_with_override_warns_that_bash_runs_unsandboxed() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = keyed_config(tmp.path(), r#""allow_bash_without_key_sandbox":true,"#);
        let (healthy, out) = doctor_probed(&cfg, false);
        assert!(healthy, "{out}");
        assert!(
            out.contains("WARN: bash key sandbox: unavailable")
                && out.contains("bash will run WITHOUT the key sandbox"),
            "{out}"
        );
        // With a WORKING sandbox the override changes nothing: plain PASS.
        let (_healthy, out) = doctor_probed(&cfg, true);
        assert!(
            out.contains("PASS: bash key sandbox: available (unprivileged user namespaces)"),
            "{out}"
        );
    }

    #[test]
    fn profile_key_files_count_into_the_guard() {
        let tmp = tempfile::tempdir().unwrap();
        let mk = |name: &str| {
            let p = tmp.path().join(name);
            std::fs::write(&p, "placeholder-not-a-real-key\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
            p.display().to_string()
        };
        let (a, b) = (mk("ka"), mk("kb"));
        let cfg = format!(
            r#"{{"profiles":{{
                "one":{{"provider":"openai-compat","model":"m","api_key_file":"{a}"}},
                "two":{{"provider":"openai-compat","model":"m","api_key_file":"{b}"}}}},
                "profile":"one"}}"#
        );
        let (healthy, out) = doctor_probed(&cfg, true);
        assert!(healthy, "{out}");
        assert!(
            out.contains("key isolation: 2 key file(s) guarded"),
            "{out}"
        );
    }
}
