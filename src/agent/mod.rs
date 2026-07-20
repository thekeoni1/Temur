//! Agent core: conversation state and the tool-call turn loop, ported from
//! OpenCode's processor semantics onto native Anthropic stop reasons.

pub mod events;

use crate::provider::{
    ChatRequest, ContentBlock, Provider, ProviderError, RequestMessage, Role, StopReason, Usage,
};
use crate::tools::{Registry, ToolCtx};
use events::AgentEvent;

/// Mirrors OpenCode's doom-loop threshold: N identical consecutive tool
/// calls stop the turn.
const DOOM_LOOP_THRESHOLD: u32 = 3;

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
        }
    }

    pub fn history(&self) -> &[RequestMessage] {
        &self.history
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
            let msg = self.provider.stream(&req, &mut |ev| {
                ui(match ev {
                    crate::provider::StreamEvent::TextDelta(t) => AgentEvent::TextDelta(t),
                    crate::provider::StreamEvent::ThinkingDelta(t) => AgentEvent::ThinkingDelta(t),
                    crate::provider::StreamEvent::ToolUseStarted { name } => {
                        AgentEvent::ToolStart { name }
                    }
                })
            })?;

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
                    let calls: Vec<(String, String, serde_json::Value)> = content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::ToolUse { id, name, input, .. } => {
                                Some((id.clone(), name.clone(), input.clone()))
                            }
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
                    let fingerprint = calls
                        .iter()
                        .map(|(_, name, input)| format!("{name}:{input}"))
                        .collect::<Vec<_>>()
                        .join("|");
                    if fingerprint == last_fingerprint {
                        repeat_count += 1;
                    } else {
                        repeat_count = 1;
                        last_fingerprint = fingerprint;
                    }
                    if repeat_count >= DOOM_LOOP_THRESHOLD {
                        ui(AgentEvent::Notice(format!(
                            "stopped: the same tool call was repeated {DOOM_LOOP_THRESHOLD} times in a row"
                        )));
                        break;
                    }

                    // Execute every call; ALL results go back in ONE user message.
                    let mut results: Vec<ContentBlock> = Vec::with_capacity(calls.len());
                    for (id, name, input) in calls {
                        let (output, title, is_error) =
                            match self.registry.execute(&name, input, &mut self.tool_ctx) {
                                Ok(out) => (out.output, out.title, false),
                                Err(e) => (e.to_string(), name.clone(), true),
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
                    self.history.push(RequestMessage {
                        role: Role::User,
                        content: results,
                    });
                }
                Some(StopReason::PauseTurn) => {
                    // Append assistant content and re-send as-is to resume.
                    self.history.push(RequestMessage {
                        role: Role::Assistant,
                        content,
                    });
                }
                other => {
                    self.history.push(RequestMessage {
                        role: Role::Assistant,
                        content,
                    });
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
