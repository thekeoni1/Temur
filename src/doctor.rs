//! `temur doctor` (T14): read-only config and environment diagnosis.
//!
//! One PASS/WARN/FAIL line per check; exit SUCCESS iff no FAIL. Strictly
//! read-only: nothing is created, written, or fixed, key files are judged
//! by metadata only (existence, mode, size) and their contents are never
//! read, and the reachability probes send no HTTP request at all, just a
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
    match (&active.provider[..], &active.api_key_file) {
        (_, Some(path)) => key_file_check(&mut r, "", Path::new(path), true)?,
        ("openai-compat", None) => {
            r.pass("credentials: keyless (no api_key_file configured)")?
        }
        (_, None) => match std::env::var_os("APP_SECRET_FILE") {
            Some(p) => key_file_check(&mut r, "APP_SECRET_FILE ", Path::new(&p), true)?,
            None => r.fail(
                "provider \"anthropic\" needs a key: no api_key_file in the config and APP_SECRET_FILE is not set",
            )?,
        },
    }
    for (name, p) in &profiles {
        if let Some(path) = &p.api_key_file {
            if active.api_key_file.as_deref() != Some(path.as_str()) {
                let prefix = format!("profile \"{name}\" ");
                key_file_check(&mut r, &prefix, Path::new(path), false)?;
            }
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
    }

    finish(r)
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
            return r.warn(&format!(
                "{label}: mode {mode:o} allows group/other access; chmod 600 recommended"
            ))
        }
        KeyState::Good(mode) => {
            return r.pass(&format!(
                "{label}: present, non-empty (by size), mode {mode:o}"
            ))
        }
    };
    debug_assert!(problem);
    if blocking {
        r.fail(&msg)
    } else {
        r.warn(&msg)
    }
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
}
