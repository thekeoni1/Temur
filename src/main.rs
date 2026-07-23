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
    // UI selection: --tui / --plain force it; default is TUI on a real
    // terminal, plain line REPL otherwise (so piped/scripted use — the mock
    // e2e, operator scripts — is unchanged without any flag).
    let mut force_tui = false;
    let mut force_plain = false;
    while let Some(arg) = parser.next()? {
        match arg {
            Long("version") | Short('V') => {
                println!("temur {VERSION}");
                return Ok(ExitCode::SUCCESS);
            }
            Long("mock") => mock = Some(parser.value()?.string()?),
            Long("capture-sse") => capture = Some(parser.value()?.string()?),
            Long("continue") => resume = true,
            Long("tui") => force_tui = true,
            Long("plain") => force_plain = true,
            Value(v) if cmd.is_none() => cmd = Some(v.string()?),
            arg => return Err(arg.unexpected().into()),
        }
    }
    if force_tui && force_plain {
        return Err(error::Error::Usage("--tui and --plain are mutually exclusive".into()));
    }
    // Persistence is disabled under --mock (fixtures must never touch real
    // state), so a --continue there could only ever mislead.
    if resume && mock.is_some() {
        return Err(error::Error::Usage("--continue is unavailable with --mock".into()));
    }
    let use_tui = if force_plain {
        false
    } else if force_tui {
        true
    } else {
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
    };

    match cmd.as_deref() {
        Some("tls-probe") => {
            tls_probe()?;
            Ok(ExitCode::SUCCESS)
        }
        Some("tui-probe") => {
            temur::ui::tui::probe()?;
            Ok(ExitCode::SUCCESS)
        }
        Some(other) => Err(error::Error::Usage(format!("unknown command: {other}"))),
        None => repl(mock, capture, use_tui, resume),
    }
}

fn repl(
    mock: Option<String>,
    capture: Option<String>,
    use_tui: bool,
    resume: bool,
) -> Result<ExitCode, error::Error> {
    let cfg = config::Config::load()?;
    // Validated up front: an unknown prompt_profile is a startup error, not
    // a silent fallback.
    let prompt_profile = cfg.prompt_profile()?;
    let cwd = std::env::current_dir()?;

    // Session persistence (T5), resolved up front so a bad cap is a startup
    // error, not a mid-session surprise. Default-on for live runs; disabled
    // under --mock — fixtures must never touch real state. --capture-sse
    // runs keep it on. Nothing is written here: a fresh run only overwrites
    // this directory's file on its FIRST SAVE, so launching and quitting
    // without a turn never destroys a resumable session.
    let session_max_bytes = cfg.session_max_bytes()?;
    let persist_path = if mock.is_none() {
        Some(temur::session_store::session_path(
            &temur::session_store::sessions_dir(cfg.sessions_dir.as_deref()),
            &cwd,
        ))
    } else {
        None
    };

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

    // Provider selection (T2). The default stays anthropic — selecting
    // "openai-compat" is a config change, never inferred. --mock replays
    // fixtures through the SELECTED provider, so the selection path itself
    // is exercised offline.
    let openai_cfg = match cfg.provider.as_str() {
        "anthropic" => None,
        "openai-compat" => {
            let oc = cfg.openai_compat.clone().unwrap_or_default();
            if oc.model.is_empty() {
                return Err(error::Error::Config(
                    "provider \"openai-compat\" requires openai_compat.model".into(),
                ));
            }
            Some(oc)
        }
        other => {
            return Err(error::Error::Config(format!(
                "unknown provider {other:?} (expected \"anthropic\" or \"openai-compat\")"
            )))
        }
    };
    let model = openai_cfg
        .as_ref()
        .map(|oc| oc.model.clone())
        .unwrap_or_else(|| cfg.model.clone());
    let cwd_display = cwd.display().to_string();

    // --continue: load BEFORE provider construction — "you have no session
    // to resume" should not hide behind a credential error — and FAIL FAST
    // on a missing, corrupt, or wrong-version file: never silently start a
    // fresh session over the very file the user asked to resume. Notices are
    // collected here but emitted only after UI construction (the TUI
    // swallows pre-alt-screen printlns).
    let mut pending_notices: Vec<String> = Vec::new();
    let seed = if resume {
        let path = persist_path
            .as_ref()
            .expect("--continue with --mock is rejected at argument parsing");
        let file = match temur::session_store::load(path) {
            Ok(f) => f,
            Err(e @ temur::session_store::StoreError::Missing { .. }) => {
                return Err(error::Error::Session(format!(
                    "{e} — run without --continue to start one"
                )))
            }
            Err(e) => return Err(e.into()),
        };
        pending_notices.extend(temur::session_store::mismatch_notices(
            &file,
            &cfg.provider,
            &model,
            &cwd_display,
        ));
        let (seed, notices) = temur::session_store::prepare_seed(file);
        pending_notices.extend(notices);
        Some(seed)
    } else {
        None
    };

    // Plain-mode banners keep their exact v1 wording; in TUI mode the same
    // facts live in the header/footer (a pre-alt-screen println would be
    // swallowed anyway).
    let provider: Box<dyn Provider> = match &mock {
        Some(paths) => {
            let files: Vec<std::path::PathBuf> =
                paths.split(',').map(std::path::PathBuf::from).collect();
            if !use_tui {
                println!("temur {VERSION} [MOCK replay: {} response(s)]", files.len());
            }
            let replay = Box::new(ReplayTransport::new(files));
            match &openai_cfg {
                Some(oc) => Box::new(OpenAiCompatProvider::new(oc.base_url.clone(), None, replay)),
                None => Box::new(AnthropicProvider::new(
                    "https://mock.invalid",
                    "mock-key".into(),
                    replay,
                )),
            }
        }
        None => match &openai_cfg {
            Some(oc) => {
                // Keyless is first-class for local endpoints; a keyed
                // endpoint reads its credential BY PATH from config — the
                // same isolation rule as APP_SECRET_FILE, never env/argv.
                let key = match &oc.api_key_file {
                    Some(p) => Some(secret::load_api_key_from(std::path::Path::new(p))?),
                    None => None,
                };
                if !use_tui {
                    println!("temur {VERSION} (model={model}, thinking={})", cfg.thinking);
                }
                match &capture {
                    Some(base) => {
                        println!("[capture-sse: writing raw streams to {base}.<n>.sse]");
                        Box::new(OpenAiCompatProvider::new(
                            oc.base_url.clone(),
                            key,
                            Box::new(temur::provider::transport::CaptureTransport::new(
                                temur::provider::openai_compat::transport::HttpTransport::new(),
                                std::path::PathBuf::from(base),
                            )),
                        ))
                    }
                    None => Box::new(OpenAiCompatProvider::with_http(oc.base_url.clone(), key)),
                }
            }
            None => {
                // Credential comes BY PATH via APP_SECRET_FILE (appsvc launcher).
                // Deliberately never read from ANTHROPIC_API_KEY.
                let key = secret::load_api_key()?;
                if !use_tui {
                    println!("temur {VERSION} (model={model}, thinking={})", cfg.thinking);
                }
                match &capture {
                    Some(base) => {
                        // Tee raw SSE bodies to <base>.<n>.sse for the golden
                        // conformance fixtures (operator-run, one-time).
                        println!("[capture-sse: writing raw streams to {base}.<n>.sse]");
                        Box::new(AnthropicProvider::new(
                            cfg.base_url.clone(),
                            key,
                            Box::new(temur::provider::transport::CaptureTransport::new(
                                temur::provider::anthropic::transport::HttpTransport::new(),
                                std::path::PathBuf::from(base),
                            )),
                        ))
                    }
                    None => Box::new(AnthropicProvider::with_http(cfg.base_url.clone(), key)),
                }
            }
        },
    };

    let base_system = cfg.system_prompt.clone().unwrap_or_else(|| {
        let default = match prompt_profile {
            temur::tools::PromptProfile::Compact => DEFAULT_SYSTEM_COMPACT,
            temur::tools::PromptProfile::Full => DEFAULT_SYSTEM,
        };
        default.replace("{cwd}", &cwd.display().to_string())
    });
    // Advertise installed skills so the model knows the skill tool is worth
    // calling; nothing appended when no skills are installed.
    let system = match temur::skills::system_prompt_section(&installed_skills) {
        Some(section) => format!("{base_system}{section}"),
        None => base_system,
    };

    let mut session_cfg = SessionConfig::from_config(&cfg, cwd);
    session_cfg.model = model.clone();
    // Advisory context awareness: the window is a property of the served
    // model, so it comes from the openai_compat section (None elsewhere).
    session_cfg.context_window = openai_cfg.as_ref().and_then(|oc| oc.context_window);
    session_cfg.system = Some(system);
    let registry = Registry::standard_with_skills(skill_dirs).with_profile(prompt_profile);
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
            },
            // T6: the render thread holds the session's cancel token so
            // Esc can interrupt a running turn.
            session.cancel_token(),
        )?)
    } else {
        Box::new(ReplUi::new())
    };
    // Resume notices (mismatches, dangling-prompt drop, the summary line)
    // surface through the same seam as everything else, after the UI exists.
    for n in &pending_notices {
        ui.event(&AgentEvent::Notice(n.clone()));
    }

    // F7: the cancel token is cleared at SUBMISSION by whichever component
    // serializes input. In TUI mode that is the render thread's Submit arm
    // (same thread as Esc — race-free); here it is the plain REPL, right
    // after read_input returns and before the turn is dispatched.
    let plain_cancel = session.cancel_token();
    let mut save_failure_notified = false;
    while let Some(line) = ui.read_input() {
        if !use_tui {
            plain_cancel.clear();
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
        if let Some(path) = &persist_path {
            let snap = session.snapshot();
            let file = temur::session_store::SessionFileRef {
                version: temur::session_store::FORMAT_VERSION,
                provider: &cfg.provider,
                model: &model,
                cwd: &cwd_display,
                history: snap.history,
                session_usage: snap.session_usage,
                todos: snap.todos,
                last_context_used: snap.last_context_used,
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
                    // Never fatal: the in-memory conversation is intact and
                    // every later turn retries. Noticed once per process so a
                    // full disk doesn't shout on every turn.
                    if !save_failure_notified {
                        save_failure_notified = true;
                        ui.event(&AgentEvent::Notice(format!(
                            "session save failed: {e} — continuing; will retry next turn"
                        )));
                    }
                }
            }
        }
    }
    drop(ui); // TUI: joins the render thread and restores the terminal
    if !use_tui {
        println!("bye");
    }
    Ok(ExitCode::SUCCESS)
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
