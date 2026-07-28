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
When you edit files, verify your changes. The current working directory is: {cwd}";

/// Shorter default system prompt used when `prompt_profile` is `"compact"`
/// AND no config `system_prompt` override exists — an explicit override
/// always wins, in either profile.
const DEFAULT_SYSTEM_COMPACT: &str = "You are temur, a coding agent in a terminal. Act through \
the provided tools; always call them with valid JSON arguments — never write a tool call as \
plain text. Prefer tools over guessing, keep answers short, verify edits. \
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
            Value(v) if cmd.is_none() => cmd = Some(v.string()?),
            arg => return Err(arg.unexpected().into()),
        }
    }
    if force && cmd.as_deref() != Some("init") {
        return Err(error::Error::Usage(
            "--force is only valid with the init subcommand".into(),
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

    match cmd.as_deref() {
        Some("init") => {
            let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
            temur::init::run(
                &config::config_path(),
                home.as_deref(),
                force,
                &mut std::io::stdin().lock(),
                &mut std::io::stdout(),
            )?;
            Ok(ExitCode::SUCCESS)
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
    cfg.prompt_profile()?;
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
    let provider: Box<dyn Provider> = match &mock {
        Some(paths) => {
            let files: Vec<std::path::PathBuf> =
                paths.split(',').map(std::path::PathBuf::from).collect();
            if banner {
                println!("temur {VERSION} [MOCK replay: {} response(s)]", files.len());
            }
            let replay = Box::new(ReplayTransport::new(files));
            if is_compat {
                Box::new(OpenAiCompatProvider::new(resolved.base_url.clone(), None, replay))
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
                        Box::new(OpenAiCompatProvider::new(
                            resolved.base_url.clone(),
                            key,
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
                None => temur::provider::build_live(&resolved)?,
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
    // Advisory context awareness: the window is a property of the served
    // model, so it comes from the selection that knows the server.
    session_cfg.context_window = resolved.context_window;
    session_cfg.system = Some(system);
    let registry =
        Registry::standard_with_skills(skill_dirs).with_profile(current_prompt_profile);
    let mut session = match seed {
        Some(seed) => Session::resume(provider, registry, session_cfg, seed),
        None => Session::new(provider, registry, session_cfg),
    };

    let mut ui: Box<dyn Ui> = if use_tui {
        Box::new(TuiUi::new(
            SessionInfo {
                model: model.clone(),
                thinking: cfg.thinking,
                cwd: cwd_display.clone(),
                version: VERSION.to_string(),
                // T9: profile names feed `/model` Tab completion.
                profiles: profiles.keys().cloned().collect(),
            },
            // T6: the render thread holds the session's cancel token so
            // Esc can interrupt a running turn.
            session.cancel_token(),
        )?)
    } else if oneshot.is_some() {
        Box::new(temur::ui::oneshot::OneShotUi::stdio())
    } else {
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
    let mut save_failure_notified = false;
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
        let result = session.turn(prompt, &mut |ev| ui.event(&ev));
        if let Err(e) = &result {
            ui.event(&AgentEvent::Notice(format!("provider error: {e}")));
        }
        save_after_turn(
            persist_path.as_ref(),
            &session,
            &provider_name,
            &current_model,
            &cwd_display,
            session_name.as_deref(),
            session_max_bytes,
            ui.as_mut(),
            &mut save_failure_notified,
        );
        ui.finish();
        return Ok(if result.is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }

    let replay_mode = mock.is_some() || capture.is_some();
    let build = |p: &temur::config::ResolvedProfile| temur::provider::build_live(p);
    let list_models = |p: &temur::config::ResolvedProfile| temur::provider::list_models_live(p);
    while let Some(line) = ui.read_input() {
        if !use_tui {
            plain_cancel.clear();
        }
        // T8: any `/`-line is command-space — it never reaches the model or
        // the history. Commands run here, between turns, by construction.
        if line.starts_with('/') {
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
                build_provider: &build,
                list_models: &list_models,
                rebuild_system: &rebuild_system,
            };
            for ev in temur::commands::run(temur::commands::parse(&line), &mut cctx) {
                ui.event(&ev);
            }
            continue;
        }
        if let Err(e) = session.turn(&line, &mut |ev| ui.event(&ev)) {
            // Provider-level failure: surface through the UI seam and keep
            // the session alive. (Behavior note, docs/TUI.md: in the plain
            // REPL this line moved from stderr to stdout with M-B.)
            ui.event(&AgentEvent::Notice(format!("provider error: {e}")));
        }
        // Save in BOTH arms — power-cut philosophy: a provider-error turn's
        // dangling user message is real history, and the resume seam is what
        // handles it (prepare_seed drops a trailing unanswered prompt).
        save_after_turn(
            persist_path.as_ref(),
            &session,
            &provider_name,
            &current_model,
            &cwd_display,
            session_name.as_deref(),
            session_max_bytes,
            ui.as_mut(),
            &mut save_failure_notified,
        );
    }
    drop(ui); // TUI: joins the render thread and restores the terminal
    if !use_tui {
        println!("bye");
    }
    Ok(ExitCode::SUCCESS)
}

/// Post-turn session save, shared by the REPL loop and one-shot mode (T14).
/// Never fatal: the in-memory conversation is intact and every later turn
/// retries. The failure is noticed once per process so a full disk doesn't
/// shout on every turn.
#[allow(clippy::too_many_arguments)]
fn save_after_turn(
    persist_path: Option<&std::path::PathBuf>,
    session: &Session,
    provider_name: &str,
    model: &str,
    cwd_display: &str,
    session_name: Option<&str>,
    session_max_bytes: u64,
    ui: &mut dyn Ui,
    save_failure_notified: &mut bool,
) {
    let Some(path) = persist_path else { return };
    let snap = session.snapshot();
    let file = temur::session_store::SessionFileRef {
        version: temur::session_store::FORMAT_VERSION,
        provider: provider_name,
        model,
        cwd: cwd_display,
        history: snap.history,
        session_usage: snap.session_usage,
        todos: snap.todos,
        last_context_used: snap.last_context_used,
        name: session_name,
    };
    let mut trim_notices: Vec<String> = Vec::new();
    match temur::session_store::save(path, &file, session_max_bytes, &mut |n| {
        trim_notices.push(n)
    }) {
        Ok(()) => {
            for n in trim_notices {
                ui.event(&AgentEvent::Notice(n));
            }
        }
        Err(e) => {
            if !*save_failure_notified {
                *save_failure_notified = true;
                ui.event(&AgentEvent::Notice(format!(
                    "session save failed: {e} — continuing; will retry next turn"
                )));
            }
        }
    }
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
                           Anthropic, OpenAI, or Gemini)\n\
           temur doctor    check the config and environment\n\
         \n\
         Config format and recipes: README.md, section \"Configure\".\n",
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
