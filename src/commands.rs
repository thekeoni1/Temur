//! T8 slash commands: parse + dispatch, UI-free. Any input line starting
//! with `/` is command-space — it never reaches the model or the history
//! (which also means a literal leading-slash MESSAGE cannot be sent; a
//! documented limitation). Commands only exist between turns by
//! construction: input is read only while the agent is at the prompt.
//!
//! All user feedback flows out as [`AgentEvent`]s, so both UIs render
//! commands with zero UI-specific code here: `Notice` carries every
//! human-readable line; `ModelSwitched` / `ThinkingChanged` /
//! `SessionCleared` are chrome/state signals a UI may fold silently.

use crate::agent::events::AgentEvent;
use crate::agent::Session;
use crate::config::ResolvedProfile;
use crate::provider::Provider;
use crate::session_store;
use crate::tools::PromptProfile;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Help,
    Status,
    Clear,
    /// `/thinking` with no argument: report the current state.
    ThinkingShow,
    ThinkingSet(bool),
    /// `/model` with no argument: list profiles.
    ModelList,
    ModelSwitch(String),
    /// `/model --save` (T15): persist the CURRENT model to the config file.
    ModelSaveCurrent,
    /// `/model <raw-id> --save` (T15): raw switch, then persist on success.
    ModelSwitchSave(String),
    /// `/models`: list model ids from the active provider (T9).
    ModelsList,
    /// `/sessions`: list every saved session, all projects (T10).
    SessionsList,
    /// `/resume <key>`: switch to a saved session (T10).
    Resume(String),
    /// `/new <name>`: start a fresh named session for this project (T10).
    New(String),
    /// Recognized command, unusable arguments; the payload is the notice.
    Invalid(String),
    /// Not a command at all (also a bare `/`).
    Unknown(String),
}

/// Parse one command line (the caller guarantees the leading `/`).
/// Exact lowercase command words, whitespace-tolerant.
pub fn parse(line: &str) -> Command {
    let mut words = line.split_whitespace();
    let head = words.next().unwrap_or("/");
    let arg = words.next();
    let extra = words.next();
    let fourth = words.next();
    match (head, arg, extra) {
        ("/help", None, _) => Command::Help,
        ("/status", None, _) => Command::Status,
        ("/clear", None, _) => Command::Clear,
        ("/thinking", None, _) => Command::ThinkingShow,
        ("/thinking", Some("on"), None) => Command::ThinkingSet(true),
        ("/thinking", Some("off"), None) => Command::ThinkingSet(false),
        ("/thinking", ..) => Command::Invalid("usage: /thinking [on|off]".into()),
        ("/model", None, _) => Command::ModelList,
        ("/model", Some("--save"), None) => Command::ModelSaveCurrent,
        ("/model", Some(name), None) => Command::ModelSwitch(name.to_string()),
        ("/model", Some(name), Some("--save")) if fourth.is_none() && name != "--save" => {
            Command::ModelSwitchSave(name.to_string())
        }
        ("/model", ..) => {
            Command::Invalid("usage: /model [<profile>|<model-id>] [--save]".into())
        }
        ("/models", None, _) => Command::ModelsList,
        ("/sessions", None, _) => Command::SessionsList,
        ("/resume", Some(key), None) => Command::Resume(key.to_string()),
        ("/resume", ..) => Command::Invalid("usage: /resume <session>".into()),
        ("/new", Some(name), None) => Command::New(name.to_string()),
        ("/new", ..) => Command::Invalid("usage: /new <name>".into()),
        ("/help" | "/status" | "/clear" | "/models" | "/sessions", Some(_), _) => {
            Command::Invalid(format!("{head} takes no arguments"))
        }
        _ => Command::Unknown(head.to_string()),
    }
}

/// Everything [`run`] needs, borrowed fresh from the driver loop per
/// command. `build_provider` is injected so the replay/capture decision
/// stays in main and tests can script construction failures.
pub struct CommandCtx<'a> {
    pub session: &'a mut Session,
    /// Eagerly validated at startup: a lookup hit here is always buildable
    /// modulo credential/IO problems.
    pub profiles: &'a BTreeMap<String, ResolvedProfile>,
    /// Active profile name; `None` = running on the base config.
    pub active_profile: &'a mut Option<String>,
    /// What the NEXT session save records — updated on switch so a save
    /// after `/model` describes what is actually active.
    pub provider_name: &'a mut String,
    pub model: &'a mut String,
    /// `None` = persistence disabled (`--mock`). Mutable since T10:
    /// `/resume` and `/new` REDIRECT where the driver loop saves — the
    /// pointer they update is the same local the loop reads next turn.
    pub persist_path: &'a mut Option<std::path::PathBuf>,
    pub session_max_bytes: u64,
    /// The sessions directory (T10), resolved once at startup.
    pub sessions_dir: &'a Path,
    /// The real working directory (T10) — named-session filenames hash its
    /// canonicalized form, so the PATH is needed, not just the display.
    pub cwd: &'a Path,
    pub cwd_display: &'a str,
    /// The live session's name (T10); `None` = the default session. What
    /// the NEXT save records, like `provider_name`/`model` above.
    pub session_name: &'a mut Option<String>,
    /// `--mock` / `--capture-sse`: state-mutating commands are disabled to
    /// keep fixture determinism.
    pub replay_mode: bool,
    /// The ACTIVE prompt profile (T9). A main-loop local like `model`:
    /// updated here on a switch so `/status` and the next switch's
    /// differs-check read what is actually live.
    pub prompt_profile: &'a mut PromptProfile,
    /// The FULL resolved selection currently active (T9) — endpoint,
    /// credential path, limits, model — updated on every successful switch.
    /// `/models` lists against it; a raw-id `/model` switch derives its
    /// target from it (same provider, only the model replaced).
    pub active_resolved: &'a mut ResolvedProfile,
    /// The config FILE `--save` edits (T15), threaded from main. The path
    /// only — reading and writing it is [`crate::config::persist_model`]'s
    /// job, and nothing else here touches the file.
    pub config_path: &'a Path,
    /// Model ids cached from the last `/models` listing (T16), threaded
    /// read-only from the driver loop, which keeps them in sync with the UI
    /// cache (refreshed on every listing, cleared when a switch changes the
    /// provider). Empty = no usable listing: every advisory stays silent.
    pub cached_model_ids: &'a [String],
    #[allow(clippy::type_complexity)]
    pub build_provider:
        &'a dyn Fn(&ResolvedProfile) -> Result<Box<dyn Provider>, crate::error::Error>,
    /// The `/models` listing GET, injected like `build_provider` so the
    /// live/network decision stays in main and tests script results.
    #[allow(clippy::type_complexity)]
    pub list_models:
        &'a dyn Fn(&ResolvedProfile) -> Result<Vec<String>, crate::error::Error>,
    /// Assembles the full system prompt for a prompt profile (T9). Injected
    /// from main — the default-prompt consts, the config override rule, the
    /// skills section, and `{cwd}` all stay there. Infallible, so a switch
    /// stays atomic: it runs only after the provider build succeeded.
    pub rebuild_system: &'a dyn Fn(PromptProfile) -> String,
}

fn notice(s: impl Into<String>) -> AgentEvent {
    AgentEvent::Notice(s.into())
}

fn onoff(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

fn profile_word(p: PromptProfile) -> &'static str {
    match p {
        PromptProfile::Full => "full",
        PromptProfile::Compact => "compact",
    }
}

/// The machine-readable command table (T9): `(name, argument hint, help)`.
/// `/help`, the TUI status-row hint, and Tab completion all derive from it;
/// [`parse`]'s match stays the authority for argument shapes.
pub const COMMANDS: &[(&str, &str, &str)] = &[
    ("/help", "", "this list"),
    (
        "/status",
        "",
        "profile, provider, model, thinking, prompt, context, session file",
    ),
    (
        "/model",
        "[<profile>|<model-id>] [--save]",
        "list profiles · switch to a profile or a raw model id (--save persists it)",
    ),
    ("/models", "", "list model ids from the active provider"),
    ("/clear", "", "wipe this session's history and start fresh"),
    ("/sessions", "", "list saved sessions (all projects)"),
    (
        "/resume",
        "<session>",
        "switch to a saved session (name or file-name prefix)",
    ),
    ("/new", "<name>", "start a fresh named session for this project"),
    (
        "/thinking",
        "[on|off]",
        "show · flip adaptive thinking (this session)",
    ),
];

/// Tab-completion candidates (T9; session keys T10), returned as FULL input
/// lines, in a stable order. Exactly four things complete: command names
/// (while the head word is still being typed), `/model` arguments (profile
/// names first, then cached model ids, deduplicated), `/resume` arguments
/// (session keys cached from the last `/sessions` listing), and `/thinking`
/// arguments (on|off). Nothing else completes — a `/new` name is by
/// definition something that does not exist yet. Pure — the TUI owns the
/// cycle state.
pub fn complete(
    input: &str,
    profiles: &[String],
    model_ids: &[String],
    session_keys: &[String],
) -> Vec<String> {
    if !input.starts_with('/') {
        return vec![];
    }
    // Still inside the head word: complete command names.
    if !input.contains(char::is_whitespace) {
        return COMMANDS
            .iter()
            .filter(|(name, ..)| name.starts_with(input))
            .map(|(name, ..)| name.to_string())
            .collect();
    }
    let mut words = input.split_whitespace();
    let head = words.next().unwrap_or("/");
    let partial = words.next().unwrap_or("");
    if words.next().is_some() {
        return vec![]; // already past the one completable argument
    }
    let args: Vec<&str> = match head {
        "/model" => {
            let mut args: Vec<&str> = profiles.iter().map(String::as_str).collect();
            for id in model_ids {
                if !args.contains(&id.as_str()) {
                    args.push(id);
                }
            }
            args
        }
        "/resume" => session_keys.iter().map(String::as_str).collect(),
        "/thinking" => vec!["on", "off"],
        _ => return vec![],
    };
    args.into_iter()
        .filter(|a| a.starts_with(partial))
        .map(|a| format!("{head} {a}"))
        .collect()
}

/// `/help` body: one line per [`COMMANDS`] row, plus the non-command exit line.
pub fn help_lines() -> Vec<String> {
    let mut out: Vec<String> = COMMANDS
        .iter()
        .map(|(name, arg, help)| {
            if arg.is_empty() {
                format!("{name} — {help}")
            } else {
                format!("{name} {arg} — {help}")
            }
        })
        .collect();
    out.push("exit or quit — leave".into());
    out
}

pub fn run(cmd: Command, ctx: &mut CommandCtx) -> Vec<AgentEvent> {
    match cmd {
        Command::Help => help_lines().into_iter().map(notice).collect(),
        Command::Status => status(ctx),
        Command::Clear => clear(ctx),
        Command::ThinkingShow => vec![notice(format!(
            "thinking: {}",
            onoff(ctx.session.thinking())
        ))],
        Command::ThinkingSet(on) => thinking_set(ctx, on),
        Command::ModelList => model_list(ctx),
        Command::ModelSwitch(name) => model_switch(ctx, name),
        Command::ModelSaveCurrent => model_save_current(ctx),
        Command::ModelSwitchSave(id) => model_switch_save(ctx, id),
        Command::ModelsList => models_list(ctx),
        Command::SessionsList => sessions_list(ctx),
        Command::Resume(key) => resume_session(ctx, key),
        Command::New(name) => new_session(ctx, name),
        Command::Invalid(msg) => vec![notice(msg)],
        Command::Unknown(cmd) => vec![notice(format!(
            "unknown command {cmd:?} — /help lists commands"
        ))],
    }
}

/// Session facts only — never key material, never key file contents.
fn status(ctx: &mut CommandCtx) -> Vec<AgentEvent> {
    let s = &*ctx.session;
    vec![
        notice(format!(
            "profile: {}",
            ctx.active_profile.as_deref().unwrap_or("(none — base config)")
        )),
        notice(format!(
            "provider: {} · model: {}",
            ctx.provider_name, ctx.model
        )),
        notice(format!(
            "thinking: {} · max_tokens: {} · prompt: {}",
            onoff(s.thinking()),
            s.max_tokens(),
            profile_word(*ctx.prompt_profile)
        )),
        notice(match (s.context_window(), s.last_context_used()) {
            (Some(w), Some(u)) => format!("context: ~{u} of {w} tokens used"),
            (None, Some(u)) => format!("context: ~{u} tokens used (window size unknown)"),
            _ => "context: no usage reported yet".into(),
        }),
        notice(match ctx.persist_path.as_deref() {
            Some(p) => format!(
                "session file: {} · session: {}",
                p.display(),
                ctx.session_name.as_deref().unwrap_or("(default)")
            ),
            None => "session file: persistence disabled (--mock)".into(),
        }),
    ]
}

fn clear(ctx: &mut CommandCtx) -> Vec<AgentEvent> {
    if ctx.replay_mode {
        return vec![notice("/clear is unavailable in replay/capture mode")];
    }
    ctx.session.clear_history();
    let mut out = vec![AgentEvent::SessionCleared, notice("session cleared")];
    // Persist the emptied session NOW: quit-then---continue must resume
    // empty, never resurrect the pre-clear file.
    if let Some(path) = ctx.persist_path.as_deref() {
        let snap = ctx.session.snapshot();
        let file = session_store::SessionFileRef {
            version: session_store::FORMAT_VERSION,
            provider: ctx.provider_name,
            model: ctx.model,
            cwd: ctx.cwd_display,
            history: snap.history,
            session_usage: snap.session_usage,
            todos: snap.todos,
            last_context_used: snap.last_context_used,
            name: ctx.session_name.as_deref(),
        };
        if let Err(e) = session_store::save(path, &file, ctx.session_max_bytes, &mut |_| {}) {
            out.push(notice(format!(
                "session save failed: {e} — the cleared state is not on disk yet"
            )));
        }
    }
    out
}

fn thinking_set(ctx: &mut CommandCtx, on: bool) -> Vec<AgentEvent> {
    if ctx.replay_mode {
        return vec![notice("/thinking is unavailable in replay/capture mode")];
    }
    ctx.session.set_thinking(on);
    let mut out = vec![
        AgentEvent::ThinkingChanged(on),
        notice(format!(
            "thinking {} (this session; the config default is unchanged)",
            onoff(on)
        )),
    ];
    if on && ctx.provider_name.as_str() == "openai-compat" {
        out.push(notice(
            "note: thinking is only used by the anthropic provider — the openai-compat wire has no mapping for it",
        ));
    }
    out
}

fn model_list(ctx: &mut CommandCtx) -> Vec<AgentEvent> {
    if ctx.profiles.is_empty() {
        return vec![
            notice("no profiles defined — add a \"profiles\" block to config.json, e.g."),
            notice(r#"  "profiles": { "local": { "provider": "openai-compat", "model": "qwen3-1.7b" } }"#),
            notice("then /model local switches to it (\"profile\": \"local\" selects it at startup)"),
        ];
    }
    let mut out: Vec<AgentEvent> = ctx
        .profiles
        .iter()
        .map(|(name, p)| {
            let mark = if Some(name.as_str()) == ctx.active_profile.as_deref() {
                " (active)"
            } else {
                ""
            };
            notice(format!("{name} — {} · {}{mark}", p.provider, p.model))
        })
        .collect();
    // T16 discoverability: the raw-id form was routinely misread as a
    // provider switch, so the listing says what a non-profile argument does.
    out.push(notice(
        "/model <name> switches profiles; any other argument is a raw model id on the ACTIVE provider",
    ));
    out.push(notice(
        "/models lists what the active provider serves; /model <id> --save persists the switch",
    ));
    out
}

fn model_switch(ctx: &mut CommandCtx, name: String) -> Vec<AgentEvent> {
    if ctx.replay_mode {
        return vec![notice("/model is unavailable in replay/capture mode")];
    }
    if ctx.active_profile.as_deref() == Some(name.as_str()) {
        return vec![notice(format!("already on profile {name:?}"))];
    }
    // Profile names win on collision (T9): a raw model id shadowed by a
    // profile name is unreachable — use the profile. Anything that is not a
    // profile name is treated as a raw model id for the ACTIVE provider.
    let Some(profile) = ctx.profiles.get(&name) else {
        return raw_model_switch(ctx, name);
    };
    if let Err(failure) = activate_profile(ctx, &name) {
        return vec![failure];
    }
    vec![
        AgentEvent::ModelSwitched {
            model: profile.model.clone(),
            provider: profile.provider.clone(),
        },
        notice(format!(
            "switched to {name} ({} · {})",
            profile.provider, profile.model
        )),
    ]
}

/// The FULL profile activation shared by `/model <name>` and the T16 hop.
/// Build FIRST — this is where a key file is read, by path, right now; the
/// session mutates only on success (a failed switch changes nothing, and
/// the failure notice is the returned `Err`). Then the prompt-profile swap
/// when the target differs (after the build, so the switch stays atomic:
/// rebuild_system and set_prompt are both infallible), then the
/// bookkeeping. The caller emits the chrome event and confirmation.
fn activate_profile(ctx: &mut CommandCtx, name: &str) -> Result<(), AgentEvent> {
    let profile = ctx.profiles.get(name).expect("caller resolved the name");
    let provider = (ctx.build_provider)(profile).map_err(|e| {
        notice(format!("switch to {name:?} failed: {e} — session unchanged"))
    })?;
    ctx.session.switch_provider(
        provider,
        profile.model.clone(),
        profile.max_tokens,
        profile.context_window,
    );
    if profile.prompt_profile != *ctx.prompt_profile {
        let system = (ctx.rebuild_system)(profile.prompt_profile);
        ctx.session.set_prompt(system, profile.prompt_profile);
        *ctx.prompt_profile = profile.prompt_profile;
    }
    *ctx.active_profile = Some(name.to_string());
    *ctx.provider_name = profile.provider.clone();
    *ctx.model = profile.model.clone();
    *ctx.active_resolved = profile.clone();
    Ok(())
}

/// `/model <raw-id>` (T9, T16). Decision order, first match wins:
///
/// 0. The ACTIVE provider's cached `/models` listing contains the id:
///    plain raw switch, no warning. The literal escape hatch — proxies
///    legitimately serve claude-* ids over openai-compat, and a user who
///    runs `/models` first always gets literal behavior.
/// 1. The id starts with "claude-", the active provider is not
///    "anthropic", and an anthropic profile is configured: HOP — full
///    profile activation of the anthropic profile whose model equals the
///    id exactly, else the first anthropic profile in name order; then,
///    when the id is not the profile's own model, the raw override on top.
/// 2. Same claude- signal but NO anthropic profile exists: today's plain
///    raw switch, plus a hint that an anthropic profile enables the hop.
/// 3. Anything else: plain raw switch, plus the T16 advisory when a cached
///    listing exists and the id is absent from it.
///
/// The plain raw switch is the active resolved selection with ONLY the
/// model replaced — same provider, endpoint, credential path, and limits,
/// so the active profile NAME stays. Raw ids are never validated online
/// here: a bad id surfaces as the provider's own clean error on the next
/// turn. Nothing in this function makes a network request; the only I/O is
/// the key file read inside the build path. The replay guard already fired
/// in [`model_switch`].
fn raw_model_switch(ctx: &mut CommandCtx, id: String) -> Vec<AgentEvent> {
    let listed = ctx.cached_model_ids.iter().any(|m| m == &id);
    if !listed && id.starts_with("claude-") && ctx.active_resolved.provider != "anthropic" {
        // Exact-model match first, else first anthropic profile; BTreeMap
        // iteration is name order, which is the tiebreak by design.
        let target = ctx
            .profiles
            .iter()
            .find(|(_, p)| p.provider == "anthropic" && p.model == id)
            .or_else(|| ctx.profiles.iter().find(|(_, p)| p.provider == "anthropic"))
            .map(|(n, _)| n.clone());
        match target {
            Some(name) => return hop_switch(ctx, id, name),
            None => {
                // Rule 2: the raw switch on the active provider, then the
                // hint (only when the switch actually happened).
                let from = ctx.active_resolved.provider.clone();
                let mut out = plain_raw_switch(ctx, &id);
                if *ctx.model == id {
                    out.push(notice(format!(
                        "note: {id:?} looks anthropic and was set on the ACTIVE provider ({from}); an anthropic profile in config.json enables the hop (temur init writes one)"
                    )));
                }
                return out;
            }
        }
    }
    // Rules 0 and 3.
    let mut out = plain_raw_switch(ctx, &id);
    // T16 advisory: the last `/models` listing is the only offline signal a
    // raw id has. Absence never blocks — servers alias ids and the listing
    // may be stale — the switch stands and the notice says exactly that.
    if *ctx.model == id && !listed && !ctx.cached_model_ids.is_empty() {
        out.push(notice(format!(
            "note: {id:?} is not in the last /models listing; the switch stands — a wrong id surfaces as the provider's error on the next turn"
        )));
    }
    out
}

/// The T16 cross-provider hop: full activation of `name` (an anthropic
/// profile), then the raw override when the id is not the profile's own
/// model. The notice names the mechanism and the profile so the switch is
/// never misread as "the active provider knows this model".
fn hop_switch(ctx: &mut CommandCtx, id: String, name: String) -> Vec<AgentEvent> {
    let hop_model = ctx.profiles[&name].model.clone();
    let hop_provider = ctx.profiles[&name].provider.clone();
    if let Err(failure) = activate_profile(ctx, &name) {
        return vec![failure];
    }
    if id == hop_model {
        return vec![
            AgentEvent::ModelSwitched {
                model: id.clone(),
                provider: hop_provider,
            },
            notice(format!(
                "{id:?} is an anthropic model - switched to profile {name:?} (anthropic, {id})"
            )),
        ];
    }
    match raw_override(ctx, &id) {
        Ok(()) => vec![
            AgentEvent::ModelSwitched {
                model: id.clone(),
                provider: hop_provider,
            },
            notice(format!(
                "{id:?} looks anthropic - hopped to profile {name:?} (its key file and limits apply), model {id}"
            )),
        ],
        // The activation already happened and stands; the override failure
        // is reported on top so the partial state is never silent.
        Err(failure) => vec![
            AgentEvent::ModelSwitched {
                model: hop_model.clone(),
                provider: hop_provider,
            },
            notice(format!(
                "hopped to profile {name:?}, but the model override to {id:?} failed — the profile's own model {hop_model} is active"
            )),
            failure,
        ],
    }
}

/// Today's raw switch behavior: [`raw_override`] plus the chrome event and
/// confirmation notice on success, or the failure notice alone.
fn plain_raw_switch(ctx: &mut CommandCtx, id: &str) -> Vec<AgentEvent> {
    if let Err(failure) = raw_override(ctx, id) {
        return vec![failure];
    }
    vec![
        AgentEvent::ModelSwitched {
            model: id.to_string(),
            provider: ctx.active_resolved.provider.clone(),
        },
        notice(format!(
            "switched model to {id} ({} · profile settings kept)",
            ctx.active_resolved.provider
        )),
    ]
}

/// The raw-id core: the active resolved selection with only the model
/// replaced, build-first atomic. Keeps the active profile name and never
/// touches the prompt profile.
fn raw_override(ctx: &mut CommandCtx, id: &str) -> Result<(), AgentEvent> {
    let mut target = ctx.active_resolved.clone();
    target.model = id.to_string();
    let provider = (ctx.build_provider)(&target).map_err(|e| {
        notice(format!(
            "switch to model {id:?} failed: {e} — session unchanged"
        ))
    })?;
    ctx.session.switch_provider(
        provider,
        target.model.clone(),
        target.max_tokens,
        target.context_window,
    );
    *ctx.model = id.to_string();
    *ctx.active_resolved = target;
    Ok(())
}

/// `/model --save` (T15): persist the CURRENTLY active model into the
/// config file, no switch. Replay-guarded like the mutators — it writes
/// real state.
fn model_save_current(ctx: &mut CommandCtx) -> Vec<AgentEvent> {
    if ctx.replay_mode {
        return vec![notice("/model is unavailable in replay/capture mode")];
    }
    vec![persist_notice(ctx)]
}

/// `/model <raw-id> --save` (T15): the raw switch, then persistence — only
/// after the switch succeeded, so a bad endpoint or credential can never
/// end up saved. A PROFILE name with `--save` is a clean error: what a
/// profile save would mean is the startup "profile" key, which stays a
/// hand edit (out of scope by design).
fn model_switch_save(ctx: &mut CommandCtx, id: String) -> Vec<AgentEvent> {
    if ctx.replay_mode {
        return vec![notice("/model is unavailable in replay/capture mode")];
    }
    if ctx.profiles.contains_key(&id) {
        return vec![notice(format!(
            "--save persists a raw model id, and {id:?} is a profile — the startup profile is the \"profile\" key in config.json, edited by hand"
        ))];
    }
    let mut out = raw_model_switch(ctx, id.clone());
    // Persist only when the REQUESTED id is what ended up active: a failed
    // switch, and the hop's partial state (activation ok, override failed),
    // must never reach the config file.
    if *ctx.model == id
        && out
            .iter()
            .any(|e| matches!(e, AgentEvent::ModelSwitched { .. }))
    {
        out.push(persist_notice(ctx));
    }
    out
}

/// The persistence step both `--save` forms share. Failure is a notice,
/// never fatal: an already-performed switch stands, and the notice says
/// so explicitly. When a profile is active — including the one a T16 hop
/// just activated — the notice names it, because that is the site
/// persist_model writes (profiles.<name>.model).
fn persist_notice(ctx: &CommandCtx) -> AgentEvent {
    match crate::config::persist_model(
        ctx.config_path,
        ctx.active_profile.as_deref(),
        ctx.provider_name,
        ctx.model,
    ) {
        Ok(()) => notice(match ctx.active_profile.as_deref() {
            Some(site) => format!(
                "saved model {} to profile {site:?} in {}",
                ctx.model,
                ctx.config_path.display()
            ),
            None => format!(
                "saved model {} to {}",
                ctx.model,
                ctx.config_path.display()
            ),
        }),
        Err(e) => notice(format!(
            "model {} is active for this session but was NOT saved: {e}",
            ctx.model
        )),
    }
}

/// `/models`: list model ids from the active provider. Read-only toward the
/// session but a LIVE network GET — so it is replay-guarded like the
/// mutators (ReplayTransport ignores URLs and pops fixtures; a live GET
/// under --mock/--capture-sse would desync the stream). Output is session
/// facts only: never key material.
fn models_list(ctx: &mut CommandCtx) -> Vec<AgentEvent> {
    if ctx.replay_mode {
        return vec![notice("/models is unavailable in replay/capture mode")];
    }
    match (ctx.list_models)(ctx.active_resolved) {
        Ok(ids) if ids.is_empty() => vec![notice("the provider reported no models")],
        Ok(ids) => vec![AgentEvent::ModelsListed(ids)],
        Err(e) => vec![notice(format!("/models failed: {e}"))],
    }
}

/// `/sessions` (T10): list every saved session, all projects. Read-only
/// toward the session but replay-guarded like its siblings — persistence is
/// off under `--mock`, so a listing there could only describe state the run
/// cannot touch. Lines carry the active marker; keys feed Tab completion.
fn sessions_list(ctx: &mut CommandCtx) -> Vec<AgentEvent> {
    if ctx.replay_mode {
        return vec![notice("/sessions is unavailable in replay/capture mode")];
    }
    let entries = session_store::list_sessions(ctx.sessions_dir);
    if entries.is_empty() {
        return vec![notice(format!(
            "no saved sessions in {} — sessions are created by the first turn",
            ctx.sessions_dir.display()
        ))];
    }
    let active = active_file_name(ctx);
    let mut lines = Vec::with_capacity(entries.len());
    let mut keys = Vec::with_capacity(entries.len());
    for e in &entries {
        let marker = if Some(e.file_name.as_str()) == active.as_deref() {
            "*"
        } else {
            " "
        };
        if e.cwd == "(unreadable)" {
            lines.push(format!("{marker} (unreadable) · {}", e.file_name));
        } else {
            let title = match &e.title {
                Some(t) => format!(" · {t}"),
                None => String::new(),
            };
            lines.push(format!(
                "{marker} {} · {} · {} msg(s) · {}{title}",
                e.name.as_deref().unwrap_or("(default)"),
                e.cwd,
                e.messages,
                e.file_name,
            ));
        }
        // The key a user would type back at /resume: the name where one
        // exists, the (prefix-resolvable) file name otherwise.
        keys.push(e.name.clone().unwrap_or_else(|| e.file_name.clone()));
    }
    vec![AgentEvent::SessionsListed { lines, keys }]
}

/// The file name the driver loop currently saves to — what "active" means.
fn active_file_name(ctx: &CommandCtx) -> Option<String> {
    ctx.persist_path
        .as_deref()
        .and_then(Path::file_name)
        .map(|s| s.to_string_lossy().into_owned())
}

/// `/resume <key>` (T10). LOAD FIRST, mutate only on success: resolution,
/// the file read, and prepare_seed all happen before the session, the
/// persist target, or the name bookkeeping change — any failure leaves the
/// live session exactly as it was (the `/model` atomicity rule applied to
/// sessions).
fn resume_session(ctx: &mut CommandCtx, key: String) -> Vec<AgentEvent> {
    if ctx.replay_mode {
        return vec![notice("/resume is unavailable in replay/capture mode")];
    }
    let entries = session_store::list_sessions(ctx.sessions_dir);
    let entry = match session_store::resolve_session_key(&entries, ctx.cwd_display, &key) {
        Ok(e) => e.clone(),
        Err(msg) => return vec![notice(msg)],
    };
    if active_file_name(ctx).as_deref() == Some(entry.file_name.as_str()) {
        return vec![notice(format!(
            "already on this session ({})",
            entry.file_name
        ))];
    }
    let path = ctx.sessions_dir.join(&entry.file_name);
    let file = match session_store::load(&path) {
        Ok(f) => f,
        Err(e) => return vec![notice(format!("/resume failed: {e} — session unchanged"))],
    };
    let name = file.name.clone();
    let file_cwd = file.cwd.clone();
    let (seed, mut notices) = session_store::prepare_seed(file);
    let summary = notices
        .pop()
        .expect("prepare_seed always appends the resume summary");
    let items = session_store::replay_items(&seed.history);

    // Everything fallible is done — now the switch, atomically.
    ctx.session.load_seed(seed);
    *ctx.persist_path = Some(path);
    *ctx.session_name = name;

    let mut out = vec![AgentEvent::SessionLoaded {
        items,
        notice: summary,
    }];
    out.extend(notices.into_iter().map(notice)); // the dropped-prompt rule
    if file_cwd != ctx.cwd_display {
        out.push(notice(format!(
            "session was recorded in {file_cwd}; tools run in the current directory {}",
            ctx.cwd_display
        )));
    }
    out
}

/// `/new <name>` (T10): start a fresh NAMED session for this project. The
/// name is required (the default session needs no command — it is what a
/// plain start uses) and must survive sanitizing; a name that already has a
/// file is an error pointing at `/resume`. No file is written here: the
/// first turn's save creates it, same as a fresh start.
fn new_session(ctx: &mut CommandCtx, raw: String) -> Vec<AgentEvent> {
    if ctx.replay_mode {
        return vec![notice("/new is unavailable in replay/capture mode")];
    }
    let Some(name) = session_store::sanitize_session_name(&raw) else {
        return vec![notice(format!(
            "session name {raw:?} has no usable characters (allowed: letters, digits, . _ -)"
        ))];
    };
    let path = ctx
        .sessions_dir
        .join(session_store::named_session_file_name(ctx.cwd, &name));
    if path.exists() {
        return vec![notice(format!(
            "session {name:?} already exists — /resume {name} switches to it"
        ))];
    }
    ctx.session.clear_history();
    *ctx.persist_path = Some(path);
    *ctx.session_name = Some(name.clone());
    vec![
        AgentEvent::SessionCleared,
        notice(format!(
            "new session {name:?} — the file is created on the first turn"
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_table() {
        // (input, expected)
        let cases: Vec<(&str, Command)> = vec![
            ("/help", Command::Help),
            ("/status", Command::Status),
            ("/clear", Command::Clear),
            ("/thinking", Command::ThinkingShow),
            ("/thinking on", Command::ThinkingSet(true)),
            ("/thinking off", Command::ThinkingSet(false)),
            ("/thinking maybe", Command::Invalid("usage: /thinking [on|off]".into())),
            ("/thinking on off", Command::Invalid("usage: /thinking [on|off]".into())),
            ("/model", Command::ModelList),
            ("/model local", Command::ModelSwitch("local".into())),
            ("/model a b", Command::Invalid("usage: /model [<profile>|<model-id>] [--save]".into())),
            // T15: the --save forms.
            ("/model --save", Command::ModelSaveCurrent),
            ("/model qwen3-4b --save", Command::ModelSwitchSave("qwen3-4b".into())),
            ("/model --save qwen3-4b", Command::Invalid("usage: /model [<profile>|<model-id>] [--save]".into())),
            ("/model a --save b", Command::Invalid("usage: /model [<profile>|<model-id>] [--save]".into())),
            ("/model --save --save", Command::Invalid("usage: /model [<profile>|<model-id>] [--save]".into())),
            ("/model a b --save", Command::Invalid("usage: /model [<profile>|<model-id>] [--save]".into())),
            ("/models", Command::ModelsList),
            ("/models extra", Command::Invalid("/models takes no arguments".into())),
            ("/sessions", Command::SessionsList),
            ("/sessions extra", Command::Invalid("/sessions takes no arguments".into())),
            ("/resume alpha", Command::Resume("alpha".into())),
            ("/resume", Command::Invalid("usage: /resume <session>".into())),
            ("/resume a b", Command::Invalid("usage: /resume <session>".into())),
            ("/new alpha", Command::New("alpha".into())),
            ("/new", Command::Invalid("usage: /new <name>".into())),
            ("/new a b", Command::Invalid("usage: /new <name>".into())),
            ("/RESUME x", Command::Unknown("/RESUME".into())), // exact lowercase only
            ("/MODELS", Command::Unknown("/MODELS".into())), // exact lowercase only
            ("/help me", Command::Invalid("/help takes no arguments".into())),
            ("/status now", Command::Invalid("/status takes no arguments".into())),
            ("/clear all", Command::Invalid("/clear takes no arguments".into())),
            ("/frobnicate", Command::Unknown("/frobnicate".into())),
            ("/", Command::Unknown("/".into())),
            ("/MODEL", Command::Unknown("/MODEL".into())), // exact lowercase only
        ];
        for (input, expected) in cases {
            assert_eq!(parse(input), expected, "input: {input:?}");
        }
        // Whitespace-tolerant around words.
        assert_eq!(parse("/model   local  "), Command::ModelSwitch("local".into()));
    }

    #[test]
    fn complete_table() {
        let profiles = vec!["local".to_string(), "sonnet".to_string()];
        let ids = vec!["qwen3-1.7b".to_string(), "local".to_string()];
        let keys = vec!["alpha".to_string(), "temur-9591.json".to_string()];
        let c = |input: &str| complete(input, &profiles, &ids, &keys);
        // (input, expected full-line candidates)
        let cases: Vec<(&str, Vec<&str>)> = vec![
            // Command names while the head is being typed; "/" offers all.
            ("/sta", vec!["/status"]),
            ("/model", vec!["/model", "/models"]),
            ("/models", vec!["/models"]),
            (
                "/",
                vec![
                    "/help", "/status", "/model", "/models", "/clear", "/sessions",
                    "/resume", "/new", "/thinking",
                ],
            ),
            ("/zzz", vec![]),
            // /resume args: the cached session keys (T10), prefix-filtered.
            ("/resume ", vec!["/resume alpha", "/resume temur-9591.json"]),
            ("/resume al", vec!["/resume alpha"]),
            ("/resume nope-", vec![]),
            // /new never completes: its argument is a NEW name by definition.
            ("/new ", vec![]),
            ("/sessions ", vec![]),
            // /model args: profiles first, then cached ids, deduplicated
            // ("local" is both), prefix-filtered.
            ("/model ", vec!["/model local", "/model sonnet", "/model qwen3-1.7b"]),
            ("/model lo", vec!["/model local"]),
            ("/model q", vec!["/model qwen3-1.7b"]),
            ("/model nope-", vec![]),
            // /thinking args.
            ("/thinking ", vec!["/thinking on", "/thinking off"]),
            ("/thinking o", vec!["/thinking on", "/thinking off"]),
            ("/thinking of", vec!["/thinking off"]),
            // Nothing else completes: not a command, past the first
            // argument, or an argument to a no-arg command.
            ("hello", vec![]),
            ("", vec![]),
            ("/model local extra", vec![]),
            ("/status ", vec![]),
            ("/clear x", vec![]),
        ];
        for (input, want) in cases {
            assert_eq!(c(input), want, "input: {input:?}");
        }
        // No profiles and no cached ids/keys: no argument candidates.
        assert!(complete("/model ", &[], &[], &[]).is_empty());
        assert!(complete("/resume ", &[], &[], &[]).is_empty());
    }
}
