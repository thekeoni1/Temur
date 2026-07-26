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

/// The synthesized error result every never-executed `tool_use` is answered
/// with when a turn is interrupted (wire rule: every id answered in the next
/// user message). One constant, one builder — the shape exists nowhere else.
pub const INTERRUPT_MARKER: &str = "[interrupted by user]";

impl Session {
    pub fn new(provider: Box<dyn Provider>, registry: Registry, cfg: SessionConfig) -> Self {
        Self::build(provider, registry, cfg, None)
    }

    /// Rebuild a session from a saved seed. Same provider, registry, and
    /// config path as [`Session::new`]; differs only in the state it starts
    /// from. Infallible by construction: every decision about what is safe to
    /// replay was already made in `session_store::prepare_seed`.
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
        Self::build(provider, registry, cfg, Some(seed))
    }

    /// The one constructor: fresh (`seed: None`) or seeded. Cancel/ToolCtx
    /// wiring exists exactly once, here.
    fn build(
        provider: Box<dyn Provider>,
        registry: Registry,
        cfg: SessionConfig,
        seed: Option<SessionSeed>,
    ) -> Self {
        let cancel = CancelToken::new();
        let mut tool_ctx = ToolCtx::new(cfg.cwd.clone());
        // One token per session: an Esc must reach a running bash too.
        tool_ctx.cancel = cancel.clone();
        let (history, session_usage, todos, last_context_used) = match seed {
            Some(s) => (s.history, s.session_usage, s.todos, s.last_context_used),
            None => (Vec::new(), Usage::default(), Vec::new(), None),
        };
        tool_ctx.todos = todos;
        Session {
            provider,
            registry,
            tool_ctx,
            cfg,
            history,
            session_usage,
            last_context_used,
            context_warned: false,
            cancel,
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

    // ------------------------------------------------- T8 between-turns seam
    // INVARIANT: everything in this block is callable only BETWEEN turns.
    // The driver loop serializes input — it reads the next line only while
    // the agent is at the prompt — so none of these can run while `turn` is
    // on the stack.

    /// Swap the active provider/model in place (`/model`). The CALLER builds
    /// the new provider first — including any credential read — and calls
    /// this only on success, so a failed switch leaves the session untouched:
    /// atomicity holds at the call site by construction. History, usage, and
    /// todos survive — switching models continues the same conversation.
    /// `context_warned` re-arms because the once-per-session warning was
    /// about the OLD window.
    pub fn switch_provider(
        &mut self,
        provider: Box<dyn Provider>,
        model: String,
        max_tokens: u32,
        context_window: Option<u64>,
    ) {
        self.provider = provider;
        self.cfg.model = model;
        self.cfg.max_tokens = max_tokens;
        self.cfg.context_window = context_window;
        self.context_warned = false;
    }

    /// Swap the system prompt and tool-prompt profile in place (T9: a
    /// `/model` switch onto a profile with a different `prompt_profile`).
    /// Infallible by design — the caller assembles the system string first —
    /// so it composes with [`Session::switch_provider`] without breaking the
    /// build-first atomicity of a switch. The next request picks both up via
    /// the per-iteration rebuild in [`Session::turn`].
    pub fn set_prompt(&mut self, system: String, profile: crate::tools::PromptProfile) {
        self.cfg.system = Some(system);
        self.registry.set_profile(profile);
    }

    /// Wipe the conversation (`/clear`): history, usage totals, context
    /// estimate, warning latch, and todos. Provider, model, and config stay.
    pub fn clear_history(&mut self) {
        self.history.clear();
        self.session_usage = Usage::default();
        self.last_context_used = None;
        self.context_warned = false;
        self.tool_ctx.todos.clear();
    }

    /// Flip adaptive thinking for THIS session (`/thinking`); the config
    /// default is untouched.
    pub fn set_thinking(&mut self, on: bool) {
        self.cfg.thinking = on;
    }

    // `/status` getters: read-only session facts, no key material anywhere.
    pub fn model(&self) -> &str {
        &self.cfg.model
    }
    pub fn thinking(&self) -> bool {
        self.cfg.thinking
    }
    pub fn max_tokens(&self) -> u32 {
        self.cfg.max_tokens
    }
    pub fn context_window(&self) -> Option<u64> {
        self.cfg.context_window
    }
    pub fn last_context_used(&self) -> Option<u64> {
        self.last_context_used
    }
    pub fn session_usage(&self) -> Usage {
        self.session_usage
    }

    /// Run one user turn to completion (which may involve many provider
    /// round-trips for tool use). Tool failures feed back to the model;
    /// only provider-level failures return `Err`.
    ///
    /// INVARIANT (F7): the CALLER clears the cancel token at submission
    /// time — the component that serializes input (the TUI render thread's
    /// Submit arm; the plain REPL right after `read_input`). `turn` itself
    /// never clears it: a clear here would race an Esc/Ctrl+C that landed
    /// between submission and turn entry and silently drop the interrupt.
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
            let result = self.provider.stream(
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
            );
            if self.cancel.is_set() {
                // Interrupted. An error that raced the cancel still ends the
                // turn as an interruption — the user asked for it to stop —
                // but a REAL failure (pre-stream 401, mid-stream API error)
                // is surfaced in the notice instead of being swallowed (F5).
                self.land_interrupted(result, &mut turn_usage, ui);
                break;
            }
            let msg = result?;

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
                    let mut interrupted = false;
                    for (id, name, input, input_raw) in calls {
                        // Interrupt between calls: results already produced
                        // stay factual; this call and every remaining one get
                        // a synthesized error result in the SAME message, so
                        // the wire rule (every tool_use answered in the next
                        // user message) holds.
                        if self.cancel.is_set() {
                            interrupted = true;
                            results.push(synth_interrupted(&id, &name, ui));
                            continue;
                        }
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

                    if interrupted {
                        self.history.push(RequestMessage {
                            role: Role::User,
                            content: results,
                        });
                        ui(AgentEvent::Notice("turn interrupted".into()));
                        break;
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

    /// T6 landing policy for an interrupted stream: file whatever partial
    /// response arrived on a wire-valid history boundary and close every UI
    /// cell the stream opened, so the driver-loop save that follows persists
    /// a resumable session.
    ///
    /// Kept: completed text, signed thinking, redacted thinking, tool_use
    /// whose arguments parsed. Dropped: tool_use still mid-JSON
    /// (`input_raw`), unsigned thinking (rejected on replay), unknown
    /// blocks. Kept tool_use blocks are answered immediately with
    /// synthesized error results — they were never executed. If nothing
    /// SUBSTANTIVE is kept — no text and no tool_use, e.g. only thinking
    /// blocks (F6) — nothing is pushed: a thinking-only assistant message
    /// is rejected on replay, so history ends with the plain user prompt
    /// and the resume seam's dangling-prompt rule handles it.
    ///
    /// An `Err` landing here means the request had actually failed while
    /// the user interrupted (pre-stream 401, mid-stream API error): the
    /// turn still ends as an interruption, but the notice carries the error
    /// instead of swallowing it (F5). `Incomplete` is the provider's own
    /// "cancelled before anything happened" — not a failure worth wording.
    fn land_interrupted(
        &mut self,
        result: Result<crate::provider::ResponseMessage, ProviderError>,
        turn_usage: &mut Usage,
        ui: &mut dyn FnMut(AgentEvent),
    ) {
        let notice = match &result {
            Ok(_) | Err(ProviderError::Incomplete) => "turn interrupted".to_string(),
            Err(e) => format!("turn interrupted (request had failed: {e})"),
        };
        if let Ok(msg) = result {
            turn_usage.add(&msg.usage);
            self.session_usage.add(&msg.usage);
            // One pass in stream order: keep-or-drop each block, close every
            // tool cell the stream opened — kept AND dropped — preserving
            // the FIFO ToolStart/ToolEnd pairing (docs/TUI.md), and answer
            // each kept tool_use with the synthesized result. A ToolStart
            // only fires once a tool_use block has a name, so an unnamed
            // quirk-server block never opened a cell (and gets no ToolEnd).
            let mut kept: Vec<ContentBlock> = Vec::new();
            let mut results: Vec<ContentBlock> = Vec::new();
            for b in msg.content {
                let keep = match &b {
                    ContentBlock::Text { text } => !text.is_empty(),
                    ContentBlock::Thinking { signature, .. } => signature.is_some(),
                    ContentBlock::RedactedThinking { .. } => true,
                    ContentBlock::ToolUse {
                        id,
                        name,
                        input_raw,
                        ..
                    } => {
                        if input_raw.is_none() {
                            results.push(synth_interrupted(id, name, ui));
                            true
                        } else {
                            // Dropped mid-JSON call: close its cell only.
                            if !name.is_empty() {
                                ui(AgentEvent::ToolEnd {
                                    name: name.clone(),
                                    title: name.clone(),
                                    is_error: true,
                                });
                            }
                            false
                        }
                    }
                    // Impossible in assistant content / never replayed.
                    ContentBlock::ToolResult { .. } | ContentBlock::Unknown => false,
                };
                if keep {
                    kept.push(b);
                }
            }
            let substantive = kept
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { .. } | ContentBlock::ToolUse { .. }));
            if substantive {
                self.history.push(RequestMessage {
                    role: Role::Assistant,
                    content: kept,
                });
                if !results.is_empty() {
                    self.history.push(RequestMessage {
                        role: Role::User,
                        content: results,
                    });
                }
            }
        }
        ui(AgentEvent::Notice(notice));
    }
}

/// F10: the one builder for a synthesized interrupt answer — closes the
/// call's UI cell (when a named block opened one) and returns the
/// [`INTERRUPT_MARKER`] error result the never-executed call is answered
/// with. Every synthesis site goes through here.
fn synth_interrupted(id: &str, name: &str, ui: &mut dyn FnMut(AgentEvent)) -> ContentBlock {
    if !name.is_empty() {
        ui(AgentEvent::ToolEnd {
            name: name.to_string(),
            title: name.to_string(),
            is_error: true,
        });
    }
    ContentBlock::ToolResult {
        tool_use_id: id.to_string(),
        content: INTERRUPT_MARKER.into(),
        is_error: true,
    }
}
