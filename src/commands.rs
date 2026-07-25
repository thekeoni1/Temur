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
    match (head, arg, extra) {
        ("/help", None, _) => Command::Help,
        ("/status", None, _) => Command::Status,
        ("/clear", None, _) => Command::Clear,
        ("/thinking", None, _) => Command::ThinkingShow,
        ("/thinking", Some("on"), None) => Command::ThinkingSet(true),
        ("/thinking", Some("off"), None) => Command::ThinkingSet(false),
        ("/thinking", ..) => Command::Invalid("usage: /thinking [on|off]".into()),
        ("/model", None, _) => Command::ModelList,
        ("/model", Some(name), None) => Command::ModelSwitch(name.to_string()),
        ("/model", ..) => Command::Invalid("usage: /model [<profile>]".into()),
        ("/help" | "/status" | "/clear", Some(_), _) => {
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
    /// `None` = persistence disabled (`--mock`).
    pub persist_path: Option<&'a Path>,
    pub session_max_bytes: u64,
    pub cwd_display: &'a str,
    /// `--mock` / `--capture-sse`: state-mutating commands are disabled to
    /// keep fixture determinism.
    pub replay_mode: bool,
    #[allow(clippy::type_complexity)]
    pub build_provider:
        &'a dyn Fn(&ResolvedProfile) -> Result<Box<dyn Provider>, crate::error::Error>,
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

const HELP_LINES: &[&str] = &[
    "/help — this list",
    "/status — profile, provider, model, thinking, context, session file",
    "/model — list profiles · /model <name> — switch to a profile",
    "/clear — wipe this session's history and start fresh",
    "/thinking — show · /thinking on|off — flip adaptive thinking (this session)",
    "exit or quit — leave",
];

pub fn run(cmd: Command, ctx: &mut CommandCtx) -> Vec<AgentEvent> {
    match cmd {
        Command::Help => HELP_LINES.iter().map(|l| notice(*l)).collect(),
        Command::Status => status(ctx),
        Command::Clear => clear(ctx),
        Command::ThinkingShow => vec![notice(format!(
            "thinking: {}",
            onoff(ctx.session.thinking())
        ))],
        Command::ThinkingSet(on) => thinking_set(ctx, on),
        Command::ModelList => model_list(ctx),
        Command::ModelSwitch(name) => model_switch(ctx, name),
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
            "thinking: {} · max_tokens: {}",
            onoff(s.thinking()),
            s.max_tokens()
        )),
        notice(match (s.context_window(), s.last_context_used()) {
            (Some(w), Some(u)) => format!("context: ~{u} of {w} tokens used"),
            (None, Some(u)) => format!("context: ~{u} tokens used (window size unknown)"),
            _ => "context: no usage reported yet".into(),
        }),
        notice(match ctx.persist_path {
            Some(p) => format!("session file: {}", p.display()),
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
    if let Some(path) = ctx.persist_path {
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
    ctx.profiles
        .iter()
        .map(|(name, p)| {
            let mark = if Some(name.as_str()) == ctx.active_profile.as_deref() {
                " (active)"
            } else {
                ""
            };
            notice(format!("{name} — {} · {}{mark}", p.provider, p.model))
        })
        .collect()
}

fn model_switch(ctx: &mut CommandCtx, name: String) -> Vec<AgentEvent> {
    if ctx.replay_mode {
        return vec![notice("/model is unavailable in replay/capture mode")];
    }
    if ctx.active_profile.as_deref() == Some(name.as_str()) {
        return vec![notice(format!("already on profile {name:?}"))];
    }
    let Some(profile) = ctx.profiles.get(&name) else {
        return vec![notice(format!(
            "no profile named {name:?} — /model lists profiles"
        ))];
    };
    // Build FIRST — this is where a key file is read, by path, right now.
    // The session mutates only on success: a failed switch changes nothing.
    let provider = match (ctx.build_provider)(profile) {
        Ok(p) => p,
        Err(e) => {
            return vec![notice(format!(
                "switch to {name:?} failed: {e} — session unchanged"
            ))]
        }
    };
    ctx.session.switch_provider(
        provider,
        profile.model.clone(),
        profile.max_tokens,
        profile.context_window,
    );
    *ctx.active_profile = Some(name.clone());
    *ctx.provider_name = profile.provider.clone();
    *ctx.model = profile.model.clone();
    vec![
        AgentEvent::ModelSwitched {
            model: profile.model.clone(),
        },
        notice(format!(
            "switched to {name} ({} · {})",
            profile.provider, profile.model
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
            ("/model a b", Command::Invalid("usage: /model [<profile>]".into())),
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
}
