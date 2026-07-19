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
            Long("tui") => force_tui = true,
            Long("plain") => force_plain = true,
            Value(v) if cmd.is_none() => cmd = Some(v.string()?),
            arg => return Err(arg.unexpected().into()),
        }
    }
    if force_tui && force_plain {
        return Err(error::Error::Usage("--tui and --plain are mutually exclusive".into()));
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
        None => repl(mock, capture, use_tui),
    }
}

fn repl(
    mock: Option<String>,
    capture: Option<String>,
    use_tui: bool,
) -> Result<ExitCode, error::Error> {
    let cfg = config::Config::load()?;
    let cwd = std::env::current_dir()?;

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

    let base_system = cfg
        .system_prompt
        .clone()
        .unwrap_or_else(|| DEFAULT_SYSTEM.replace("{cwd}", &cwd.display().to_string()));
    // Advertise installed skills so the model knows the skill tool is worth
    // calling; nothing appended when no skills are installed.
    let system = match temur::skills::system_prompt_section(&installed_skills) {
        Some(section) => format!("{base_system}{section}"),
        None => base_system,
    };

    let cwd_display = cwd.display().to_string();
    let mut session_cfg = SessionConfig::from_config(&cfg, cwd);
    session_cfg.model = model.clone();
    session_cfg.system = Some(system);
    let mut session = Session::new(
        provider,
        Registry::standard_with_skills(skill_dirs),
        session_cfg,
    );

    let mut ui: Box<dyn Ui> = if use_tui {
        Box::new(TuiUi::new(SessionInfo {
            model: model.clone(),
            thinking: cfg.thinking,
            cwd: cwd_display,
            version: VERSION.to_string(),
        })?)
    } else {
        Box::new(ReplUi::new())
    };
    while let Some(line) = ui.read_input() {
        if let Err(e) = session.turn(&line, &mut |ev| ui.event(&ev)) {
            // Provider-level failure: surface through the UI seam and keep
            // the session alive. (Behavior note, docs/TUI.md: in the plain
            // REPL this line moved from stderr to stdout with M-B.)
            ui.event(&AgentEvent::Notice(format!("provider error: {e}")));
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
