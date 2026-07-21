//! Agent core: conversation state and the tool-call turn loop, ported from
//! OpenCode's processor semantics onto native Anthropic stop reasons.

pub mod events;
pub mod recover;

use crate::cancel::CancelToken;
use crate::provider::{
    ChatRequest, ContentBlock, Provider, ProviderError, RequestMessage, Role, StopReason, Usage,
};
use crate::session_store::SessionSeed;
use crate::tools::{Registry, TodoItem, ToolCtx};
use events::AgentEvent;

/// Mirrors OpenCode's doom-loop threshold: N identical consecutive tool
/// calls stop the turn.
const DOOM_LOOP_THRESHOLD: u32 = 3;

/// T4 weak-model guards, hardcoded like the doom-loop threshold above.
///
/// Consecutive batches in which EVERY tool result was an error stop the
/// turn. Independent of the doom-loop guard: identical×3 catches a model
/// stuck verbatim, this catches one thrashing with different arguments.
const CONSECUTIVE_TOOL_FAILURE_LIMIT: u32 = 5;
/// Consecutive empty responses (no tool use, no thinking, whitespace-only
/// text) stop the turn — protects the PauseTurn-resend and nudge paths. A
/// single empty EndTurn still finishes cleanly.
const EMPTY_RESPONSE_LIMIT: u32 = 3;
/// Corrective nudges per turn for tool calls written as plain text.
const NUDGE_LIMIT: u32 = 2;

#[derive(thiserror::Error, Debug)]
pub enum AgentError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

pub struct SessionConfig {
    pub model: String,
    pub max_tokens: u32,
    pub system: Option<String>,
    pub thinking: bool,
    pub cwd: std::path::PathBuf,
    pub max_iterations: u32,
    /// Sampling knobs, mapped by every provider; `None` = provider default.
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    /// Advisory context-window size (tokens) of the served model. `None` =
    /// awareness off. Warnings only — never compaction, trimming, or
    /// request-side enforcement.
    pub context_window: Option<u64>,
}

impl SessionConfig {
    pub fn from_config(cfg: &crate::config::Config, cwd: std::path::PathBuf) -> Self {
        SessionConfig {
            model: cfg.model.clone(),
            max_tokens: cfg.max_tokens,
            system: cfg.system_prompt.clone(),
            thinking: cfg.thinking,
            cwd,
            max_iterations: cfg.max_turn_iterations,
            temperature: cfg.temperature,
            top_p: cfg.top_p,
            // A property of the served model, not of temur: main.rs sets it
            // from the provider section that knows the server.
            context_window: None,
        }
    }
}

pub struct Session {
    provider: Box<dyn Provider>,
    registry: Registry,
    tool_ctx: ToolCtx,
    cfg: SessionConfig,
    history: Vec<RequestMessage>,
    session_usage: Usage,
    /// input+output of the MOST RECENT response — the best available
    /// estimate of context occupancy (session totals would double-count the
    /// resent history). Stays stale when a quirk server reports no usage.
    last_context_used: Option<u64>,
    /// The context pre-warning fires once per session, not per turn.
    context_warned: bool,
    /// T6 cooperative interruption. The UI holds a clone (via
    /// [`Session::cancel_token`]) and sets it; the provider stack polls it.
    cancel: CancelToken,
}

/// Everything a session persists, borrowed. ONE method
/// ([`Session::snapshot`]) defines what survives a restart, and it lives here
/// next to the private state it reads — so adding state to `Session` puts the
/// question "does this belong in a saved session?" in front of whoever adds
/// it, instead of leaving it to a distant serializer to notice.
pub struct SessionSnapshot<'a> {
    pub history: &'a [RequestMessage],
    pub session_usage: Usage,
    pub todos: &'a [TodoItem],
    pub last_context_used: Option<u64>,
}

impl Session {
    pub fn new(provider: Box<dyn Provider>, registry: Registry, cfg: SessionConfig) -> Self {
        let tool_ctx = ToolCtx::new(cfg.cwd.clone());
        Session {
            provider,
            registry,
            tool_ctx,
            cfg,
            history: Vec::new(),
            session_usage: Usage::default(),
            last_context_used: None,
            context_warned: false,
            cancel: CancelToken::new(),
        }
    }

    /// Rebuild a session from a saved seed. Mirrors [`Session::new`] — same
    /// provider, registry, and config path — and differs only in the state it
    /// starts from. Infallible by construction: every decision about what is
    /// safe to replay was already made in `session_store::prepare_seed`.
    ///
    /// `context_warned` is deliberately NOT seeded: the context pre-warning is
    /// once per process, and a fresh process that is about to overflow should
    /// say so again.
    pub fn resume(
        provider: Box<dyn Provider>,
        registry: Registry,
        cfg: SessionConfig,
        seed: SessionSeed,
    ) -> Self {
        let mut tool_ctx = ToolCtx::new(cfg.cwd.clone());
        tool_ctx.todos = seed.todos;
        Session {
            provider,
            registry,
            tool_ctx,
            cfg,
            history: seed.history,
            session_usage: seed.session_usage,
            last_context_used: seed.last_context_used,
            context_warned: false,
            cancel: CancelToken::new(),
        }
    }

    pub fn history(&self) -> &[RequestMessage] {
        &self.history
    }

    /// A clone of this session's cancel token, for the UI thread to set.
    /// Holding a clone never requires holding the session itself.
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// The persistable view of this session. Borrowed throughout: saving a
    /// multi-megabyte history must not clone it.
    pub fn snapshot(&self) -> SessionSnapshot<'_> {
        SessionSnapshot {
            history: &self.history,
            session_usage: self.session_usage,
            todos: &self.tool_ctx.todos,
            last_context_used: self.last_context_used,
        }
    }

    /// Run one user turn to completion (which may involve many provider
    /// round-trips for tool use). Tool failures feed back to the model;
    /// only provider-level failures return `Err`.
    pub fn turn(
        &mut self,
        user_input: &str,
        ui: &mut dyn FnMut(AgentEvent),
    ) -> Result<(), AgentError> {
        self.history.push(RequestMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: user_input.to_string(),
            }],
        });

        let mut turn_usage = Usage::default();
        let mut iterations: u32 = 0;
        let mut last_fingerprint = String::new();
        let mut repeat_count: u32 = 0;
        // T4 guard state (all per-turn).
        let mut fingerprint_window: Vec<String> = Vec::new();
        let mut consecutive_failed_batches: u32 = 0;
        let mut consecutive_empty: u32 = 0;
        let mut nudges: u32 = 0;

        loop {
            iterations += 1;
            if iterations > self.cfg.max_iterations {
                ui(AgentEvent::Notice(format!(
                    "stopped: reached the {}-iteration limit for a single turn",
                    self.cfg.max_iterations
                )));
                break;
            }

            let req = ChatRequest {
                model: self.cfg.model.clone(),
                max_tokens: self.cfg.max_tokens,
                system: self.cfg.system.clone(),
                thinking: self.cfg.thinking,
                // None sends nothing (provider default), exactly as before
                // config grew these knobs.
                temperature: self.cfg.temperature.map(f64::from),
                top_p: self.cfg.top_p.map(f64::from),
                messages: self.history.clone(),
                tools: self.registry.definitions(),
            };
            let msg = self.provider.stream(
                &req,
                &mut |ev| {
                    ui(match ev {
                        crate::provider::StreamEvent::TextDelta(t) => AgentEvent::TextDelta(t),
                        crate::provider::StreamEvent::ThinkingDelta(t) => {
                            AgentEvent::ThinkingDelta(t)
                        }
                        crate::provider::StreamEvent::ToolUseStarted { name } => {
                            AgentEvent::ToolStart { name }
                        }
                    })
                },
                &self.cancel,
            )?;

            turn_usage.add(&msg.usage);
            self.session_usage.add(&msg.usage);

            // Advisory context estimate (T3): the most recent response's
            // input+output IS the occupancy after this round-trip. One
            // round-trip stale by nature (no local tokenizer), and left
            // stale when usage isn't reported at all.
            if msg.usage.input_tokens.is_some() || msg.usage.output_tokens.is_some() {
                self.last_context_used = Some(
                    msg.usage.input_tokens.unwrap_or(0) + msg.usage.output_tokens.unwrap_or(0),
                );
            }
            if !self.context_warned {
                if let (Some(window), Some(used)) =
                    (self.cfg.context_window, self.last_context_used)
                {
                    if window.saturating_sub(used) < u64::from(self.cfg.max_tokens) {
                        self.context_warned = true;
                        ui(AgentEvent::Notice(format!(
                            "context: ~{used} of {window} tokens used; the next response may not fit (max_tokens {}) — consider starting a new session",
                            self.cfg.max_tokens
                        )));
                    }
                }
            }

            let stop = msg.stop_reason;
            let stop_details = msg.stop_details.clone();
            // Unknown blocks must not be echoed back to the API.
            let content: Vec<ContentBlock> = msg
                .content
                .into_iter()
                .filter(|b| !matches!(b, ContentBlock::Unknown))
                .collect();

            // Empty-response guard: no tool use, no thinking, and only
            // whitespace text. One empty EndTurn finishes cleanly below;
            // this stops the loops that would otherwise resend forever.
            let is_empty = content.iter().all(|b| match b {
                ContentBlock::Text { text } => text.trim().is_empty(),
                _ => false,
            });
            if is_empty {
                consecutive_empty += 1;
                if consecutive_empty >= EMPTY_RESPONSE_LIMIT {
                    ui(AgentEvent::Notice(format!(
                        "stopped: the model returned {EMPTY_RESPONSE_LIMIT} consecutive empty responses"
                    )));
                    break;
                }
            } else {
                consecutive_empty = 0;
            }

            match stop {
                Some(StopReason::Refusal) => {
                    // Discard the (partial or empty) refused output entirely;
                    // never auto-retry the same prompt.
                    let mut notice = String::from("the model refused this request");
                    if let Some(d) = stop_details {
                        if let Some(cat) = d.category {
                            notice.push_str(&format!(" (category: {cat})"));
                        }
                        if let Some(expl) = d.explanation {
                            notice.push_str(&format!(": {expl}"));
                        }
                    }
                    ui(AgentEvent::Notice(notice));
                    break;
                }
                Some(StopReason::ToolUse) => {
                    self.history.push(RequestMessage {
                        role: Role::Assistant,
                        content: content.clone(),
                    });
                    let calls: Vec<(String, String, serde_json::Value, Option<String>)> = content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::ToolUse {
                                id,
                                name,
                                input,
                                input_raw,
                            } => Some((id.clone(), name.clone(), input.clone(), input_raw.clone())),
                            _ => None,
                        })
                        .collect();
                    if calls.is_empty() {
                        ui(AgentEvent::Notice(
                            "model requested tool use but sent no tool calls".into(),
                        ));
                        break;
                    }

                    // Doom-loop guard on identical consecutive calls.
                    // Fingerprint format unchanged by T4.
                    let fingerprint = calls
                        .iter()
                        .map(|(_, name, input, _)| format!("{name}:{input}"))
                        .collect::<Vec<_>>()
                        .join("|");
                    if fingerprint == last_fingerprint {
                        repeat_count += 1;
                    } else {
                        repeat_count = 1;
                        last_fingerprint = fingerprint.clone();
                    }
                    if repeat_count >= DOOM_LOOP_THRESHOLD {
                        ui(AgentEvent::Notice(format!(
                            "stopped: the same tool call was repeated {DOOM_LOOP_THRESHOLD} times in a row"
                        )));
                        break;
                    }

                    // Alternating-pair doom loop (T4): A,B,A,B,A,B over a
                    // 6-deep fingerprint window.
                    fingerprint_window.push(fingerprint);
                    if fingerprint_window.len() > 6 {
                        fingerprint_window.remove(0);
                    }
                    if let [f6, f5, f4, f3, f2, f1] = fingerprint_window.as_slice() {
                        if f1 == f3 && f3 == f5 && f2 == f4 && f4 == f6 && f1 != f2 {
                            ui(AgentEvent::Notice(
                                "stopped: two tool calls alternated 3 times in a row".into(),
                            ));
                            break;
                        }
                    }

                    // Execute every call; ALL results go back in ONE user message.
                    let mut results: Vec<ContentBlock> = Vec::with_capacity(calls.len());
                    for (id, name, input, input_raw) in calls {
                        let (output, title, is_error) = match input_raw {
                            // T4 dispatch policy for arguments that failed to
                            // parse on the wire: execute only a LOSSLESS
                            // repair. A completed truncation is schema-valid
                            // but semantically wrong — a silent wrong
                            // write/bash — so Lossy and unrepairable both
                            // feed an error back instead of executing.
                            Some(raw) => match recover::repair_json(&raw) {
                                Some(recover::Repaired::Lossless(v)) => {
                                    ui(AgentEvent::Notice(format!(
                                        "{name}: malformed tool arguments were losslessly repaired before execution"
                                    )));
                                    match self.registry.execute(&name, v, &mut self.tool_ctx) {
                                        Ok(out) => (out.output, out.title, false),
                                        Err(e) => (e.to_string(), name.clone(), true),
                                    }
                                }
                                _ => {
                                    let parse_err =
                                        match serde_json::from_str::<serde_json::Value>(&raw) {
                                            Err(e) => e.to_string(),
                                            Ok(_) => "not a JSON object".to_string(),
                                        };
                                    let echoed: String = raw.chars().take(500).collect();
                                    (
                                        format!(
                                            "The tool call was NOT executed: its arguments were not valid JSON. \
                                             Parse error: {parse_err}\n\
                                             Raw arguments as received (first 500 chars):\n{echoed}\n\
                                             Re-issue the tool call with complete, valid JSON arguments."
                                        ),
                                        name.clone(),
                                        true,
                                    )
                                }
                            },
                            None => match self.registry.execute(&name, input, &mut self.tool_ctx) {
                                Ok(out) => (out.output, out.title, false),
                                Err(e) => (e.to_string(), name.clone(), true),
                            },
                        };
                        ui(AgentEvent::ToolEnd {
                            name,
                            title,
                            is_error,
                        });
                        results.push(ContentBlock::ToolResult {
                            tool_use_id: id,
                            content: output,
                            is_error,
                        });
                    }

                    // Consecutive-failure cap (T4): a batch where every
                    // result errored counts; any success resets. The results
                    // message is pushed BEFORE stopping so history stays
                    // consistent with the calls the model made.
                    let all_errored = results.iter().all(|b| {
                        matches!(b, ContentBlock::ToolResult { is_error: true, .. })
                    });
                    if all_errored {
                        consecutive_failed_batches += 1;
                    } else {
                        consecutive_failed_batches = 0;
                    }
                    self.history.push(RequestMessage {
                        role: Role::User,
                        content: results,
                    });
                    if consecutive_failed_batches >= CONSECUTIVE_TOOL_FAILURE_LIMIT {
                        ui(AgentEvent::Notice(format!(
                            "stopped: every tool call failed in {CONSECUTIVE_TOOL_FAILURE_LIMIT} consecutive batches"
                        )));
                        break;
                    }
                }
                Some(StopReason::PauseTurn) => {
                    // Append assistant content and re-send as-is to resume.
                    self.history.push(RequestMessage {
                        role: Role::Assistant,
                        content,
                    });
                }
                other => {
                    // Text-tool-call nudge (T4): an EndTurn whose message
                    // made no structured calls but *reads* like a tool call.
                    // DETECT + FEEDBACK only — prose is never parsed into an
                    // execution.
                    let nudge = matches!(other, Some(StopReason::EndTurn))
                        && nudges < NUDGE_LIMIT
                        && !content
                            .iter()
                            .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
                        && {
                            let text = content
                                .iter()
                                .filter_map(|b| match b {
                                    ContentBlock::Text { text } => Some(text.as_str()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            let tool_names: Vec<String> = self
                                .registry
                                .definitions()
                                .iter()
                                .map(|d| d.name.clone())
                                .collect();
                            recover::detect_text_tool_call(&text, &tool_names)
                        };
                    self.history.push(RequestMessage {
                        role: Role::Assistant,
                        content,
                    });
                    if nudge {
                        nudges += 1;
                        self.history.push(RequestMessage {
                            role: Role::User,
                            content: vec![ContentBlock::Text {
                                text: "You wrote what looks like a tool call as plain text. \
                                       Nothing was executed — text is never interpreted as a \
                                       tool call. Invoke the tool through the structured \
                                       tool-calling interface instead."
                                    .into(),
                            }],
                        });
                        ui(AgentEvent::Notice(
                            "the model wrote a tool call as plain text; asked it to use the tool interface"
                                .into(),
                        ));
                        continue;
                    }
                    match other {
                        Some(StopReason::MaxTokens) => {
                            // Near the configured window, max_tokens is the
                            // symptom, overflow the likely cause. Providers
                            // stay faithful wire mappers; this heuristic
                            // lives here. Without a window (or without
                            // usage) the wording is EXACTLY the old string.
                            let near_window = match (self.cfg.context_window, self.last_context_used)
                            {
                                (Some(window), Some(used)) => {
                                    used + u64::from(self.cfg.max_tokens) >= window
                                }
                                _ => false,
                            };
                            if near_window {
                                let used = self.last_context_used.unwrap_or(0);
                                let window = self.cfg.context_window.unwrap_or(0);
                                ui(AgentEvent::Notice(format!(
                                    "response truncated: max_tokens reached near the context window (~{used} of {window} tokens) — likely context overflow; consider starting a new session"
                                )));
                            } else {
                                ui(AgentEvent::Notice(
                                    "response truncated: max_tokens reached".into(),
                                ));
                            }
                        }
                        Some(StopReason::ModelContextWindowExceeded) => ui(AgentEvent::Notice(
                            "context window exceeded; consider starting a new session".into(),
                        )),
                        Some(StopReason::Unknown) => ui(AgentEvent::Notice(
                            "model stopped for an unrecognized reason".into(),
                        )),
                        None => ui(AgentEvent::Notice(
                            "stream ended without a stop reason".into(),
                        )),
                        _ => {} // EndTurn / StopSequence: clean finish
                    }
                    break;
                }
            }
        }

        ui(AgentEvent::TurnComplete {
            turn_usage,
            session_usage: self.session_usage,
        });
        Ok(())
    }
}
