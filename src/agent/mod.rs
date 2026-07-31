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
    /// Where `max_tokens` came from (T16): the active profile's name, or
    /// `None` for the base config. Display only — the truncation notice
    /// names the limit's source so the fix is findable.
    pub max_tokens_source: Option<String>,
    /// T19 P3 (recorded amendment to T4's "prose is never executed"
    /// policy): execute an UNAMBIGUOUS tool call written as plain text.
    /// `false` restores detect+nudge exactly.
    pub prose_tool_calls: bool,
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
            max_tokens_source: None,
            prose_tool_calls: cfg.prose_tool_calls,
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

/// Header line the compacted-summary text block starts with (T20). The next
/// request's first user message begins with this, so a transcript reader
/// (human or model) can tell summary from live conversation.
pub const COMPACT_SUMMARY_HEADER: &str = "[conversation summary (compacted)]";

/// The final user instruction the `/compact` summary call appends (T20).
/// Structured headings by design: small local models write poor freeform
/// summaries, and the headings force the facts a continuation needs.
const COMPACT_INSTRUCTION: &str = "Summarize this conversation so that work can \
continue from the summary alone. Reply with ONLY the summary, under these exact \
headings:\n\
Goal: what the user is trying to accomplish\n\
State: what has been done and what is true right now\n\
Decisions: choices made, and why\n\
Files: files read, created, or modified, with paths\n\
Next steps: what remains to be done\n\
Be specific: name files, commands, and values. The full conversation is about \
to be discarded; anything not in the summary is lost.";

/// What [`Session::compact`] did. The command layer words the notices; the
/// variants carry only facts.
#[derive(Debug, PartialEq, Eq)]
pub enum CompactOutcome {
    /// Empty history: no call was made.
    Nothing,
    /// The user interrupted the summary call; history untouched.
    Cancelled,
    /// Provider error or empty summary; history untouched. The payload is
    /// the human-readable reason.
    Failed(String),
    /// History replaced. Message counts, for the notice.
    Compacted { before: usize, after: usize },
}

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
        mut registry: Registry,
        cfg: SessionConfig,
        seed: Option<SessionSeed>,
    ) -> Self {
        // T19: the per-result output cap scales to the active window. Set
        // here and in `switch_provider`, the same two moments the T18
        // redaction key is registered, so no construction path can skip it.
        registry.set_context_window(cfg.context_window);
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

    /// Install the key-file guard (T18), called once at startup right after
    /// construction. Config-derived and covering EVERY configured key file,
    /// so a `/model` switch never needs to change it. A setter (not a
    /// constructor parameter) so `ToolCtx::new` keeps yielding the empty
    /// guard and every guard-free construction stays byte-identical.
    /// `allow_unsandboxed_bash` is the config's escape hatch for hosts
    /// without unprivileged user namespaces; it travels with the guard
    /// because it means nothing without one.
    pub fn set_key_guard(&mut self, guard: crate::tools::KeyGuard, allow_unsandboxed_bash: bool) {
        self.tool_ctx.guard = guard;
        self.tool_ctx.allow_unsandboxed_bash = allow_unsandboxed_bash;
    }

    /// Install the interactive bash approver (T21), in the style of
    /// [`Session::set_key_guard`]: a setter, not a constructor parameter,
    /// so `ToolCtx::new` keeps its `None` default and every
    /// approver-free construction stays byte-identical. Installed ONLY by
    /// an interactive UI (TUI, or the plain REPL on a real terminal);
    /// one-shot -p and piped stdin never install one, so their Ask arm
    /// stays a refusal.
    pub fn set_bash_approver(&mut self, approver: Box<dyn FnMut(&str) -> bool>) {
        self.tool_ctx.bash_approver = Some(approver);
    }

    /// Register the ACTIVE provider's credential for tool-output redaction
    /// (T18 layer 3), or clear it with `None`. Called at startup and after
    /// every successful provider switch, with the very string the build
    /// already read: no extra key read ever happens for redaction.
    pub fn set_redaction_key(&mut self, key: Option<String>) {
        self.registry.set_redaction_key(key);
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
        max_tokens_source: Option<String>,
    ) {
        self.provider = provider;
        self.cfg.model = model;
        self.cfg.max_tokens = max_tokens;
        self.cfg.context_window = context_window;
        self.cfg.max_tokens_source = max_tokens_source;
        self.context_warned = false;
        // T19: the output cap follows the new window (see `build`).
        self.registry.set_context_window(context_window);
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

    /// Swap in a saved session (T10 `/resume`): the resume constructor's
    /// work applied to a LIVE session between turns. Replaces history, usage
    /// totals, todos, and the context estimate; re-arms the context
    /// pre-warning (it was about the old conversation). Provider, model, and
    /// config stay — resuming a session never switches providers. Infallible
    /// by the same construction as [`Session::resume`]: every replay-safety
    /// decision was already made in `session_store::prepare_seed`.
    pub fn load_seed(&mut self, seed: SessionSeed) {
        self.history = seed.history;
        self.session_usage = seed.session_usage;
        self.tool_ctx.todos = seed.todos;
        self.last_context_used = seed.last_context_used;
        self.context_warned = false;
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

    /// Compact the conversation (`/compact`, T20): ONE provider call
    /// summarizes the history, then the history is replaced by that summary
    /// plus a verbatim tail (see [`compact_tail_start`]). FAIL-CLOSED: the
    /// replacement happens only after a completed, successful response with
    /// non-empty text; any error, cancellation, or empty summary returns
    /// with history untouched.
    ///
    /// The summary request is the CURRENT history plus a final user
    /// instruction, with tools omitted entirely (no tool_use possible), on
    /// the session's own model, max_tokens, and system prompt. Cancel works
    /// like a turn: the provider stack polls the same token.
    ///
    /// After success: `last_context_used` is cleared (the estimate described
    /// the old conversation), the context advisory re-arms, session usage
    /// totals KEEP accumulating (the summary call's own usage is real spend),
    /// and todos are untouched. Saving is the caller's job, same as `/clear`.
    pub fn compact(&mut self) -> CompactOutcome {
        if self.history.is_empty() {
            return CompactOutcome::Nothing;
        }
        // Alternation-safe instruction: history normally ends with an
        // assistant message and the instruction is a new user message; when
        // it already ends with a user message (dangling prompt after a
        // provider-error turn), the instruction joins that message as an
        // extra text block instead. The Anthropic wire rejects two
        // consecutive user messages.
        let mut messages = self.history.clone();
        let instruction = ContentBlock::Text {
            text: COMPACT_INSTRUCTION.to_string(),
        };
        match messages.last_mut() {
            Some(m) if m.role == Role::User => m.content.push(instruction),
            _ => messages.push(RequestMessage {
                role: Role::User,
                content: vec![instruction],
            }),
        }
        let req = ChatRequest {
            model: self.cfg.model.clone(),
            max_tokens: self.cfg.max_tokens,
            system: self.cfg.system.clone(),
            thinking: self.cfg.thinking,
            temperature: self.cfg.temperature.map(f64::from),
            top_p: self.cfg.top_p.map(f64::from),
            messages,
            tools: Vec::new(),
        };
        // Deltas are dropped: the summary is bookkeeping, not conversation,
        // and the notice reports the outcome when the call lands.
        let result = self.provider.stream(&req, &mut |_| {}, &self.cancel);
        if self.cancel.is_set() {
            // Interrupted like a turn. A partial that did arrive still cost
            // real tokens, so its usage is recorded; history stays put.
            if let Ok(msg) = &result {
                self.session_usage.add(&msg.usage);
            }
            return CompactOutcome::Cancelled;
        }
        let msg = match result {
            Ok(m) => m,
            Err(e) => return CompactOutcome::Failed(e.to_string()),
        };
        self.session_usage.add(&msg.usage);
        // Concatenated Text blocks only; Thinking blocks are ignored.
        let summary = msg
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let summary = summary.trim();
        if summary.is_empty() {
            return CompactOutcome::Failed("the model returned an empty summary".into());
        }
        let before = self.history.len();
        self.history = compacted_history(summary, &self.history);
        let after = self.history.len();
        self.last_context_used = None;
        self.context_warned = false;
        CompactOutcome::Compacted { before, after }
    }

    /// Flip adaptive thinking for THIS session (`/thinking`); the config
    /// default is untouched.
    pub fn set_thinking(&mut self, on: bool) {
        self.cfg.thinking = on;
    }

    /// The unified context advisory (T20). Fires ONCE per latch period,
    /// when either arm crosses first: `used >= 80%` of the window, or the
    /// remaining window is smaller than `max_tokens` (the next response may
    /// not fit). Returns the advisory and sets the latch; `None` below
    /// threshold, when already warned, or without a window/estimate (no
    /// configured `context_window` means no advisory can fire, as before).
    ///
    /// Two trigger paths call it: the turn loop after each response, and
    /// the resume seams (startup `--continue`/`--resume`, `/resume`) right
    /// after a seed load, because resume is the zero-waste moment to
    /// compact: no provider cache prefix is warm yet. The latch is shared,
    /// so a resume-time advisory suppresses the turn-loop one and vice
    /// versa, and every existing re-arm point (provider switch, `/clear`,
    /// seed load, `/compact`) resets both.
    pub fn context_advisory(&mut self) -> Option<String> {
        if self.context_warned {
            return None;
        }
        let window = self.cfg.context_window?;
        let used = self.last_context_used?;
        let eighty = u128::from(used) * 5 >= u128::from(window) * 4;
        let tight = window.saturating_sub(used) < u64::from(self.cfg.max_tokens);
        if !(eighty || tight) {
            return None;
        }
        self.context_warned = true;
        Some(format!(
            "context: ~{used} of {window} tokens used; /compact frees the window by summarizing the conversation, or start a new session"
        ))
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
            if let Some(advisory) = self.context_advisory() {
                ui(AgentEvent::Notice(advisory));
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
                    // Text-tool-call recovery (T4 + T19 P3): an EndTurn
                    // whose message made no structured calls but *reads*
                    // like a tool call. T19 P3 amends T4's "prose is never
                    // parsed into an execution" NARROWLY (recorded in the
                    // RUNBOOK next to the T4 policy history): when the text
                    // is an UNAMBIGUOUS call (exactly one candidate,
                    // lossless inner JSON, registered tool, object args)
                    // and `prose_tool_calls` is on, it executes through
                    // Registry::execute exactly like a structured call
                    // (T18 guard, redaction, and the T19 truncation all
                    // apply by construction). Anything short of that
                    // contract nudges, exactly as before; the config off
                    // switch restores detect+nudge byte-identically.
                    let mut prose_call: Option<recover::ProseCall> = None;
                    let mut nudge = false;
                    if matches!(other, Some(StopReason::EndTurn))
                        && nudges < NUDGE_LIMIT
                        && !content
                            .iter()
                            .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
                    {
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
                        if self.cfg.prose_tool_calls {
                            prose_call =
                                recover::extract_prose_tool_call(&text, &tool_names);
                        }
                        if prose_call.is_none() {
                            nudge = recover::detect_text_tool_call(&text, &tool_names);
                        }
                    }
                    self.history.push(RequestMessage {
                        role: Role::Assistant,
                        content,
                    });
                    if let Some(call) = prose_call {
                        // No tool_use id exists, so the result goes back as
                        // PLAIN USER TEXT, wire-legal on both providers,
                        // request-body goldens untouched. No ToolEnd event:
                        // no stream ever opened a tool cell for this call
                        // (the TUI's FIFO ToolStart/ToolEnd pairing holds).
                        let name = call.name.clone();
                        let feedback = match self
                            .registry
                            .execute(&call.name, call.args, &mut self.tool_ctx)
                        {
                            Ok(out) => {
                                ui(AgentEvent::Notice(format!(
                                    "prose-call recovery: executed the {name} tool call the model wrote as plain text"
                                )));
                                format!(
                                    "Result of the {name} tool call you wrote as text (executed by prose-call recovery):\n{}",
                                    out.output
                                )
                            }
                            Err(e) => {
                                // A failed prose execution counts toward
                                // the nudge cap so a stuck model still
                                // terminates; successes are uncapped.
                                nudges += 1;
                                ui(AgentEvent::Notice(format!(
                                    "prose-call recovery: the {name} tool call the model wrote as plain text failed; fed the error back"
                                )));
                                format!(
                                    "Error result of the {name} tool call you wrote as text (executed by prose-call recovery):\n{e}"
                                )
                            }
                        };
                        self.history.push(RequestMessage {
                            role: Role::User,
                            content: vec![ContentBlock::Text { text: feedback }],
                        });
                        continue;
                    }
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
                                // T16: name the limit and where it came
                                // from, so the fix is findable without
                                // guessing which config knob applied.
                                let source = match &self.cfg.max_tokens_source {
                                    Some(p) => format!("from profile {p:?}"),
                                    None => "from config".into(),
                                };
                                ui(AgentEvent::Notice(format!(
                                    "response truncated: max_tokens ({}, {source}) reached; raise max_tokens in config.json",
                                    self.cfg.max_tokens
                                )));
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

/// T20 verbatim-tail boundary: the index of the LAST user message that
/// contains no ToolResult block. Everything from there to the end of history
/// is kept verbatim (the last user-initiated exchange), which can never
/// split a tool_use/tool_result pair: every tool_result lives in a user
/// message that HAS one, so the pair sits entirely inside the tail. `None`
/// means no such message exists and the summary stands alone.
fn compact_tail_start(history: &[RequestMessage]) -> Option<usize> {
    history.iter().rposition(|m| {
        m.role == Role::User
            && !m
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
    })
}

/// T20 merge: the new history after a successful compact. Alternation-safe
/// on BOTH wires by construction: with a tail, the summary is PREPENDED as
/// a leading Text block inside the tail's first user message (never a new
/// message, so no two consecutive user messages exist); with no tail, the
/// history becomes one user message holding the summary.
fn compacted_history(summary: &str, history: &[RequestMessage]) -> Vec<RequestMessage> {
    let summary_text = format!("{COMPACT_SUMMARY_HEADER}\n{summary}");
    match compact_tail_start(history) {
        Some(i) => {
            let mut first = history[i].clone();
            let mut content = Vec::with_capacity(first.content.len() + 1);
            content.push(ContentBlock::Text { text: summary_text });
            content.append(&mut first.content);
            first.content = content;
            let mut out = Vec::with_capacity(history.len() - i);
            out.push(first);
            out.extend(history[i + 1..].iter().cloned());
            out
        }
        None => vec![RequestMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text: summary_text }],
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_text(t: &str) -> RequestMessage {
        RequestMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text: t.into() }],
        }
    }

    fn assistant_text(t: &str) -> RequestMessage {
        RequestMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: t.into() }],
        }
    }

    fn assistant_tool_use(id: &str) -> RequestMessage {
        RequestMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.into(),
                name: "read".into(),
                input: serde_json::json!({}),
                input_raw: None,
            }],
        }
    }

    fn user_tool_result(id: &str) -> RequestMessage {
        RequestMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.into(),
                content: "result".into(),
                is_error: false,
            }],
        }
    }

    #[test]
    fn tail_starts_at_last_plain_user_message() {
        // u a u(toolresult-free) a(tool_use) u(tool_result) a: the tail must
        // start at index 2, keeping the whole final exchange including its
        // tool_use/tool_result pair.
        let h = vec![
            user_text("one"),
            assistant_text("a1"),
            user_text("two"),
            assistant_tool_use("tu_1"),
            user_tool_result("tu_1"),
            assistant_text("a2"),
        ];
        assert_eq!(compact_tail_start(&h), Some(2));
    }

    #[test]
    fn tool_result_only_user_messages_never_start_the_tail() {
        // The only user messages after index 0 carry tool_results; the tail
        // must reach back to the plain prompt at 0.
        let h = vec![
            user_text("go"),
            assistant_tool_use("tu_1"),
            user_tool_result("tu_1"),
            assistant_tool_use("tu_2"),
            user_tool_result("tu_2"),
            assistant_text("done"),
        ];
        assert_eq!(compact_tail_start(&h), Some(0));
    }

    #[test]
    fn mixed_user_message_with_a_tool_result_is_not_a_boundary() {
        // A user message holding text AND a tool_result is still an answer
        // to a tool_use: cutting there would split the pair.
        let mixed = RequestMessage {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "tu_1".into(),
                    content: "r".into(),
                    is_error: false,
                },
                ContentBlock::Text { text: "note".into() },
            ],
        };
        let h = vec![user_text("go"), assistant_tool_use("tu_1"), mixed];
        assert_eq!(compact_tail_start(&h), Some(0));
    }

    #[test]
    fn no_plain_user_message_means_no_tail() {
        let h = vec![user_tool_result("tu_0"), assistant_text("a")];
        assert_eq!(compact_tail_start(&h), None);
        let merged = compacted_history("S", &h);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].role, Role::User);
        match &merged[0].content[..] {
            [ContentBlock::Text { text }] => {
                assert!(text.starts_with(COMPACT_SUMMARY_HEADER));
                assert!(text.ends_with("\nS"));
            }
            other => panic!("summary-only message expected: {other:?}"),
        }
    }

    #[test]
    fn empty_history_has_no_tail() {
        assert_eq!(compact_tail_start(&[]), None);
    }

    #[test]
    fn single_exchange_history_keeps_itself_as_the_tail() {
        let h = vec![user_text("only"), assistant_text("reply")];
        assert_eq!(compact_tail_start(&h), Some(0));
        let merged = compacted_history("S", &h);
        assert_eq!(merged.len(), 2);
        // Summary prepended INSIDE the first user message: one leading Text
        // block, then the original content, roles alternating as before.
        match &merged[0].content[..] {
            [ContentBlock::Text { text: s }, ContentBlock::Text { text: orig }] => {
                assert!(s.starts_with(COMPACT_SUMMARY_HEADER));
                assert_eq!(orig, "only");
            }
            other => panic!("merged first message: {other:?}"),
        }
        assert_eq!(merged[1], assistant_text("reply"));
    }

    #[test]
    fn merge_never_creates_consecutive_user_messages() {
        let h = vec![
            user_text("one"),
            assistant_text("a1"),
            user_text("two"),
            assistant_tool_use("tu_1"),
            user_tool_result("tu_1"),
            assistant_text("a2"),
        ];
        let merged = compacted_history("S", &h);
        assert_eq!(merged.len(), 4); // tail from index 2, summary inside its head
        assert_eq!(merged[0].role, Role::User);
        for pair in merged.windows(2) {
            assert!(
                !(pair[0].role == Role::User && pair[1].role == Role::User),
                "consecutive user messages after merge"
            );
        }
        // The tool_use/tool_result pair survived intact.
        assert_eq!(merged[1], assistant_tool_use("tu_1"));
        assert_eq!(merged[2], user_tool_result("tu_1"));
    }
}
