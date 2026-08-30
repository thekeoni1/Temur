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
    let current_exe = std::env::current_exe().ok();
    let path_var = std::env::var_os("PATH");
    run_with_sandbox_probe(
        cfg_path,
        no_network,
        out,
        &crate::tools::sandbox_available,
        &InstallProbe {
            current_exe: current_exe.as_deref(),
            path_var: path_var.as_deref(),
        },
    )
}

/// What the T13 F4 install check compares: the running binary's own path
/// and the PATH it searches for an installed `temur`. Injected rather than
/// read from the environment inside the check, so tests can stage both
/// sides in a temp dir instead of depending on where the host happens to
/// keep its binaries. Either field absent means "nothing to compare".
struct InstallProbe<'a> {
    current_exe: Option<&'a Path>,
    path_var: Option<&'a std::ffi::OsStr>,
}

/// [`run`] with the bash-sandbox availability probe and the install-skew
/// inputs injected, so tests can exercise the unavailable arm on hosts
/// where the real probe passes, and stage a fake install on a fake PATH.
fn run_with_sandbox_probe(
    cfg_path: &Path,
    no_network: bool,
    out: &mut dyn Write,
    sandbox_probe: &dyn Fn() -> bool,
    install: &InstallProbe<'_>,
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

    if let Err(e) = cfg.prompt_profile_spec() {
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
                "bash key sandbox: unavailable on this kernel (no unprivileged user namespaces): an interactive session will ask per-command approval before running bash unsandboxed; non-interactive runs refuse. Setting allow_bash_without_key_sandbox to true in config.json accepts running it unsandboxed without asking (the other tools stay guarded; see README.md, section \"Untrusted hosts\")",
            )?;
        }
    }

    // Install skew (T13 F4). Offline and metadata/bytes only, so it sits
    // here with the other local checks, before anything that can touch a
    // network.
    install_check(&mut r, install)?;

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

    // Context-window checks (T22). The llama.cpp /props probe is the
    // SECOND keyless request doctor may make (same amendment contract as
    // the model listing: unauthenticated by construction, short timeout,
    // keyless openai-compat profiles only, never under --no-network).
    // Independently of the probe, a profile with NO context_window gets a
    // one-line NOTE: the T20 context advisory and the T19 tool-output
    // scaling are off without it, whatever the provider. Probes are
    // cached per base_url like the listings.
    let mut props: std::collections::BTreeMap<String, Option<u64>> =
        std::collections::BTreeMap::new();
    context_check(&mut r, "", &active, no_network, &mut props)?;
    for (name, p) in &profiles {
        if *p == active {
            continue; // the active profile: already checked above
        }
        let prefix = format!("profile \"{name}\" ");
        context_check(&mut r, &prefix, p, no_network, &mut props)?;
    }

    // Prompt floor (T41). The ACTIVE selection only, same reason as the
    // tools-drop probe below: the offline half is cheap, but the measured
    // half POSTs, and one POST per configured profile is more than a
    // report should cost.
    prompt_floor_check(&mut r, &cfg, &active, no_network)?;

    // Tools-drop probe (T31). The ACTIVE selection only, because unlike
    // the checks above this one POSTs, and two requests per configured
    // profile is more than a report should cost.
    tools_drop_check(&mut r, &cfg, &active, no_network)?;

    finish(r)
}

/// One context-window check (T22). Four outcomes when the /props probe
/// answered: PASS on an exact match; WARN naming both values and the
/// consequence direction on a mismatch (configured larger than the server
/// allocation means the advisory fires too late and requests can fail at
/// the real limit; smaller is safe but early); WARN suggesting the exact
/// config line when context_window is unset. No probe answer (keyed
/// profile, --no-network, or a non-llama.cpp server, all normal): a NOTE
/// when context_window is unset, silence when it is set. NOTEs and WARNs
/// never affect the exit code.
fn context_check(
    r: &mut Report<'_>,
    prefix: &str,
    p: &crate::config::ResolvedProfile,
    no_network: bool,
    props: &mut std::collections::BTreeMap<String, Option<u64>>,
) -> std::io::Result<()> {
    let probed = if !no_network && p.provider == "openai-compat" && p.api_key_file.is_none() {
        *props.entry(p.base_url.clone()).or_insert_with(|| {
            crate::provider::probe_props_context(
                &p.base_url,
                std::time::Duration::from_secs(crate::provider::KEYLESS_LISTING_TIMEOUT_SECS),
            )
        })
    } else {
        None
    };
    match (probed, p.context_window) {
        (Some(n), Some(c)) if c == n => r.pass(&format!(
            "{prefix}context_window {c} matches the server context allocation (n_ctx {n}) at {}",
            p.base_url
        )),
        (Some(n), Some(c)) if c > n => r.warn(&format!(
            "{prefix}context_window {c} is larger than the server context allocation (n_ctx {n}) at {}: the context advisory fires too late and requests can fail at the real limit",
            p.base_url
        )),
        (Some(n), Some(c)) => r.warn(&format!(
            "{prefix}context_window {c} is smaller than the server context allocation (n_ctx {n}) at {}: safe, but the advisory fires earlier than it needs to",
            p.base_url
        )),
        (Some(n), None) => r.warn(&format!(
            "{prefix}no context_window configured; the server at {} allocates n_ctx {n}: add \"context_window\": {n} to the profile",
            p.base_url
        )),
        (None, None) => writeln!(
            r.out,
            "NOTE: {prefix}no context_window configured: the context usage advisory and context-scaled tool-output caps are off for this profile"
        ),
        (None, Some(_)) => Ok(()),
    }
}

/// The tool definitions a real session would send, for the tools-drop
/// probe (T34). Construction mirrors startup exactly: the same skill-dir
/// resolution (env override, else config, else the built-in defaults) and
/// the selection's own resolved prompt profile, because the profile
/// decides which description set goes on the wire and therefore what the
/// server has to render.
///
/// Skill directories affect only what the `skill` tool can later LOAD, not
/// its schema, so a machine with no skills installed still probes with the
/// same definitions as one that has them.
fn session_tool_definitions(
    cfg: &Config,
    p: &crate::config::ResolvedProfile,
) -> Vec<crate::provider::ToolDef> {
    let skill_override = std::env::var("TEMUR_SKILLS_DIR")
        .or_else(|_| std::env::var("OPENCODE_SKILLS_DIR"))
        .ok()
        .or_else(|| cfg.skills_dir.clone());
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let skill_dirs =
        crate::skills::skill_dirs(skill_override.as_deref(), &cwd, home.as_deref());
    crate::tools::Registry::standard_with_skills(skill_dirs)
        .with_profile(p.prompt_profile)
        .definitions()
}

/// The system prompt a session would send for this selection, assembled
/// exactly as main's `rebuild_system` closure assembles it: the config
/// override wins in either profile, `{cwd}` is the real working
/// directory, and the skills section is appended. Doctor cannot call that
/// closure (it lives in the binary, over locals main owns), so it rebuilds
/// from the same three ingredients; [`crate::prompt`] holds the templates
/// so at least the text itself cannot drift between the two.
fn session_system_prompt(cfg: &Config, p: &crate::config::ResolvedProfile) -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let base = cfg.system_prompt.clone().unwrap_or_else(|| {
        crate::prompt::system_prompt_template(p.prompt_profile)
            .replace("{cwd}", &cwd.display().to_string())
    });
    let skill_override = std::env::var("TEMUR_SKILLS_DIR")
        .or_else(|_| std::env::var("OPENCODE_SKILLS_DIR"))
        .ok()
        .or_else(|| cfg.skills_dir.clone());
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let skill_dirs =
        crate::skills::skill_dirs(skill_override.as_deref(), &cwd, home.as_deref());
    match crate::skills::system_prompt_section(&crate::skills::enumerate(&skill_dirs)) {
        Some(section) => format!("{base}{section}"),
        None => base,
    }
}

/// Fraction of the context window the prompt floor may occupy before the
/// floor check WARNs. 40% is where F6's measurements stop being a tax and
/// start being the budget: the full profile's 6,991 tokens are 57% of a
/// 12,288-token window, which is the configuration desktop experiments 3
/// and 4 watched exhaust itself.
const PROMPT_FLOOR_WARN_PERCENT: u64 = 40;

/// Bytes-to-tokens divisor for the offline estimate. Deliberately crude
/// and deliberately named: see [`PROMPT_FLOOR_ESTIMATE_NOTE`] for what it
/// gets wrong and by how much.
const PROMPT_FLOOR_CHARS_PER_TOKEN: u64 = 4;

/// The offline half of the floor, in one place so the check and the tie
/// test that guards [`crate::config::PROMPT_AUTO_COMPACT_BELOW`] cannot
/// measure it differently: system-prompt bytes plus definition bytes
/// (description text and the serialized schema, which is what goes on the
/// wire), over [`PROMPT_FLOOR_CHARS_PER_TOKEN`].
fn floor_estimate(system: &str, defs: &[crate::provider::ToolDef]) -> u64 {
    let tool_bytes: u64 = defs
        .iter()
        .map(|d| {
            d.description.len() as u64
                + serde_json::to_string(&d.input_schema)
                    .map(|s| s.len() as u64)
                    .unwrap_or(0)
        })
        .sum();
    (system.len() as u64 + tool_bytes).div_ceil(PROMPT_FLOOR_CHARS_PER_TOKEN)
}

/// What the estimate is worth, printed only when an estimate is the
/// number being reported.
///
/// The note deliberately does NOT quote an error percentage or a
/// direction. The only calibration this project has is F6 (RUNBOOK,
/// 2026-08-29, llama.cpp `server-b10438`, Qwen3-4B-Instruct-2507,
/// `context_window` 12288): 6,991 counted tokens for the full profile and
/// 2,763 for the compact one. Against those, this estimator reads 7,240
/// for the full profile in this repository's checkout, i.e. 4% HIGH, not
/// low: the two runs weigh different cwd paths and different installed
/// skills, so the sign of the gap is not a property of chars/4. Anyone
/// who wants the real number can have it, from the measured line, for one
/// request.
const PROMPT_FLOOR_ESTIMATE_NOTE: &str =
    "NOTE: that estimate is prompt bytes divided by 4, which is not tokenization: expect it to be off by some percent in either direction. A networked run against a keyless openai-compat server reports a measured figure instead. Reference measurement (2026-08-29, llama.cpp, Qwen3-4B-Instruct-2507): 6,991 tokens for the full profile, 2,763 for the compact one.";

/// What moves the number, printed with every floor line, measured or not.
/// Both ingredients are things the reader can change and neither is
/// obvious from the number alone.
const PROMPT_FLOOR_INPUTS_NOTE: &str =
    "NOTE: the prompt floor moves with the length of the cwd path and the number of installed skills, both of which ride in the system prompt";

/// The prompt floor (T41): how much of the context window is spent before
/// the user's first word.
///
/// T40 finding F6 measured temur's own floor at 6,991 tokens on the full
/// profile and 2,763 on the compact one. On a 12,288-token local server
/// that is 57% of the window gone before the task starts, and desktop
/// experiments 3 and 4 found context exhaustion to be the dominant
/// Terminal-Bench failure mode at those sizes. The number is knowable
/// offline and cheap to measure exactly, so doctor reports it.
///
/// Two halves. The ESTIMATE always runs: system prompt bytes plus tool
/// description and schema bytes, divided by
/// [`PROMPT_FLOOR_CHARS_PER_TOKEN`]. It is honest about being an estimate
/// because chars/4 is not tokenization. The MEASURED half runs under the
/// same gate as the tools-drop probe (keyless openai-compat, network on)
/// and asks the server that will actually serve this session: one POST,
/// one generated token, carrying the real system prompt and the real
/// definitions. When it answers, its number is the one judged.
///
/// Reports the ACTIVE selection only, and never FAILs: a floor that is
/// too large is a configuration to change, not a broken install.
fn prompt_floor_check(
    r: &mut Report<'_>,
    cfg: &Config,
    p: &crate::config::ResolvedProfile,
    no_network: bool,
) -> std::io::Result<()> {
    use crate::provider::ProbeOutcome;
    let system = session_system_prompt(cfg, p);
    let defs = session_tool_definitions(cfg, p);
    let estimate = floor_estimate(&system, &defs);

    // The measured half, under the tools-drop gate. Anything short of a
    // usable count (keyed selection, --no-network, a server that refuses
    // or reports no usage) falls back to the estimate, which is why the
    // estimate is computed first and unconditionally.
    let probe_gate = !no_network && p.provider == "openai-compat" && p.api_key_file.is_none();
    let measured = if probe_gate {
        match crate::provider::probe_prompt_tokens(
            &p.base_url,
            &p.model,
            Some(&defs),
            Some(&system),
            std::time::Duration::from_secs(crate::provider::TOOLS_DROP_PROBE_TIMEOUT_SECS),
        ) {
            ProbeOutcome::Ok(n) => Some(n),
            _ => None,
        }
    } else {
        None
    };
    if no_network {
        writeln!(r.out, "SKIP: prompt floor measurement (--no-network)")?;
    }

    let (kind, tokens, tilde) = match measured {
        Some(n) => ("measured", n, ""),
        None => ("estimate", estimate, "~"),
    };
    let Some(window) = p.context_window else {
        // Nothing to compare against: report the number and say why there
        // is no verdict, rather than inventing a window to divide by.
        writeln!(
            r.out,
            "NOTE: prompt floor ({kind}): {tilde}{tokens} tokens; no context_window is configured, so there is nothing to compare it against"
        )?;
        return floor_notes(r, measured.is_none());
    };
    let percent = tokens.saturating_mul(100) / window.max(1);
    let line = format!(
        "prompt floor ({kind}): {tilde}{tokens} tokens; window {window}; {percent}% of the window"
    );
    if percent < PROMPT_FLOOR_WARN_PERCENT {
        r.pass(&line)?;
    } else if p.prompt_profile == crate::tools::PromptProfile::Compact {
        // Already on the small prompts and still over the line: the knob
        // this check usually points at is spent, so point at the other one.
        r.warn(&format!(
            "{line} is spent before the task starts, and the compact profile is ALREADY active: raise context_window, or serve a model with a larger window"
        ))?;
    } else {
        r.warn(&format!(
            "{line} is spent before the task starts; set prompt_profile to \"compact\" or raise context_window"
        ))?;
    }
    floor_notes(r, measured.is_none())
}

/// The trailing notes for one floor line: what moves the number, always,
/// and what the estimate gets wrong, only when the number IS an estimate.
fn floor_notes(r: &mut Report<'_>, estimated: bool) -> std::io::Result<()> {
    if estimated {
        writeln!(r.out, "{PROMPT_FLOOR_ESTIMATE_NOTE}")?;
    }
    writeln!(r.out, "{PROMPT_FLOOR_INPUTS_NOTE}")
}

/// The tools-drop probe (T31, widened by T34). llama.cpp's `--jinja` mode
/// SILENTLY drops the tools array when the model's chat template has no
/// tool support: the request returns HTTP 200, the server logs nothing, and
/// the response carries no signal at all, so every tool call the agent
/// could make simply never happens. Confirmed on b10423-a94d563ed on
/// 2026-08-14: gemma-3-4b 10/10, Phi-4-mini 4/4 and SmolLM2 31/31
/// prompt_tokens with and without a tools array, against a Qwen3-4B control
/// that moved. Tracked upstream at ggml-org/llama.cpp#27129. This probe
/// reproduced those three counts live on 2026-08-15 across ten served
/// models (T32 P2).
///
/// temur can see what the server will not say: send the same tiny
/// completion twice, once bare and once with tools, and compare the
/// prompt-token counts. A template that rendered the tools MUST cost more
/// prompt tokens; identical counts mean the array went nowhere.
///
/// T34 sends temur's REAL tool definitions, not one synthetic minimal
/// tool. The synthetic version answered the wrong question: on 2026-08-17
/// a Hermes-2-Pro template probed PASS (221 -> 290 prompt_tokens) and then
/// HTTP 400d on every real request, because rendering temur's actual
/// schemas threw inside the template and the toy schema never did. A probe
/// that carries what a turn carries catches that class immediately, which
/// is the third arm below.
///
/// Costs two requests of ~1 generated token each, and only ever runs for
/// the ACTIVE selection, on a keyless openai-compat endpoint, with network
/// checks enabled. Never FAILs: a degraded server is a known world, and
/// doctor stays honest but calm.
fn tools_drop_check(
    r: &mut Report<'_>,
    cfg: &Config,
    p: &crate::config::ResolvedProfile,
    no_network: bool,
) -> std::io::Result<()> {
    use crate::provider::ProbeOutcome;
    if no_network || p.provider != "openai-compat" || p.api_key_file.is_some() {
        return Ok(());
    }
    let timeout =
        std::time::Duration::from_secs(crate::provider::TOOLS_DROP_PROBE_TIMEOUT_SECS);
    let defs = session_tool_definitions(cfg, p);
    // The second request makes the server prefill every tool definition, so
    // on a CPU-only local server this check is the slow one (measured 106s,
    // 2026-08-18). Say so before going quiet for it, rather than after.
    let bytes: usize = defs.iter().map(|d| d.description.len()).sum();
    writeln!(
        r.out,
        "NOTE: tools-drop probe: sending {} tool definitions (~{}KB) to {}; a local server must prefill all of it, which can take minutes the first time",
        defs.len(),
        bytes / 1024,
        p.base_url
    )?;
    let _ = r.out.flush();
    let bare = crate::provider::probe_prompt_tokens(&p.base_url, &p.model, None, None, timeout);
    let with_tools =
        crate::provider::probe_prompt_tokens(&p.base_url, &p.model, Some(&defs), None, timeout);
    match (bare, with_tools) {
        (ProbeOutcome::Ok(a), ProbeOutcome::Ok(b)) if a == b => r.warn(&format!(
            "the server at {} appears to drop tool definitions for \"{}\" (prompt_tokens {a} with and without temur's tools): the chat template has no tool support, so tool calls can silently never happen",
            p.base_url, p.model
        )),
        (ProbeOutcome::Ok(a), ProbeOutcome::Ok(b)) => r.pass(&format!(
            "the server at {} renders temur's tool definitions for \"{}\" (prompt_tokens {a} without tools, {b} with)",
            p.base_url, p.model
        )),
        // The Hermes catch: the server is alive and answers a bare
        // completion, and rejects the request the moment temur's real tool
        // definitions are attached. Every turn this session makes will fail
        // exactly here, so the server's own words are worth repeating.
        (ProbeOutcome::Ok(_), ProbeOutcome::HttpError { status, message }) => r.warn(&format!(
            "the server at {} rejected temur's tool definitions for \"{}\" (HTTP {status}: {message}): every turn that sends tools will fail the same way",
            p.base_url, p.model
        )),
        // A bare request that answered and a with-tools request that never
        // did is almost always the prefill above running past the bound,
        // not a dead server: say which, so nobody reads it as "no signal".
        (ProbeOutcome::Ok(_), ProbeOutcome::Unreachable) => writeln!(
            r.out,
            "NOTE: tools-drop probe at {} inconclusive: the server answered a bare completion but not the one carrying temur's tools within {}s (a slow local server may need longer to prefill them)",
            p.base_url,
            crate::provider::TOOLS_DROP_PROBE_TIMEOUT_SECS
        ),
        _ => writeln!(
            r.out,
            "NOTE: tools-drop probe at {} skipped: the server reported no usable prompt_tokens",
            p.base_url
        ),
    }
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

/// Install skew (T13 F4): is the `temur` on PATH the binary that is
/// running? A stale install is a real source of confusion, since the
/// operator rebuilds, then runs a months-old copy from `~/.local/bin` and
/// sees bugs that were already fixed.
///
/// Metadata and BYTES only. This check never executes what it finds:
/// running a binary discovered on PATH is precisely what a diagnostic
/// tool must not do, so the version on the other side is inferred from
/// its contents, never asked for. Never a FAIL (a second copy is a
/// legitimate setup), and independent of `--no-network`.
///
/// Silence when there is nothing to compare: no `temur` on PATH, no
/// readable `current_exe`, or files that cannot be read.
fn install_check(r: &mut Report<'_>, probe: &InstallProbe<'_>) -> std::io::Result<()> {
    let (Some(current), Some(path_var)) = (probe.current_exe, probe.path_var) else {
        return Ok(());
    };
    // First hit wins: that is the one the shell would run.
    let Some(found) = std::env::split_paths(path_var)
        .map(|dir| dir.join("temur"))
        .find(|c| c.is_file())
    else {
        return Ok(());
    };
    let version = env!("CARGO_PKG_VERSION");
    // The same file reached by two spellings (symlink, ., relative path)
    // is not skew.
    if let (Ok(a), Ok(b)) = (
        std::fs::canonicalize(&found),
        std::fs::canonicalize(current),
    ) {
        if a == b {
            return r.pass(&format!(
                "install: the temur on PATH ({}) is this running binary (temur {version})",
                found.display()
            ));
        }
    }
    match same_bytes(&found, current) {
        None => Ok(()),
        Some(true) => r.pass(&format!(
            "install: the temur on PATH ({}) is a byte-identical copy of this running binary (temur {version})",
            found.display()
        )),
        Some(false) => {
            let (fm, cm) = (mtime_of(&found), mtime_of(current));
            let advice = match (fm, cm) {
                (Some(f), Some(c)) if c > f => "the PATH copy is the older one, so \"temur\" in a shell is a stale install: reinstall it with scripts/install.sh (which installs to ~/.local/bin)",
                (Some(f), Some(c)) if f > c => "the PATH copy is the newer one, so this session is running an older build: rebuild, or run the copy on PATH",
                _ => "reinstall with scripts/install.sh (which installs to ~/.local/bin), or rebuild, so the two agree",
            };
            r.warn(&format!(
                "install: the temur on PATH ({}) is a DIFFERENT build from the one running ({}); PATH copy modified {}, running binary modified {}: {advice}",
                found.display(),
                current.display(),
                age_phrase(fm),
                age_phrase(cm)
            ))
        }
    }
}

/// Whether two files hold identical bytes. Sizes are compared first, so
/// the common answer costs two stats and no read. `None` means one of
/// them could not be read, which is not a finding about either.
fn same_bytes(a: &Path, b: &Path) -> Option<bool> {
    let (ma, mb) = (std::fs::metadata(a).ok()?, std::fs::metadata(b).ok()?);
    // u64 deliberately (these are file sizes, not in-memory extents).
    let (la, lb): (u64, u64) = (ma.len(), mb.len());
    if la != lb {
        return Some(false);
    }
    Some(std::fs::read(a).ok()? == std::fs::read(b).ok()?)
}

fn mtime_of(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}

/// Age in whole days, matching how the key-rotation reminder speaks. A
/// future or unreadable mtime is said plainly rather than guessed at.
fn age_phrase(t: Option<std::time::SystemTime>) -> String {
    match t.and_then(|t| std::time::SystemTime::now().duration_since(t).ok()) {
        Some(d) => format!("{} day(s) ago", d.as_secs() / 86_400),
        None => "at an unknown time".to_string(),
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

    /// Path-aware sibling of [`canned_server`] for the T22 context checks:
    /// `GET /props` answers `props_body`, every other GET answers `body`.
    fn canned_server_with_props(body: &'static str, props_body: &'static str) -> String {
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
                let head = String::from_utf8_lossy(&req);
                if head.starts_with("GET ") {
                    let picked = if head.starts_with("GET /props ") {
                        props_body
                    } else {
                        body
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{picked}",
                        picked.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                }
            }
        });
        format!("http://127.0.0.1:{port}/v1")
    }

    /// Path- and method-aware sibling for the T31 tools-drop probe: POST
    /// answers `with_tools` when the request body carries a tools array and
    /// `bare` when it does not, every GET answers `models_body`. Reads the
    /// whole declared body, since the tools array is the thing under test.
    fn canned_server_with_completions(
        models_body: &'static str,
        bare: &'static str,
        with_tools: &'static str,
    ) -> String {
        canned_server_with_completions_ex(models_body, bare, "HTTP/1.1 200 OK", with_tools).0
    }

    /// [`canned_server_with_completions`] with the two things the T34 arms
    /// need: a settable status line for the WITH-TOOLS response (the Hermes
    /// case is a 400 on that request alone, while the bare one succeeds),
    /// and a capture of every POST body, so a test can assert what temur
    /// actually put on the wire rather than only what came back.
    ///
    /// An EMPTY `with_tools_status` drops that connection without
    /// answering, which is what a probe running past its bound looks like
    /// from this side, without a test having to wait for the real one.
    fn canned_server_with_completions_ex(
        models_body: &'static str,
        bare: &'static str,
        with_tools_status: &'static str,
        with_tools: &'static str,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let posts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = std::sync::Arc::clone(&posts);
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
                            let Some(h) = req.windows(4).position(|w| w == b"\r\n\r\n") else {
                                continue;
                            };
                            let head =
                                String::from_utf8_lossy(&req[..h]).to_ascii_lowercase();
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
                let text = String::from_utf8_lossy(&req).into_owned();
                if text.is_empty() {
                    continue; // the reachability probe: a bare TCP connect
                }
                let (status, picked) = if text.starts_with("POST ") {
                    seen.lock().unwrap().push(text.clone());
                    if text.contains("\"tools\"") {
                        if with_tools_status.is_empty() {
                            continue; // answer nothing: the stream drops here
                        }
                        (with_tools_status, with_tools)
                    } else {
                        ("HTTP/1.1 200 OK", bare)
                    }
                } else {
                    ("HTTP/1.1 200 OK", models_body)
                };
                let response = format!(
                    "{status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{picked}",
                    picked.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (format!("http://127.0.0.1:{port}/v1"), posts)
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

    // ------------------------------------ T22: context-window checks (/props)

    /// A keyless openai-compat config with a context_window.
    fn keyless_config_with_window(base: &str, model: &str, window: u64) -> String {
        format!(
            r#"{{"provider":"openai-compat","openai_compat":{{"base_url":"{base}","model":"{model}","context_window":{window}}}}}"#
        )
    }

    const LLAMA_MODELS: &str = r#"{"data":[{"id":"served"}]}"#;
    const LLAMA_PROPS_8192: &str =
        r#"{"default_generation_settings":{"n_ctx":8192},"total_slots":1}"#;

    #[test]
    fn context_check_pass_on_exact_match() {
        let base = canned_server_with_props(LLAMA_MODELS, LLAMA_PROPS_8192);
        let (healthy, out) =
            doctor_over(&keyless_config_with_window(&base, "served", 8192), false);
        assert!(healthy, "{out}");
        assert!(
            out.contains(&format!(
                "PASS: context_window 8192 matches the server context allocation (n_ctx 8192) at {base}"
            )),
            "{out}"
        );
    }

    #[test]
    fn context_check_configured_larger_warns_with_the_consequence() {
        let base = canned_server_with_props(LLAMA_MODELS, LLAMA_PROPS_8192);
        let (healthy, out) =
            doctor_over(&keyless_config_with_window(&base, "served", 16384), false);
        assert!(healthy, "WARN must not affect the exit code: {out}");
        assert!(
            out.contains("WARN: context_window 16384 is larger than the server context allocation (n_ctx 8192)"),
            "{out}"
        );
        assert!(
            out.contains("advisory fires too late") && out.contains("requests can fail"),
            "{out}"
        );
    }

    #[test]
    fn context_check_configured_smaller_warns_safe_but_early() {
        let base = canned_server_with_props(LLAMA_MODELS, LLAMA_PROPS_8192);
        let (healthy, out) =
            doctor_over(&keyless_config_with_window(&base, "served", 4096), false);
        assert!(healthy, "{out}");
        assert!(
            out.contains("WARN: context_window 4096 is smaller than the server context allocation (n_ctx 8192)"),
            "{out}"
        );
        assert!(out.contains("safe, but the advisory fires earlier"), "{out}");
    }

    #[test]
    fn context_check_unset_warn_suggests_the_exact_config_line() {
        let base = canned_server_with_props(LLAMA_MODELS, LLAMA_PROPS_8192);
        let (healthy, out) = doctor_over(&keyless_config(&base, "served"), false);
        assert!(healthy, "{out}");
        assert!(
            out.contains("WARN: no context_window configured;")
                && out.contains("allocates n_ctx 8192")
                && out.contains("add \"context_window\": 8192 to the profile"),
            "{out}"
        );
        // The WARN replaces the offline NOTE; never both for one profile.
        assert!(!out.contains("NOTE: no context_window"), "{out}");
    }

    #[test]
    fn context_check_non_llamacpp_server_is_silent_when_set_note_when_unset() {
        // Plain canned server: /props answers the MODELS body, which does
        // not parse as props, exactly a non-llama.cpp server's behavior.
        let base = canned_server(r#"{"data":[{"id":"served"}]}"#);
        let (healthy, out) =
            doctor_over(&keyless_config_with_window(&base, "served", 8192), false);
        assert!(healthy, "{out}");
        assert!(
            !out.contains("server context allocation") && !out.contains("NOTE: no context_window"),
            "probe None + window set = silence: {out}"
        );
        let (healthy, out) = doctor_over(&keyless_config(&base, "served"), false);
        assert!(healthy, "{out}");
        assert!(
            out.contains("NOTE: no context_window configured: the context usage advisory"),
            "{out}"
        );
    }

    #[test]
    fn context_note_offline_is_per_profile_and_only_for_unset() {
        let cfg = r#"{"profiles":{
            "bare":{"provider":"openai-compat","base_url":"http://127.0.0.1:1/v1","model":"m"},
            "sized":{"provider":"openai-compat","base_url":"http://127.0.0.1:1/v1","model":"m","context_window":8192}},
            "profile":"sized"}"#;
        let (healthy, out) = doctor_over(cfg, true);
        assert!(healthy, "{out}");
        assert!(
            out.contains("NOTE: profile \"bare\" no context_window configured"),
            "{out}"
        );
        assert_eq!(
            out.matches("no context_window configured").count(),
            1,
            "one line per affected profile, none for the sized one: {out}"
        );
        assert!(!out.contains("server context allocation"), "no probe offline: {out}");
    }

    #[test]
    fn context_check_keyed_profile_is_never_probed() {
        // The canned server WOULD answer /props; a keyed profile must not
        // ask, so the only context line is the offline NOTE.
        let base = canned_server_with_props(LLAMA_MODELS, LLAMA_PROPS_8192);
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
        assert!(!out.contains("server context allocation"), "{out}");
        assert!(out.contains("NOTE: no context_window configured"), "{out}");
    }

    // ------------------------- T31/T34: tools-drop probe (D-probe)

    const USAGE_10: &str = r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#;
    const USAGE_31: &str = r#"{"choices":[],"usage":{"prompt_tokens":31,"completion_tokens":1}}"#;

    /// The confirmed llama.cpp --jinja failure: identical prompt_tokens with
    /// and without a tools array means the template rendered nothing, and
    /// the server says so nowhere else.
    #[test]
    fn tools_drop_probe_warns_when_counts_are_identical() {
        let base = canned_server_with_completions(LLAMA_MODELS, USAGE_10, USAGE_10);
        let (healthy, out) = doctor_over(&keyless_config(&base, "m"), false);
        assert!(healthy, "a dropped tools array is a WARN, never a FAIL: {out}");
        assert!(
            out.contains(&format!(
                "WARN: the server at {base} appears to drop tool definitions for \"m\" (prompt_tokens 10 with and without temur's tools)"
            )),
            "{out}"
        );
        assert!(out.contains("tool calls can silently never happen"), "{out}");
    }

    #[test]
    fn tools_drop_probe_passes_when_counts_differ() {
        let base = canned_server_with_completions(LLAMA_MODELS, USAGE_10, USAGE_31);
        let (healthy, out) = doctor_over(&keyless_config(&base, "m"), false);
        assert!(healthy, "{out}");
        assert!(
            out.contains(&format!(
                "PASS: the server at {base} renders temur's tool definitions for \"m\" (prompt_tokens 10 without tools, 31 with)"
            )),
            "{out}"
        );
    }

    /// T34, the Hermes catch. The bare completion succeeds and the one
    /// carrying temur's real tool definitions is refused, which is exactly
    /// what a template that cannot render those schemas looks like. WARN,
    /// quoting the server, because doctor never FAILs on a degraded server
    /// and the server already said the useful thing.
    #[test]
    fn tools_drop_probe_warns_when_the_server_rejects_the_real_definitions() {
        let hermes = r#"{"error":{"code":500,"message":"Unable to generate parser for this template. Error: Object key of unhashable type: Array","type":"server_error"}}"#;
        let (base, _posts) = canned_server_with_completions_ex(
            LLAMA_MODELS,
            USAGE_10,
            "HTTP/1.1 400 Bad Request",
            hermes,
        );
        let (healthy, out) = doctor_over(&keyless_config(&base, "m"), false);
        assert!(healthy, "a rejecting server is a WARN, never a FAIL: {out}");
        assert!(
            out.contains(&format!(
                "WARN: the server at {base} rejected temur's tool definitions for \"m\" (HTTP 400:"
            )),
            "{out}"
        );
        // The server's own words, which are the whole diagnosis here.
        assert!(out.contains("Object key of unhashable type: Array"), "{out}");
        assert!(
            out.contains("every turn that sends tools will fail the same way"),
            "{out}"
        );
        // The pre-T34 probe reported PASS against exactly this server.
        assert!(!out.contains("renders temur's tool definitions"), "{out}");
    }

    /// T34: what goes out is what a session would send. Ties P1 to P2: the
    /// `skill` tool rides every request, and its "section" type is the
    /// schema that made a shipped template throw.
    #[test]
    fn tools_drop_probe_sends_the_real_registry_schemas() {
        let (base, posts) = canned_server_with_completions_ex(
            LLAMA_MODELS,
            USAGE_10,
            "HTTP/1.1 200 OK",
            USAGE_31,
        );
        let (healthy, out) = doctor_over(&keyless_config(&base, "m"), false);
        assert!(healthy, "{out}");
        let bodies = posts.lock().unwrap().clone();
        // T41 put a THIRD POST on the wire (the prompt-floor probe), and
        // it carries tools too. The tools-drop pair is the one without a
        // system message; see `the_floor_probe_carries_the_real_system_prompt`.
        let with_tools = bodies
            .iter()
            .find(|b| b.contains("\"tools\"") && !b.contains("\"system\""))
            .expect("one tools-drop request carries tools and no system");
        let json_start = with_tools.find("{").expect("a JSON body");
        let v: serde_json::Value =
            serde_json::from_str(&with_tools[json_start..]).expect("parseable body");
        let tools = v["tools"].as_array().expect("a tools array");
        let expected = crate::tools::Registry::standard_with_skills(vec![]).definitions();
        assert_eq!(tools.len(), expected.len(), "the whole registry, not a probe tool");
        assert!(
            tools.iter().any(|t| t["function"]["name"] == "bash"),
            "{with_tools}"
        );
        let skill = tools
            .iter()
            .find(|t| t["function"]["name"] == "skill")
            .expect("the skill tool is registered unconditionally");
        assert_eq!(
            skill["function"]["parameters"]["properties"]["section"]["type"],
            "string",
            "T34 P1: a union type here is a 400 on servers that render schemas"
        );
        // The synthetic tool the pre-T34 probe sent is gone.
        assert!(!with_tools.contains("\"name\":\"probe\""), "{with_tools}");
    }

    #[test]
    fn tools_drop_probe_missing_usage_is_a_note() {
        let no_usage = r#"{"choices":[{"message":{"content":"hi"}}]}"#;
        let base = canned_server_with_completions(LLAMA_MODELS, no_usage, no_usage);
        let (healthy, out) = doctor_over(&keyless_config(&base, "m"), false);
        assert!(healthy, "{out}");
        assert!(
            out.contains(&format!(
                "NOTE: tools-drop probe at {base} skipped: the server reported no usable prompt_tokens"
            )),
            "{out}"
        );
        assert!(!out.contains("drop tool definitions"), "{out}");
    }

    /// A bare request that itself fails is the NOTE class, not the new
    /// rejection WARN: nothing was learned about the tool definitions.
    #[test]
    fn tools_drop_probe_failing_bare_request_is_a_note() {
        let (base, _posts) = canned_server_with_completions_ex(
            LLAMA_MODELS,
            "<html>gateway</html>",
            "HTTP/1.1 400 Bad Request",
            r#"{"error":{"message":"nope"}}"#,
        );
        let (healthy, out) = doctor_over(&keyless_config(&base, "m"), false);
        assert!(healthy, "{out}");
        assert!(
            out.contains(&format!(
                "NOTE: tools-drop probe at {base} skipped: the server reported no usable prompt_tokens"
            )),
            "{out}"
        );
        assert!(!out.contains("rejected temur's tool definitions"), "{out}");
    }

    /// T34: a bare request that answers and a with-tools request that never
    /// does is the shape a slow local server makes, and the shape doctor
    /// itself produced live on 2026-08-18 before the probe got its own
    /// timeout: the second request has to prefill every tool definition.
    /// Its NOTE says which request went unanswered, so it cannot be read as
    /// "the server said nothing useful".
    #[test]
    fn tools_drop_probe_unanswered_with_tools_request_names_itself() {
        let (base, _posts) =
            canned_server_with_completions_ex(LLAMA_MODELS, USAGE_10, "", USAGE_31);
        let (healthy, out) = doctor_over(&keyless_config(&base, "m"), false);
        assert!(healthy, "{out}");
        assert!(
            out.contains(&format!("NOTE: tools-drop probe at {base} inconclusive")),
            "{out}"
        );
        assert!(
            out.contains("answered a bare completion but not the one carrying temur's tools"),
            "{out}"
        );
    }

    /// The heads-up line: a check that can go quiet for minutes says so
    /// before it does, not after.
    #[test]
    fn tools_drop_probe_announces_what_it_is_about_to_send() {
        let base = canned_server_with_completions(LLAMA_MODELS, USAGE_10, USAGE_31);
        let (_healthy, out) = doctor_over(&keyless_config(&base, "m"), false);
        let expected = crate::tools::Registry::standard_with_skills(vec![]).definitions();
        assert!(
            out.contains(&format!(
                "NOTE: tools-drop probe: sending {} tool definitions (~",
                expected.len()
            )),
            "{out}"
        );
        assert!(out.contains("can take minutes the first time"), "{out}");
    }

    #[test]
    fn tools_drop_probe_is_absent_under_no_network() {
        let base = canned_server_with_completions(LLAMA_MODELS, USAGE_10, USAGE_10);
        let (healthy, out) = doctor_over(&keyless_config(&base, "m"), true);
        assert!(healthy, "{out}");
        assert!(!out.contains("tools-drop probe"), "{out}");
        assert!(!out.contains("tool definitions"), "{out}");
    }

    // ----------------------------------------- T41: the prompt floor

    /// The window that puts the floor at exactly 39% and 40% of it, given
    /// a server that reports `tokens` for the floor probe. Chosen so the
    /// PASS/WARN boundary is asserted on the number, not on a real prompt
    /// whose size drifts with the cwd.
    const USAGE_3900: &str =
        r#"{"choices":[],"usage":{"prompt_tokens":3900,"completion_tokens":1}}"#;
    const USAGE_4000: &str =
        r#"{"choices":[],"usage":{"prompt_tokens":4000,"completion_tokens":1}}"#;

    /// A keyless openai-compat config with a window, and optionally an
    /// explicit prompt_profile.
    fn windowed_keyless(base: &str, window: u64, prompt_profile: Option<&str>) -> String {
        let pp = prompt_profile
            .map(|p| format!(r#""prompt_profile":"{p}","#))
            .unwrap_or_default();
        format!(
            r#"{{"provider":"openai-compat",{pp}"openai_compat":{{"base_url":"{base}","model":"m","context_window":{window}}}}}"#
        )
    }

    /// The measured half asks the server that will serve the session, with
    /// what the session would actually send: the real system prompt AND
    /// the real definitions, capped at one generated token.
    #[test]
    fn the_floor_probe_carries_the_real_system_prompt_and_the_real_definitions() {
        let (base, posts) = canned_server_with_completions_ex(
            LLAMA_MODELS,
            USAGE_10,
            "HTTP/1.1 200 OK",
            USAGE_3900,
        );
        let (healthy, out) = doctor_over(&windowed_keyless(&base, 10000, None), false);
        assert!(healthy, "{out}");
        let bodies = posts.lock().unwrap().clone();
        let floor = bodies
            .iter()
            .find(|b| b.contains("\"system\""))
            .expect("the floor probe carries a system message");
        let v: serde_json::Value =
            serde_json::from_str(&floor[floor.find('{').expect("a JSON body")..])
                .expect("parseable body");
        assert_eq!(v["messages"][0]["role"], "system");
        let sent = v["messages"][0]["content"].as_str().expect("a system string");
        // The compact template, because a 10000 window auto-selects it.
        assert!(sent.starts_with("You are temur, a coding agent in a terminal."), "{sent}");
        assert_eq!(v["max_tokens"], 1, "one generated token, like its siblings");
        let expected = crate::tools::Registry::standard_with_skills(vec![]).definitions();
        assert_eq!(
            v["tools"].as_array().expect("a tools array").len(),
            expected.len(),
            "the whole registry rides the floor probe"
        );
    }

    /// The T34 comparison baseline must not move: the two tools-drop
    /// bodies are byte-identical to their pre-T41 selves, which here means
    /// they carry no system message at all.
    #[test]
    fn the_two_tools_drop_bodies_carry_no_system_field() {
        let (base, posts) = canned_server_with_completions_ex(
            LLAMA_MODELS,
            USAGE_10,
            "HTTP/1.1 200 OK",
            USAGE_31,
        );
        let (_healthy, _out) = doctor_over(&windowed_keyless(&base, 10000, None), false);
        let bodies = posts.lock().unwrap().clone();
        assert_eq!(bodies.len(), 3, "one floor probe plus the tools-drop pair");
        let without_system: Vec<&String> =
            bodies.iter().filter(|b| !b.contains("\"system\"")).collect();
        assert_eq!(without_system.len(), 2, "exactly the tools-drop pair: {bodies:?}");
        // And they are still the bare/with-tools pair, unchanged.
        let expected_bare = crate::provider::tools_drop_probe_body("m", None, None);
        assert!(
            without_system.iter().any(|b| b.ends_with(&expected_bare)),
            "the bare body is byte-identical: {without_system:?}"
        );
    }

    #[test]
    fn floor_passes_below_forty_percent_and_warns_at_it() {
        // 3900 of 10000 = 39%: PASS.
        let (base, _p) = canned_server_with_completions_ex(
            LLAMA_MODELS,
            USAGE_10,
            "HTTP/1.1 200 OK",
            USAGE_3900,
        );
        let (healthy, out) = doctor_over(&windowed_keyless(&base, 10000, None), false);
        assert!(healthy, "{out}");
        assert!(
            out.contains("PASS: prompt floor (measured): 3900 tokens; window 10000; 39% of the window"),
            "{out}"
        );
        assert!(!out.contains("(estimate)"), "a measurement wins: {out}");
        // No estimate caveat when nothing was estimated.
        assert!(!out.contains("is not tokenization"), "{out}");
        assert!(out.contains("moves with the length of the cwd path"), "{out}");

        // 4000 of 10000 = 40%: WARN, naming the fix.
        let (base, _p) = canned_server_with_completions_ex(
            LLAMA_MODELS,
            USAGE_10,
            "HTTP/1.1 200 OK",
            USAGE_4000,
        );
        let (healthy, out) = doctor_over(&windowed_keyless(&base, 10000, Some("full")), false);
        assert!(healthy, "a big floor is a WARN, never a FAIL: {out}");
        assert!(
            out.contains("WARN: prompt floor (measured): 4000 tokens; window 10000; 40% of the window is spent before the task starts; set prompt_profile to \"compact\" or raise context_window"),
            "{out}"
        );
    }

    /// Already compact and still over the line: the advice this check
    /// usually gives is spent, so it gives the other one instead.
    #[test]
    fn floor_warn_on_an_already_compact_profile_says_so_and_points_elsewhere() {
        let (base, _p) = canned_server_with_completions_ex(
            LLAMA_MODELS,
            USAGE_10,
            "HTTP/1.1 200 OK",
            USAGE_4000,
        );
        let (healthy, out) =
            doctor_over(&windowed_keyless(&base, 10000, Some("compact")), false);
        assert!(healthy, "{out}");
        assert!(out.contains("the compact profile is ALREADY active"), "{out}");
        assert!(
            out.contains("raise context_window, or serve a model with a larger window"),
            "{out}"
        );
        assert!(
            !out.contains("set prompt_profile to \"compact\""),
            "never advise what is already true: {out}"
        );
    }

    /// A server that answers nothing usable leaves the offline estimate,
    /// which says so in the word "estimate" and carries its own caveat.
    #[test]
    fn floor_falls_back_to_the_estimate_when_the_probe_says_nothing_usable() {
        let no_usage = r#"{"choices":[{"message":{"content":"hi"}}]}"#;
        let base = canned_server_with_completions(LLAMA_MODELS, no_usage, no_usage);
        let (healthy, out) = doctor_over(&windowed_keyless(&base, 100000, None), false);
        assert!(healthy, "{out}");
        assert!(out.contains("prompt floor (estimate): ~"), "{out}");
        assert!(out.contains("is not tokenization"), "the caveat rides it: {out}");
    }

    #[test]
    fn floor_under_no_network_is_the_estimate_with_a_skip_line() {
        let (healthy, out) = doctor_over(
            r#"{"provider":"openai-compat","openai_compat":{"model":"m","context_window":100000}}"#,
            true,
        );
        assert!(healthy, "{out}");
        assert!(out.contains("SKIP: prompt floor measurement (--no-network)"), "{out}");
        assert!(out.contains("PASS: prompt floor (estimate): ~"), "{out}");
        assert!(out.contains("window 100000"), "{out}");
        assert!(!out.contains("(measured)"), "{out}");
    }

    /// No window: the number is still worth printing, and a percentage of
    /// nothing is not. NOTE, so it cannot move the exit code either way.
    #[test]
    fn floor_without_a_window_is_a_note_with_no_verdict() {
        let (healthy, out) = doctor_over(
            r#"{"provider":"openai-compat","openai_compat":{"model":"m"}}"#,
            true,
        );
        assert!(healthy, "{out}");
        assert!(
            out.contains("NOTE: prompt floor (estimate): ~")
                && out.contains("no context_window is configured, so there is nothing to compare it against"),
            "{out}"
        );
        assert!(!out.contains("% of the window"), "no percentage of nothing: {out}");
    }

    /// The estimate follows the ACTIVE profile: the compact prompts really
    /// are the smaller number, which is the whole reason auto exists.
    #[test]
    fn the_estimate_is_smaller_on_the_compact_profile() {
        fn floor_of(profile: &str) -> u64 {
            let (_healthy, out) = doctor_over(
                &format!(
                    r#"{{"provider":"openai-compat","prompt_profile":"{profile}","openai_compat":{{"model":"m","context_window":100000}}}}"#
                ),
                true,
            );
            let at = out.find("prompt floor (estimate): ~").expect("a floor line");
            out[at..]
                .split('~')
                .nth(1)
                .and_then(|s| s.split(' ').next())
                .and_then(|s| s.parse().ok())
                .expect("a number")
        }
        assert!(floor_of("compact") < floor_of("full"));
    }

    /// The floor probe is gated exactly like the tools-drop probe: no
    /// network, no POST.
    #[test]
    fn floor_makes_no_request_under_no_network() {
        let (base, posts) = canned_server_with_completions_ex(
            LLAMA_MODELS,
            USAGE_10,
            "HTTP/1.1 200 OK",
            USAGE_31,
        );
        let (_healthy, _out) = doctor_over(&windowed_keyless(&base, 10000, None), true);
        assert!(posts.lock().unwrap().is_empty(), "no POST under --no-network");
    }

    /// The tie between the two constants, which v0.30.0 shipped broken.
    /// At exactly `PROMPT_AUTO_COMPACT_BELOW` the auto rule picks FULL,
    /// so the full profile's own floor has to sit under the percentage at
    /// which this same binary WARNs about it. Computed with the shipped
    /// estimator over the real full-profile prompts and the real registry,
    /// so a change to either constant, or to the prompts, fails here
    /// instead of shipping a default that doctor immediately advises the
    /// user to undo.
    #[test]
    fn the_auto_threshold_keeps_the_full_floor_under_the_doctor_warn_line() {
        let window = crate::config::PROMPT_AUTO_COMPACT_BELOW;
        assert_eq!(
            crate::config::auto_prompt_profile(Some(window)),
            crate::tools::PromptProfile::Full,
            "the threshold itself selects full: that is what makes this a tie"
        );
        // A representative cwd, not this checkout's: the floor moves with
        // the path length, and the tie must not depend on where the
        // repository happens to live.
        let system =
            crate::prompt::system_prompt_template(crate::tools::PromptProfile::Full)
                .replace("{cwd}", "/home/user/projects/example-project");
        let defs = crate::tools::Registry::standard_with_skills(vec![])
            .with_profile(crate::tools::PromptProfile::Full)
            .definitions();
        let estimate = floor_estimate(&system, &defs);
        let percent = estimate.saturating_mul(100) / window.max(1);
        assert!(
            percent < PROMPT_FLOOR_WARN_PERCENT,
            "PROMPT_AUTO_COMPACT_BELOW ({window}) selects the FULL profile, whose \
             floor estimates at {estimate} tokens = {percent}% of that window, at or \
             above PROMPT_FLOOR_WARN_PERCENT ({PROMPT_FLOOR_WARN_PERCENT}): doctor \
             would WARN about the very profile the auto rule just chose, and tell the \
             user to set prompt_profile to \"compact\". Raise \
             PROMPT_AUTO_COMPACT_BELOW, or shrink the full prompts."
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
            run_with_sandbox_probe(&cfg_path, true, &mut out, &move || probe_ok, &no_install())
                .unwrap();
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
    fn keyed_config_without_sandbox_warns_naming_approval_refusal_and_override() {
        let tmp = tempfile::tempdir().unwrap();
        let (healthy, out) = doctor_probed(&keyed_config(tmp.path(), ""), false);
        assert!(healthy, "WARN must not affect the exit code: {out}");
        assert!(out.contains("WARN: bash key sandbox: unavailable"), "{out}");
        // T21: the arm names all three outcomes and the docs section.
        assert!(out.contains("ask per-command approval"), "{out}");
        assert!(out.contains("non-interactive runs refuse"), "{out}");
        assert!(out.contains("allow_bash_without_key_sandbox"), "{out}");
        assert!(out.contains("README.md, section \"Untrusted hosts\""), "{out}");
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

    // ---------------------------- T13 F4: stale-install (version skew)

    /// The probe that says "nothing to compare", which is what every test
    /// predating this check wants: their output must not depend on what
    /// the host happens to have on its PATH.
    fn no_install() -> InstallProbe<'static> {
        InstallProbe { current_exe: None, path_var: None }
    }

    /// Doctor over a minimal keyless config, offline, with the install
    /// probe injected: the install line is the only thing that varies.
    fn doctor_install(current_exe: Option<&Path>, path_dirs: &[&Path]) -> String {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        std::fs::write(&cfg_path, keyless_config("http://127.0.0.1:1/v1", "m")).unwrap();
        let joined = std::env::join_paths(path_dirs.iter()).unwrap();
        let mut out: Vec<u8> = Vec::new();
        run_with_sandbox_probe(
            &cfg_path,
            true,
            &mut out,
            &|| true,
            &InstallProbe {
                current_exe,
                path_var: Some(joined.as_os_str()),
            },
        )
        .unwrap();
        String::from_utf8(out).unwrap()
    }

    fn fake_binary(dir: &Path, bytes: &[u8]) -> std::path::PathBuf {
        let p = dir.join("temur");
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn install_passes_when_path_holds_this_very_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = fake_binary(tmp.path(), b"ELF-ish\n");
        let out = doctor_install(Some(&bin), &[tmp.path()]);
        assert!(
            out.contains(&format!(
                "PASS: install: the temur on PATH ({}) is this running binary (temur {})",
                bin.display(),
                env!("CARGO_PKG_VERSION")
            )),
            "{out}"
        );
    }

    #[test]
    fn install_passes_on_a_byte_identical_copy_at_another_path() {
        let installed = tempfile::tempdir().unwrap();
        let built = tempfile::tempdir().unwrap();
        let on_path = fake_binary(installed.path(), b"same bytes\n");
        let running = fake_binary(built.path(), b"same bytes\n");
        let out = doctor_install(Some(&running), &[installed.path()]);
        assert!(
            out.contains(&format!(
                "PASS: install: the temur on PATH ({}) is a byte-identical copy of this running binary (temur {})",
                on_path.display(),
                env!("CARGO_PKG_VERSION")
            )),
            "{out}"
        );
    }

    #[test]
    fn install_warns_on_a_stale_path_copy_and_names_the_direction() {
        let installed = tempfile::tempdir().unwrap();
        let built = tempfile::tempdir().unwrap();
        let on_path = fake_binary(installed.path(), b"old build\n");
        let running = fake_binary(built.path(), b"a different, newer build\n");
        let now = std::time::SystemTime::now();
        touch_at(&on_path, now - std::time::Duration::from_secs(30 * 86_400));
        touch_at(&running, now - std::time::Duration::from_secs(86_400));
        let out = doctor_install(Some(&running), &[installed.path()]);
        assert!(
            out.contains(&format!(
                "WARN: install: the temur on PATH ({}) is a DIFFERENT build from the one running ({})",
                on_path.display(),
                running.display()
            )),
            "{out}"
        );
        assert!(
            out.contains("PATH copy modified 30 day(s) ago, running binary modified 1 day(s) ago"),
            "{out}"
        );
        assert!(
            out.contains("the PATH copy is the older one")
                && out.contains("scripts/install.sh")
                && out.contains("~/.local/bin"),
            "{out}"
        );
        // Never a FAIL: a second copy is a legitimate setup.
        assert!(!out.contains("FAIL: install:"), "{out}");
    }

    #[test]
    fn install_warns_the_other_direction_when_the_path_copy_is_newer() {
        let installed = tempfile::tempdir().unwrap();
        let built = tempfile::tempdir().unwrap();
        let on_path = fake_binary(installed.path(), b"newer build\n");
        let running = fake_binary(built.path(), b"an older, different build\n");
        let now = std::time::SystemTime::now();
        touch_at(&on_path, now - std::time::Duration::from_secs(86_400));
        touch_at(&running, now - std::time::Duration::from_secs(30 * 86_400));
        let out = doctor_install(Some(&running), &[installed.path()]);
        assert!(
            out.contains("the PATH copy is the newer one, so this session is running an older build"),
            "{out}"
        );
    }

    #[test]
    fn install_is_silent_with_nothing_to_compare() {
        // Nothing named temur anywhere on PATH.
        let empty = tempfile::tempdir().unwrap();
        let built = tempfile::tempdir().unwrap();
        let running = fake_binary(built.path(), b"build\n");
        let out = doctor_install(Some(&running), &[empty.path()]);
        assert!(!out.contains("install:"), "{out}");
        // No current_exe: same silence, and this is the arm every other
        // doctor test runs under.
        let installed = tempfile::tempdir().unwrap();
        fake_binary(installed.path(), b"build\n");
        let out = doctor_install(None, &[installed.path()]);
        assert!(!out.contains("install:"), "{out}");
    }

    #[test]
    fn install_takes_the_first_temur_on_path_the_one_a_shell_would_run() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let built = tempfile::tempdir().unwrap();
        let winner = fake_binary(first.path(), b"first on PATH\n");
        let loser = fake_binary(second.path(), b"never reached\n");
        let running = fake_binary(built.path(), b"first on PATH\n");
        let out = doctor_install(Some(&running), &[first.path(), second.path()]);
        assert!(out.contains(&format!("{}", winner.display())), "{out}");
        assert!(!out.contains(&format!("{}", loser.display())), "{out}");
    }

}
