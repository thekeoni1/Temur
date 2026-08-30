use temur::agent::{Session, SessionConfig};
use temur::provider::anthropic::AnthropicProvider;
use temur::provider::openai_compat::OpenAiCompatProvider;
use temur::provider::transport::ReplayTransport;
use temur::provider::Provider;
use temur::tools::Registry;
use temur::agent::events::AgentEvent;
use temur::ui::tui::{SessionInfo, TuiUi};
use temur::ui::{repl::ReplUi, Ui};
use temur::{config, error, secret};
use std::io::IsTerminal;
use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Compact default system prompt for v1; overridable via config.
/// (`{cwd}` is substituted at startup.)
const DEFAULT_SYSTEM: &str = "You are temur, a terminal coding agent. You help with software \
engineering tasks: reading and editing code, running commands, and searching the codebase.\n\
Use the provided tools (read, write, edit, bash, glob, grep, todowrite, todoread, skill) to act; \
prefer tools over guessing. Keep responses concise and direct — this is a terminal. \
When you edit files, verify your changes. \
You can see the local filesystem through these tools, so list or read a path before saying you \
cannot access it. \
The current working directory is: {cwd}";

/// Shorter default system prompt used when `prompt_profile` is `"compact"`
/// AND no config `system_prompt` override exists — an explicit override
/// always wins, in either profile.
const DEFAULT_SYSTEM_COMPACT: &str = "You are temur, a coding agent in a terminal. Act through \
the provided tools; always call them with valid JSON arguments — never write a tool call as \
plain text. Prefer tools over guessing, keep answers short, verify edits. \
You can see the local filesystem through these tools, so list or read a path before saying you \
cannot access it. \
Working directory: {cwd}";

fn main() -> ExitCode {
    env_logger::init();
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("temur: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, error::Error> {
    use lexopt::prelude::*;
    let mut parser = lexopt::Parser::from_env();
    let mut cmd: Option<String> = None;
    let mut mock: Option<String> = None;
    let mut capture: Option<String> = None;
    // --continue (`continue` is a keyword, hence the variable name): resume
    // this directory's saved session instead of starting fresh.
    let mut resume = false;
    // --resume <key> (T10): resume a session by name / file-name prefix,
    // resolved over ALL saved sessions. Distinct from --continue, which
    // takes no key and always means this directory's default session.
    let mut resume_key: Option<String> = None;
    // UI selection: --tui / --plain force it; default is TUI on a real
    // terminal, plain line REPL otherwise (so piped/scripted use — the mock
    // e2e, operator scripts — is unchanged without any flag).
    let mut force_tui = false;
    let mut force_plain = false;
    // -p/--prompt (T14): one-shot mode. Run exactly one full agentic turn
    // over this prompt on the plain path, then exit by outcome.
    let mut oneshot: Option<String> = None;
    // --force (T14): only meaningful for `init` (overwrite an existing
    // config); rejected anywhere else so a typo cannot look accepted.
    let mut force = false;
    // --no-network (T14): only meaningful for `doctor` (skip probes).
    let mut no_network = false;
    // --add <template> (T17): only meaningful for `init` (merge a template
    // into an existing config as profiles, instead of writing a fresh one).
    let mut add: Option<String> = None;
    while let Some(arg) = parser.next()? {
        match arg {
            Long("version") | Short('V') => {
                println!("temur {VERSION}");
                return Ok(ExitCode::SUCCESS);
            }
            Long("mock") => mock = Some(parser.value()?.string()?),
            Long("capture-sse") => capture = Some(parser.value()?.string()?),
            Long("continue") => resume = true,
            Long("resume") => resume_key = Some(parser.value()?.string()?),
            Long("tui") => force_tui = true,
            Long("plain") => force_plain = true,
            Short('p') | Long("prompt") => oneshot = Some(parser.value()?.string()?),
            Long("force") => force = true,
            Long("no-network") => no_network = true,
            Long("add") => add = Some(parser.value()?.string()?),
            Value(v) if cmd.is_none() => cmd = Some(v.string()?),
            arg => return Err(arg.unexpected().into()),
        }
    }
    if force && cmd.as_deref() != Some("init") {
        return Err(error::Error::Usage(
            "--force is only valid with the init subcommand".into(),
        ));
    }
    if no_network && cmd.as_deref() != Some("doctor") {
        return Err(error::Error::Usage(
            "--no-network is only valid with the doctor subcommand".into(),
        ));
    }
    if add.is_some() && cmd.as_deref() != Some("init") {
        return Err(error::Error::Usage(
            "--add is only valid with the init subcommand".into(),
        ));
    }
    // Contradictory by construction: --force overwrites the whole config,
    // --add merges into it.
    if add.is_some() && force {
        return Err(error::Error::Usage(
            "--force does not combine with --add (init --add merges into the existing config)"
                .into(),
        ));
    }
    if force_tui && force_plain {
        return Err(error::Error::Usage("--tui and --plain are mutually exclusive".into()));
    }
    // One-shot is plain by definition: there is nothing interactive to draw.
    if oneshot.is_some() && force_tui {
        return Err(error::Error::Usage(
            "-p/--prompt and --tui are mutually exclusive".into(),
        ));
    }
    if oneshot.is_some() && cmd.is_some() {
        return Err(error::Error::Usage(
            "-p/--prompt does not combine with a subcommand".into(),
        ));
    }
    // One resume flag at a time: they disagree about WHICH file to load.
    if resume && resume_key.is_some() {
        return Err(error::Error::Usage(
            "--continue and --resume are mutually exclusive".into(),
        ));
    }
    // Persistence is disabled under --mock (fixtures must never touch real
    // state), so a --continue/--resume there could only ever mislead.
    if resume && mock.is_some() {
        return Err(error::Error::Usage("--continue is unavailable with --mock".into()));
    }
    if resume_key.is_some() && mock.is_some() {
        return Err(error::Error::Usage("--resume is unavailable with --mock".into()));
    }
    let use_tui = if oneshot.is_some() || force_plain {
        false
    } else if force_tui {
        true
    } else {
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
    };
    // T13 F13(a). Reachable only through --tui, since auto-select above
    // already requires both terminals. The TUI's event source needs a real
    // terminal on both ends; given a pipe it draws a prompt it can never
    // read from and spins on redraws indefinitely. Refuse up front, naming
    // the two ways to get work done without a terminal.
    if use_tui && !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        return Err(error::Error::Usage(
            "the TUI needs a terminal on stdin and stdout: use -p \"...\" for piped one-shot input, or --plain for the line REPL".into(),
        ));
    }

    match cmd.as_deref() {
        Some("init") => {
            let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
            // T15+T22: the wizard's ONLY network capabilities, the two
            // keyless, unauthenticated GETs (listing + /props context
            // probe), both on the short timeout.
            let list = |base: &str| {
                temur::provider::list_models_keyless(
                    base,
                    std::time::Duration::from_secs(
                        temur::provider::KEYLESS_LISTING_TIMEOUT_SECS,
                    ),
                )
            };
            let probe = |base: &str| {
                temur::provider::probe_props_context(
                    base,
                    std::time::Duration::from_secs(
                        temur::provider::KEYLESS_LISTING_TIMEOUT_SECS,
                    ),
                )
            };
            // T17 P3: the hidden key prompt's terminal seam, real termios
            // over stdin here.
            let mut term = temur::init::StdinKeyTerminal::new();
            match &add {
                Some(template) => temur::init::run_add(
                    &config::config_path(),
                    home.as_deref(),
                    template,
                    &mut std::io::stdin().lock(),
                    &mut std::io::stdout(),
                    &list,
                    &probe,
                    &mut term,
                )?,
                None => temur::init::run(
                    &config::config_path(),
                    home.as_deref(),
                    force,
                    &mut std::io::stdin().lock(),
                    &mut std::io::stdout(),
                    &list,
                    &probe,
                    &mut term,
                )?,
            }
            Ok(ExitCode::SUCCESS)
        }
        Some("doctor") => {
            let healthy = temur::doctor::run(
                &config::config_path(),
                no_network,
                &mut std::io::stdout(),
            )?;
            Ok(if healthy {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Some("tls-probe") => {
            tls_probe()?;
            Ok(ExitCode::SUCCESS)
        }
        Some("tui-probe") => {
            temur::ui::tui::probe()?;
            Ok(ExitCode::SUCCESS)
        }
        Some(other) => Err(error::Error::Usage(format!("unknown command: {other}"))),
        None => repl(mock, capture, use_tui, resume, resume_key, oneshot),
    }
}

fn repl(
    mock: Option<String>,
    capture: Option<String>,
    use_tui: bool,
    resume: bool,
    resume_key: Option<String>,
    oneshot: Option<String>,
) -> Result<ExitCode, error::Error> {
    let (cfg, cfg_existed) = config::Config::load_reporting()?;
    // Validated up front: an unknown GLOBAL prompt_profile is a startup
    // error even when every named profile overrides it (per-profile values
    // are validated inside resolved_profiles below).
    cfg.prompt_profile_spec()?;
    let cwd = std::env::current_dir()?;

    // Session persistence (T5), resolved up front so a bad cap is a startup
    // error, not a mid-session surprise. Default-on for live runs; disabled
    // under --mock — fixtures must never touch real state. --capture-sse
    // runs keep it on. Nothing is written here: a fresh run only overwrites
    // this directory's file on its FIRST SAVE, so launching and quitting
    // without a turn never destroys a resumable session.
    let session_max_bytes = cfg.session_max_bytes()?;
    // T10: resolved once; /sessions, /resume, /new, and --resume all work
    // over this directory. `persist_path` is mutable now — /resume and /new
    // redirect where the driver loop saves.
    let sessions_dir = temur::session_store::sessions_dir(cfg.sessions_dir.as_deref());
    let mut persist_path = if mock.is_none() {
        Some(temur::session_store::session_path(&sessions_dir, &cwd))
    } else {
        None
    };
    // The live session's name (None = default), recorded by every save.
    let mut session_name: Option<String> = None;

    // Resolve the skill search path and enumerate installed skills once at
    // startup. Env override wins over config; both fall back to the always-included
    // `.temur/skills` defaults resolved inside skill_dirs().
    let skill_override = std::env::var("TEMUR_SKILLS_DIR")
        .or_else(|_| std::env::var("OPENCODE_SKILLS_DIR")) // pre-rename name, one release
        .ok()
        .or_else(|| cfg.skills_dir.clone());
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let skill_dirs =
        temur::skills::skill_dirs(skill_override.as_deref(), &cwd, home.as_deref());
    let installed_skills = temur::skills::enumerate(&skill_dirs);

    // Provider selection (T2; named profiles T8). The default stays
    // anthropic — selecting anything else is a config change, never
    // inferred. Every named profile is validated eagerly here, so a later
    // `/model` switch can only fail on credential/IO problems; with no
    // startup profile the base fields resolve through the pre-T8 path,
    // byte-identical behavior (and error strings) included. --mock replays
    // fixtures through the SELECTED provider, so the selection path itself
    // is exercised offline.
    let profiles = cfg.resolved_profiles()?;
    let (mut active_profile, resolved) = cfg.startup_selection(&profiles)?;
    // T14 first-run quickstart: a genuinely missing config file on a live
    // run (--mock replays never need credentials) whose selection would need
    // a key that is not there means the very next step is the raw
    // "secret: APP_SECRET_FILE is not set" error. Replace that with
    // guidance. Any EXISTING config file, and any run with a usable
    // credential path (the appsvc launcher sets APP_SECRET_FILE with no
    // config file at all), behaves byte-identically to before.
    if !cfg_existed
        && mock.is_none()
        && resolved.provider == "anthropic"
        && resolved.api_key_file.is_none()
        && std::env::var_os("APP_SECRET_FILE").is_none()
    {
        eprint!("{}", quickstart_text());
        return Ok(ExitCode::FAILURE);
    }
    let is_compat = resolved.provider == "openai-compat";
    let model = resolved.model.clone();
    let cwd_display = cwd.display().to_string();
    // T9: the ACTIVE prompt profile — starts as the startup selection's
    // (profile's own > global > full), then tracks `/model` switches.
    let mut current_prompt_profile = resolved.prompt_profile;
    // T9: the FULL active selection, tracked for `/models` and raw-id
    // `/model` switches (both derive endpoint/credentials/limits from it).
    let mut active_resolved = resolved.clone();

    // --continue: load BEFORE provider construction — "you have no session
    // to resume" should not hide behind a credential error — and FAIL FAST
    // on a missing, corrupt, or wrong-version file: never silently start a
    // fresh session over the very file the user asked to resume. The replay
    // event and notices are collected here but emitted only after UI
    // construction (the TUI swallows pre-alt-screen printlns).
    //
    // T10: the seeded history is also flattened into a SessionLoaded event,
    // so BOTH UIs render the resumed backscroll; advisory notices (provider/
    // model/cwd mismatches, the dropped-prompt rule) follow it, surviving
    // the TUI's transcript rebuild.
    let mut pending_notices: Vec<String> = Vec::new();
    // T41: the auto rule picked compact for this selection, so say so once.
    // A user who never wrote "compact" anywhere should not have to guess why
    // the tool descriptions look short, and the line names the override.
    // Nothing is printed when auto chose full: that is the shape every
    // config had before T41.
    if let (
        temur::config::PromptProfileSource::Auto,
        temur::tools::PromptProfile::Compact,
        Some(w),
    ) = (
        resolved.prompt_profile_source,
        resolved.prompt_profile,
        resolved.context_window,
    ) {
        pending_notices.push(temur::config::auto_compact_notice(w));
    }
    let mut pending_loaded: Option<AgentEvent> = None;
    let seed = if resume || resume_key.is_some() {
        // Which file: --continue = this directory's default session;
        // --resume <key> = resolved over the full listing, exactly like
        // /resume (and the resumed file becomes the save target).
        let path: std::path::PathBuf = match &resume_key {
            None => persist_path
                .clone()
                .expect("--continue with --mock is rejected at argument parsing"),
            Some(key) => {
                let entries = temur::session_store::list_sessions(&sessions_dir);
                if entries.is_empty() {
                    return Err(error::Error::Session(format!(
                        "no saved sessions in {} — sessions are created by a first turn, so \
                         there is nothing to --resume yet",
                        sessions_dir.display()
                    )));
                }
                match temur::session_store::resolve_session_key(&entries, &cwd_display, key) {
                    Ok(e) => sessions_dir.join(&e.file_name),
                    Err(msg) => return Err(error::Error::Session(msg)),
                }
            }
        };
        let file = match temur::session_store::load(&path) {
            Ok(f) => f,
            Err(e @ temur::session_store::StoreError::Missing { .. })
                if resume_key.is_none() =>
            {
                return Err(error::Error::Session(format!(
                    "{e} — run without --continue to start one"
                )))
            }
            Err(e) => return Err(e.into()),
        };
        session_name = file.name.clone();
        persist_path = Some(path);
        pending_notices.extend(temur::session_store::mismatch_notices(
            &file,
            &resolved.provider,
            &model,
            &cwd_display,
        ));
        let (seed, mut notices) = temur::session_store::prepare_seed(file);
        // prepare_seed's contract: notices end with the resume summary; any
        // drop notice precedes it. The summary rides inside SessionLoaded.
        let summary = notices
            .pop()
            .expect("prepare_seed always appends the resume summary");
        pending_notices.extend(notices);
        pending_loaded = Some(AgentEvent::SessionLoaded {
            items: temur::session_store::replay_items(&seed.history),
            notice: summary,
        });
        Some(seed)
    } else {
        None
    };

    // Plain-mode banners keep their exact v1 wording; in TUI mode the same
    // facts live in the header/footer (a pre-alt-screen println would be
    // swallowed anyway). One-shot mode (T14) prints no banner at all:
    // stdout is reserved for the assistant's prose.
    let banner = !use_tui && oneshot.is_none();
    // T18: the credential the startup construction read (None = keyless or
    // mock), registered below for tool-output redaction. Never an extra
    // read: each arm hands over the very string it loaded anyway.
    let mut startup_key: Option<String> = None;
    let provider: Box<dyn Provider> = match &mock {
        Some(paths) => {
            let files: Vec<std::path::PathBuf> =
                paths.split(',').map(std::path::PathBuf::from).collect();
            if banner {
                println!("temur {VERSION} [MOCK replay: {} response(s)]", files.len());
            }
            let replay = Box::new(ReplayTransport::new(files));
            if is_compat {
                Box::new(OpenAiCompatProvider::new(
                    resolved.base_url.clone(),
                    None,
                    resolved.max_tokens_parameter,
                    replay,
                ))
            } else {
                Box::new(AnthropicProvider::new(
                    "https://mock.invalid",
                    "mock-key".into(),
                    replay,
                ))
            }
        }
        None => {
            if banner {
                println!("temur {VERSION} (model={model}, thinking={})", cfg.thinking);
            }
            match &capture {
                // Tee raw SSE bodies to <base>.<n>.sse for the golden
                // conformance fixtures (operator-run, one-time). Credentials
                // here follow the same by-path rule as build_live.
                Some(base) => {
                    // In one-shot mode this status line moves to stderr with
                    // the rest of the chrome.
                    if oneshot.is_some() {
                        eprintln!("[capture-sse: writing raw streams to {base}.<n>.sse]");
                    } else {
                        println!("[capture-sse: writing raw streams to {base}.<n>.sse]");
                    }
                    if is_compat {
                        let key = match &resolved.api_key_file {
                            Some(p) => Some(secret::load_api_key_from(std::path::Path::new(p))?),
                            None => None,
                        };
                        startup_key = key.clone();
                        Box::new(OpenAiCompatProvider::new(
                            resolved.base_url.clone(),
                            key,
                            resolved.max_tokens_parameter,
                            Box::new(temur::provider::transport::CaptureTransport::new(
                                temur::provider::openai_compat::transport::HttpTransport::new(),
                                std::path::PathBuf::from(base),
                            )),
                        ))
                    } else {
                        let key = match &resolved.api_key_file {
                            Some(p) => secret::load_api_key_from(std::path::Path::new(p))?,
                            None => secret::load_api_key()?,
                        };
                        startup_key = Some(key.clone());
                        Box::new(AnthropicProvider::new(
                            resolved.base_url.clone(),
                            key,
                            Box::new(temur::provider::transport::CaptureTransport::new(
                                temur::provider::anthropic::transport::HttpTransport::new(),
                                std::path::PathBuf::from(base),
                            )),
                        ))
                    }
                }
                // The one live construction path, shared with `/model` (T8).
                None => {
                    let (provider, key) = temur::provider::build_live_with_key(&resolved)?;
                    startup_key = key;
                    provider
                }
            }
        }
    };

    // T9: ONE place assembles the system prompt for a given prompt profile —
    // startup and `/model` prompt-profile swaps both call it. The config
    // override wins in EITHER profile; the skills section (advertising
    // installed skills so the model knows the skill tool is worth calling)
    // and {cwd} are captured here. Infallible, so a switch can call it after
    // its provider build already succeeded.
    let rebuild_system = |profile: temur::tools::PromptProfile| -> String {
        let base_system = cfg.system_prompt.clone().unwrap_or_else(|| {
            let default = match profile {
                temur::tools::PromptProfile::Compact => DEFAULT_SYSTEM_COMPACT,
                temur::tools::PromptProfile::Full => DEFAULT_SYSTEM,
            };
            default.replace("{cwd}", &cwd_display)
        });
        match temur::skills::system_prompt_section(&installed_skills) {
            Some(section) => format!("{base_system}{section}"),
            None => base_system,
        }
    };
    let system = rebuild_system(current_prompt_profile);

    let mut session_cfg = SessionConfig::from_config(&cfg, cwd.clone());
    session_cfg.model = model.clone();
    // Profile overrides (T8): identical to the global values when no profile
    // is active — resolve_base copies them through.
    session_cfg.max_tokens = resolved.max_tokens;
    // T16: the truncation notice names where the limit came from.
    session_cfg.max_tokens_source = active_profile.clone();
    // Advisory context awareness: the window is a property of the served
    // model, so it comes from the selection that knows the server.
    session_cfg.context_window = resolved.context_window;
    // T26: the same rates `/status` estimates at, from the same gate, so the
    // mid-session advisory can only fire where the estimate is real. The
    // step is validated HERE, at startup, so a nonsense value is an error
    // before the first prompt rather than an advisory that never fires.
    session_cfg.cost_rates = temur::cost::CostRates::for_profile(&resolved);
    session_cfg.cost_advisory_step_usd = cfg.cost_advisory_step_usd()?;
    // T40: resolved HERE because the default depends on the invocation mode,
    // which only main.rs knows. One-shot -p has nobody to act on a context
    // advisory, so it compacts itself; the REPL and TUI keep the advisory.
    session_cfg.auto_compact = cfg.auto_compact_enabled(oneshot.is_some());
    session_cfg.system = Some(system);
    let registry =
        Registry::standard_with_skills(skill_dirs).with_profile(current_prompt_profile);
    let mut session = match seed {
        Some(seed) => Session::resume(provider, registry, session_cfg, seed),
        None => Session::new(provider, registry, session_cfg),
    };
    // T18: the key-file guard, from the ONE construction rule (active
    // selection + every profile + APP_SECRET_FILE). Empty when the config
    // is keyless, and an empty guard checks nothing. The bash escape hatch
    // travels with it.
    session.set_key_guard(
        temur::tools::KeyGuard::from_selection(&resolved, &profiles),
        cfg.allow_bash_without_key_sandbox,
    );
    // T18 layer 3: the startup credential (already read above, or None)
    // registers for tool-output redaction.
    session.set_redaction_key(startup_key);

    let mut ui: Box<dyn Ui> = if use_tui {
        let tui = TuiUi::new(
            SessionInfo {
                model: model.clone(),
                thinking: cfg.thinking,
                cwd: cwd_display.clone(),
                version: VERSION.to_string(),
                // T9: profile names feed `/model` Tab completion.
                profiles: profiles.keys().cloned().collect(),
                // T16: the clear-on-provider-change baseline for cached ids.
                provider: resolved.provider.clone(),
            },
            // T6: the render thread holds the session's cancel token so
            // Esc can interrupt a running turn.
            session.cancel_token(),
        )?;
        // T21: the TUI is interactive by construction, so it can ask
        // per-command bash approval when the key sandbox is unavailable.
        session.set_bash_approver(tui.bash_approver());
        Box::new(tui)
    } else if oneshot.is_some() {
        // T21: one-shot -p NEVER installs an approver; its Ask arm stays a
        // refusal, terminal or not.
        Box::new(temur::ui::oneshot::OneShotUi::stdio())
    } else {
        // T21: the plain REPL is interactive only on a real terminal;
        // piped runs (the mock e2e suites) stay byte-identical.
        if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
            session.set_bash_approver(temur::ui::repl::stdin_bash_approver());
        }
        Box::new(ReplUi::new())
    };
    // Resume output surfaces through the same seam as everything else, after
    // the UI exists: the replay event first (it rebuilds the TUI transcript),
    // then the advisory notices so they land in the fresh backscroll.
    if let Some(ev) = &pending_loaded {
        ui.event(ev);
    }
    for n in &pending_notices {
        ui.event(&AgentEvent::Notice(n.clone()));
    }
    // T20 resume-time context action: when the RESTORED estimate already
    // crosses the threshold, act now. Resume is the zero-waste moment to
    // compact, and this one call site covers the plain REPL, the TUI, and
    // one-shot -p with --continue/--resume (whose UI routes it to stderr).
    // T40 rider: what "act" means (advise, or compact and continue) is the
    // session's decision, not main.rs's.
    if pending_loaded.is_some() {
        session.resume_seam_context_action(&mut |ev| ui.event(&ev));
    }

    // F4: plain-REPL SIGINT — the first Ctrl+C interrupts the running turn
    // through the same cooperative token as a TUI Esc; the second (while
    // the flag is still set) force-quits with exit 130. Installed ONLY in
    // plain mode: TUI raw mode never generates SIGINT and its Ctrl+C
    // semantics are unchanged.
    if !use_tui {
        temur::signal::install_plain_repl_handler()?;
    }
    // F7: the cancel token is cleared at SUBMISSION by whichever component
    // serializes input. In TUI mode that is the render thread's Submit arm
    // (same thread as Esc — race-free); here it is the plain REPL, right
    // after read_input returns and before the turn is dispatched (the
    // clear also resets the SIGINT flag — F4).
    let plain_cancel = session.cancel_token();
    // T8 command state: what the NEXT save records — a /model switch
    // updates these, so a session saved after switching describes what is
    // actually active (the advisory mismatch notice on a later resume under
    // different config is then correct behavior).
    let mut provider_name = resolved.provider.clone();
    let mut current_model = model.clone();

    // T14 one-shot: exactly ONE full agentic turn (all tool rounds run to
    // completion inside session.turn), through the same session, event, and
    // save seams as the loop below; then exit by outcome. Session saving
    // stays on for live runs, so `temur -p` chains with
    // `temur --continue -p`.
    if let Some(prompt) = &oneshot {
        plain_cancel.clear();
        // T40 P2: the session persists itself mid-turn now, so the target is
        // installed BEFORE the turn rather than passed to the save after it.
        session.set_persist_target(persist_target(
            persist_path.as_ref(),
            &provider_name,
            &current_model,
            &cwd_display,
            session_name.as_deref(),
            session_max_bytes,
        ));
        let result = session.turn(prompt, &mut |ev| ui.event(&ev));
        // Read the token right after the turn, before anything else can
        // touch it: set means a Ctrl+C landed THIS turn (T6 semantics), and
        // the exit code must say so (130) even if an error raced the cancel.
        let interrupted = plain_cancel.is_set();
        if let Err(e) = &result {
            ui.event(&AgentEvent::Notice(format!("provider error: {e}")));
        }
        save_after_turn(&mut session, ui.as_mut());
        ui.finish();
        return Ok(ExitCode::from(temur::ui::oneshot::exit_code(
            result.is_ok(),
            interrupted,
        )));
    }

    let replay_mode = mock.is_some() || capture.is_some();
    // T18: each successful in-loop provider build deposits the key it read
    // here; the loop re-registers it for redaction after the command runs.
    // Written ONLY on success, so a failed switch (which leaves the session
    // unchanged) also leaves the registered key unchanged. `Some(None)`
    // means "switched to a keyless selection: clear".
    let switched_key: std::cell::RefCell<Option<Option<String>>> =
        std::cell::RefCell::new(None);
    let build = |p: &temur::config::ResolvedProfile| {
        let (provider, key) = temur::provider::build_live_with_key(p)?;
        *switched_key.borrow_mut() = Some(key);
        Ok(provider)
    };
    let list_models = |p: &temur::config::ResolvedProfile| temur::provider::list_models_live(p);
    // T15: the file `/model --save` edits — the exact path startup loaded.
    let cfg_path = config::config_path();
    // T16: the driver-loop mirror of the UI's `/models` id cache — the
    // command layer reads it for the raw-id advisory, and (T22) it carries
    // the wire-reported windows too, refreshed by the command layer itself
    // on every listing. Same drop rule as the TUI cache: cleared on a
    // provider change.
    let mut cached_models: Vec<temur::provider::ModelEntry> = Vec::new();
    let mut cached_ids_provider = resolved.provider.clone();
    while let Some(line) = ui.read_input() {
        if !use_tui {
            plain_cancel.clear();
        }
        // T8: any `/`-line is command-space — it never reaches the model or
        // the history. Commands run here, between turns, by construction.
        if line.starts_with('/') {
            let events = {
                let mut cctx = temur::commands::CommandCtx {
                    session: &mut session,
                    profiles: &profiles,
                    active_profile: &mut active_profile,
                    provider_name: &mut provider_name,
                    model: &mut current_model,
                    persist_path: &mut persist_path,
                    session_max_bytes,
                    sessions_dir: &sessions_dir,
                    cwd: &cwd,
                    cwd_display: &cwd_display,
                    session_name: &mut session_name,
                    replay_mode,
                    prompt_profile: &mut current_prompt_profile,
                    active_resolved: &mut active_resolved,
                    config_path: &cfg_path,
                    cached_models: &mut cached_models,
                    build_provider: &build,
                    list_models: &list_models,
                    rebuild_system: &rebuild_system,
                };
                temur::commands::run(temur::commands::parse(&line), &mut cctx)
            };
            // T18: a switch that built a provider re-registers that build's
            // key (the LAST build wins, which is the one now active; the
            // T16 hop builds twice and the second, when it succeeds, is the
            // active override).
            if let Some(key) = switched_key.borrow_mut().take() {
                session.set_redaction_key(key);
            }
            for ev in &events {
                // The listing cache itself is refreshed inside the command
                // layer (T22); the loop only enforces the drop rule.
                if let AgentEvent::ModelSwitched { provider, .. } = ev {
                    if *provider != cached_ids_provider {
                        cached_models.clear();
                        cached_ids_provider = provider.clone();
                    }
                }
                ui.event(ev);
            }
            continue;
        }
        // T40 P2: refreshed every turn, so a `/model`, `/resume`, or `/new`
        // between turns is reflected in what the mid-turn writes record.
        session.set_persist_target(persist_target(
            persist_path.as_ref(),
            &provider_name,
            &current_model,
            &cwd_display,
            session_name.as_deref(),
            session_max_bytes,
        ));
        if let Err(e) = session.turn(&line, &mut |ev| ui.event(&ev)) {
            // Provider-level failure: surface through the UI seam and keep
            // the session alive. (Behavior note, docs/TUI.md: in the plain
            // REPL this line moved from stderr to stdout with M-B.)
            ui.event(&AgentEvent::Notice(format!("provider error: {e}")));
        }
        // Save in BOTH arms — power-cut philosophy: a provider-error turn's
        // dangling user message is real history, and the resume seam is what
        // handles it (prepare_seed drops a trailing unanswered prompt).
        save_after_turn(&mut session, ui.as_mut());
    }
    drop(ui); // TUI: joins the render thread and restores the terminal
    if !use_tui {
        println!("bye");
    }
    Ok(ExitCode::SUCCESS)
}

/// T40 P2: the persist target for the NEXT turn, from the state main.rs
/// owns and mutates between turns (`/model`, `/resume`, `/new`). `None`
/// whenever there is no session file, which is what `--mock` gets.
#[allow(clippy::too_many_arguments)]
fn persist_target(
    persist_path: Option<&std::path::PathBuf>,
    provider_name: &str,
    model: &str,
    cwd_display: &str,
    session_name: Option<&str>,
    session_max_bytes: u64,
) -> Option<temur::agent::PersistTarget> {
    persist_path.map(|path| temur::agent::PersistTarget {
        path: path.clone(),
        provider: provider_name.to_string(),
        model: model.to_string(),
        cwd_display: cwd_display.to_string(),
        name: session_name.map(str::to_string),
        max_bytes: session_max_bytes,
    })
}

/// Post-turn session save, shared by the REPL loop and one-shot mode (T14).
/// Never fatal: the in-memory conversation is intact and every later turn
/// retries. The failure is noticed once per process so a full disk doesn't
/// shout on every turn.
///
/// T40 P2: the write itself moved into the session, which now also writes
/// mid-turn. This stays as the FINAL save so behaviour at turn end is
/// unchanged, and it shares the session's once-per-process failure latch
/// rather than keeping a second one.
fn save_after_turn(session: &mut Session, ui: &mut dyn temur::ui::Ui) {
    session.persist_now(&mut |ev| ui.event(&ev));
}

/// First-run guidance (T14), printed to stderr instead of the raw credential
/// error when no config file exists. Short by design: the path that was
/// looked for, the command that creates a starter config, and where the
/// docs are.
fn quickstart_text() -> String {
    format!(
        "temur: no config file found\n\
         \n\
         Looked for: {}\n\
         \n\
         The default provider (anthropic) needs an API key file, so a first\n\
         run has nothing to talk to yet. To get started:\n\
         \n\
           temur init      create a starter config (local llama.cpp/Ollama,\n\
                           Anthropic, OpenAI, Gemini, or xAI)\n\
           temur doctor    check the config and environment\n\
         \n\
         Config format and recipes: README.md, section \"Configure\".\n\
         Local model picks: docs/OFFLINE.md, section \"Recommended small models\".\n\
         Conversations are saved automatically per working directory;\n\
         temur --continue resumes the last one.\n",
        config::config_path().display()
    )
}

/// M0 prove-it gate: a real rustls(ring)+webpki-roots TLS handshake on the
/// i686 target, against a neutral endpoint. Never the Anthropic API from the
/// build environment.
fn tls_probe() -> Result<(), error::Error> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| error::Error::Tls("failed to install ring CryptoProvider".into()))?;
    let url = "https://crates.io/";
    let status = match ureq::get(url).call() {
        Ok(res) => res.status().as_u16(),
        Err(ureq::Error::StatusCode(code)) => code,
        Err(e) => {
            return Err(error::Error::Tls(format!(
                "handshake/request to {url} failed: {e}"
            )))
        }
    };
    println!("tls-probe OK: GET {url} -> HTTP {status} (rustls/ring + webpki-roots)");
    Ok(())
}
