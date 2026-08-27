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

/// T36 futile-call guard: a dispatched call whose fingerprint AND result
/// both repeat an earlier call from the SAME turn gained zero information.
///
/// The discriminator is lack of PROGRESS, not repetition. A model editing
/// ten files legitimately rotates through the same few tools, so counting
/// repetition alone would punish real work; what cannot be real work is
/// re-running a call that already returned this exact result. temur is
/// single-threaded: between two calls in one turn nothing the agent did
/// can have changed the answer unless a call in between changed it, and
/// then the result differs and this never counts.
///
/// The counter is GLOBAL to the turn, not consecutive: the archived loop
/// rotated through six distinct calls, so any consecutive rule launders
/// itself, and one timestamp-varying call in the rotation must not reset
/// the count for the rest.
///
/// Evidence (`~/temur-eval-archive/llama32-coercion-2026-08-16`,
/// `task8.run1`, captured 2026-08-16): 77 tool calls in one task with
/// ZERO identical-consecutive pairs, cycling `read` offset `"0"` x19,
/// `gunzip -c` x15, `read` offset `"1"` x14, `zcat` x9 plus `cat`,
/// `gunzip` and `write`, ending at the context window on 440,983 input
/// tokens. All three existing guards provably non-firing on that shape
/// (verified 2026-08-17): no identical-consecutive pair for the doom-loop
/// guard, no strict `A,B,A,B,A,B` six-window for the alternating-pair
/// guard, and `ProseRepeatGuard` is the prose path only.
///
/// The honest false positive: a model deliberately POLLING an external
/// condition (waiting on a file another process writes, a server coming
/// up) whose answer has not changed yet. That is rare in this product
/// (nothing runs between turns; only a tool the model itself called can
/// change anything), and it is why the first response is a NOTICE that
/// names the situation rather than a stop. The guard applies to
/// structured `tool_use` dispatches; the prose path keeps its own
/// `ProseRepeatGuard`.
///
/// The gap between the two thresholds is deliberate headroom: the notice
/// needs a real chance to work before the hard stop, and a long
/// legitimate turn with a few incidental re-reads must never come near
/// 18.
const FUTILE_NOTICE_THRESHOLD: u32 = 6;
const FUTILE_STOP_THRESHOLD: u32 = 18;

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
    /// T26: the list rates the mid-session cost advisory computes at, or
    /// `None` when this selection can show no estimate at all (keyless or
    /// unpriced). A property of the SELECTION, not of temur, so it travels
    /// exactly like `context_window`: main.rs sets it at startup from the
    /// resolved profile, and [`Session::switch_provider`] replaces it.
    pub cost_rates: Option<crate::cost::CostRates>,
    /// T26: dollar step between mid-session cost advisories; `0` disables
    /// them. Already validated (see `Config::cost_advisory_step_usd`).
    pub cost_advisory_step_usd: f64,
    /// T40: compact the session automatically at the next safe point when
    /// the context advisory fires, instead of only advising. Already
    /// resolved against the invocation mode (see
    /// `Config::auto_compact_enabled`), so the core sees a plain `bool`.
    pub auto_compact: bool,
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
            // A property of the SELECTION, like context_window above:
            // main.rs sets it from the resolved profile.
            cost_rates: None,
            // The default step, so a session built straight from a Config
            // still has the advisory armed. main.rs overwrites this with the
            // VALIDATED value, which is where a bad step becomes a startup
            // error instead of a silent fallback.
            cost_advisory_step_usd: crate::config::DEFAULT_COST_ADVISORY_STEP_USD,
            // The interactive default. main.rs overwrites this with the
            // mode-resolved value; a session built straight from a Config
            // (tests, embedders) gets the conservative arm: advise, never
            // spend a summary call nobody asked for.
            auto_compact: false,
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
    /// T26: the highest cost-advisory step multiple already accounted for.
    /// Reset (recomputed from the current estimate, never to a bare 0) at
    /// every point where the money already spent must stop being new news:
    /// session creation, seed load, `/clear`, and a provider switch. NOT
    /// persisted, and deliberately so: it is a pure function of the usage
    /// totals and the rates, both of which the session already has, so the
    /// session file format is untouched.
    cost_latch: u64,
    /// T6 cooperative interruption. The UI holds a clone (via
    /// [`Session::cancel_token`]) and sets it; the provider stack polls it.
    cancel: CancelToken,
    /// T40 P2: where this session writes itself. `None` = never persist.
    persist: Option<PersistTarget>,
    /// T40 P2: the save-failure notice is once per PROCESS, not per write.
    /// It lives here rather than in the caller because mid-turn writes and
    /// the end-of-turn write must share one latch: a full disk should say
    /// so once, not once per round-trip.
    save_failure_notified: bool,
}

/// The cost-advisory latch value for a session holding `usage` under `cfg`:
/// the whole steps that spend already covers, and `0` when no estimate can
/// be computed (unpriced, keyless, or nothing reported yet). A free function
/// because [`Session::build`] needs it before a `Session` exists.
fn cost_latch_for(cfg: &SessionConfig, usage: &Usage) -> u64 {
    match cfg.cost_rates.as_ref().and_then(|r| r.estimate(usage)) {
        Some(estimate) => crate::cost::step_multiple(estimate, cfg.cost_advisory_step_usd),
        None => 0,
    }
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

/// T40: the most auto-compactions one turn may perform. A bound, not a
/// target: each one is a real provider call, and a turn that needs a fourth
/// is not going to be rescued by it. On the fourth need the plain advisory
/// is emitted instead, byte-identical to today, and the request goes out as
/// it would have, which may 400, and that is the honest outcome.
const MAX_AUTO_COMPACTIONS_PER_TURN: u32 = 3;

/// T40: how many completed round-trips the auto-compaction tail keeps
/// verbatim. Two is the immediate working state (the last tool call and
/// its result, plus the one before it for context), small enough that the
/// summary does the real work.
const AUTO_COMPACT_TAIL_ROUND_TRIPS: usize = 2;

/// T40 auto-compaction's summary instruction: a sibling of
/// [`COMPACT_INSTRUCTION`], not a reuse. Two differences matter. The
/// conversation is NOT about to be discarded (the task prompt and the last
/// round-trips survive verbatim), so promising that would be a lie a small
/// model acts on; and this fires MID-TASK, so what a continuation needs is
/// working state, not a conversation recap.
const AUTO_COMPACT_INSTRUCTION: &str = "You are running low on context. Summarize the \
work done so far on this task so that it can continue from the summary alone. Reply \
with ONLY the summary, under these exact headings:\n\
State: what has been done and what is true right now\n\
Files: files read, created, or modified, with paths\n\
Findings: what has been learned that the remaining work depends on\n\
Remaining: what still has to be done\n\
Be specific: name files, commands, and values. The middle of this conversation is \
about to be replaced by this summary; the original task and the most recent steps \
are kept.";

/// T40: the user half of the summary pair. The summary is spoken by the
/// ASSISTANT (it is the assistant's own account of its work), so the wire's
/// alternation rule needs a user message before the retained tail resumes.
/// It does double duty as the instruction to carry on.
const AUTO_COMPACT_RESUME: &str = "That summary replaces the earlier steps of this \
task. Continue the task from it and from the most recent steps below.";

/// T40 P2: where and how a session persists ITSELF. `None` on the session
/// means never persist, which is what the `--mock` replay paths and any
/// embedder without a session file get.
///
/// The provider/model/cwd fields describe what the NEXT write records, and
/// the REPL refreshes them before every turn: a session saved after a
/// `/model` switch must describe what is actually active.
pub struct PersistTarget {
    pub path: std::path::PathBuf,
    pub provider: String,
    pub model: String,
    pub cwd_display: String,
    pub name: Option<String>,
    /// Byte cap for the file on disk, checked by `session_store::save`
    /// against the SERIALIZED length. `u64` end to end: this is a file
    /// size, and `usize` is 32-bit on the shipped target.
    pub max_bytes: u64,
}

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
        // T26: a resumed session starts latched at whatever it already
        // spent, so only NEW spend can advise. A fresh session's usage is
        // zero, which lands the same call on 0.
        let cost_latch = cost_latch_for(&cfg, &session_usage);
        Session {
            provider,
            registry,
            tool_ctx,
            cfg,
            history,
            session_usage,
            last_context_used,
            context_warned: false,
            cost_latch,
            cancel,
            // T40 P2: installed by the caller that owns a session file.
            persist: None,
            save_failure_notified: false,
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

    /// T40 P2: install (or clear) the persist target. Called before every
    /// turn by the caller that owns the session file, so a `/model` switch,
    /// a `/resume`, or a `/new` between turns is reflected in what the next
    /// write records.
    pub fn set_persist_target(&mut self, target: Option<PersistTarget>) {
        self.persist = target;
    }

    /// T40 P2: write the session file NOW, reflecting history so far.
    ///
    /// Called after every round-trip inside a turn (once when the assistant
    /// message is appended, once when its tool results are), and once more
    /// at turn end. Before T40 the only write was the one at turn end, so a
    /// SIGKILL during a long agentic turn lost the whole turn; now it loses
    /// at most one request.
    ///
    /// Never fatal, exactly as the turn-end save was never fatal: the
    /// in-memory conversation is intact and the next write retries. The
    /// failure notice is latched once per process (see
    /// `save_failure_notified`) so a full disk does not shout on every
    /// round-trip.
    pub fn persist_now(&mut self, ui: &mut dyn FnMut(AgentEvent)) {
        let Some(target) = self.persist.as_ref() else {
            return;
        };
        let file = crate::session_store::SessionFileRef {
            version: crate::session_store::FORMAT_VERSION,
            provider: &target.provider,
            model: &target.model,
            cwd: &target.cwd_display,
            history: &self.history,
            session_usage: self.session_usage,
            todos: &self.tool_ctx.todos,
            last_context_used: self.last_context_used,
            name: target.name.as_deref(),
        };
        let mut trim_notices: Vec<String> = Vec::new();
        let result = crate::session_store::save(&target.path, &file, target.max_bytes, &mut |n| {
            trim_notices.push(n)
        });
        match result {
            Ok(()) => {
                for n in trim_notices {
                    ui(AgentEvent::Notice(n));
                }
            }
            Err(e) => {
                if !self.save_failure_notified {
                    self.save_failure_notified = true;
                    ui(AgentEvent::Notice(format!(
                        "session save failed: {e} — continuing; will retry next turn"
                    )));
                }
            }
        }
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
    ///
    /// `selection` is the whole profile being switched onto, not a handful
    /// of its fields: a switch replaces the ENTIRE selection, so passing the
    /// struct makes that rule structural, and the next selection-scoped
    /// setting costs no signature change here or at any call site. Both
    /// production callers already hold the exact value.
    ///
    /// `max_tokens_source` stays separate because it is not a profile field:
    /// it is the NAME under which the selection is active, which the caller
    /// alone knows (a profile activation passes the profile name, a raw
    /// model override keeps whatever name was already active).
    pub fn switch_provider(
        &mut self,
        provider: Box<dyn Provider>,
        selection: &crate::config::ResolvedProfile,
        max_tokens_source: Option<String>,
    ) {
        self.provider = provider;
        self.cfg.model = selection.model.clone();
        self.cfg.max_tokens = selection.max_tokens;
        self.cfg.context_window = selection.context_window;
        self.cfg.max_tokens_source = max_tokens_source;
        self.cfg.cost_rates = crate::cost::CostRates::for_profile(selection);
        self.context_warned = false;
        // T26: the rates just changed, so the money already spent must be
        // re-measured against them before anything new can advise. Without
        // this, switching onto a pricier profile would advise about spend
        // that happened at the old rates.
        self.reset_cost_latch();
        // T19: the output cap follows the new window (see `build`).
        self.registry.set_context_window(selection.context_window);
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
        // T26: the restored totals are spend that already happened; resuming
        // an expensive session must not replay its advisories.
        self.reset_cost_latch();
    }

    /// Wipe the conversation (`/clear`): history, usage totals, context
    /// estimate, warning latch, and todos. Provider, model, and config stay.
    pub fn clear_history(&mut self) {
        self.history.clear();
        self.session_usage = Usage::default();
        self.last_context_used = None;
        self.context_warned = false;
        // T26: usage went to zero here, so the latch follows it back to zero
        // and the next $5 of a cleared session advises again.
        self.reset_cost_latch();
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
        let summary = match self.summarize(COMPACT_INSTRUCTION) {
            Ok(s) => s,
            Err(outcome) => return outcome,
        };
        let before = self.history.len();
        self.history = compacted_history(&summary, &self.history);
        let after = self.history.len();
        self.last_context_used = None;
        self.context_warned = false;
        CompactOutcome::Compacted { before, after }
    }

    /// T40 auto-compaction: the same fail-closed summary call as
    /// [`Session::compact`], merged by [`auto_compacted_history`] instead.
    /// Called from the turn loop at the safe point, never by a command.
    ///
    /// `turn_start` is the index of the CURRENT turn's user prompt, which
    /// the caller captured when it pushed it. `CompactOutcome::Nothing`
    /// means the turn is too short to fold (invariant (e)); the caller
    /// advises instead and counts it against the bound.
    pub fn auto_compact(&mut self, turn_start: usize) -> CompactOutcome {
        let Some(tail_start) =
            auto_compact_tail_start(&self.history, turn_start, AUTO_COMPACT_TAIL_ROUND_TRIPS)
        else {
            return CompactOutcome::Nothing;
        };
        let summary = match self.summarize(AUTO_COMPACT_INSTRUCTION) {
            Ok(s) => s,
            Err(outcome) => return outcome,
        };
        let before = self.history.len();
        self.history = auto_compacted_history(&summary, &self.history, turn_start, tail_start);
        let after = self.history.len();
        // Same reset as `/compact`: the estimate described the old
        // conversation, and the advisory re-arms so a turn that fills the
        // window AGAIN can compact again, up to the bound.
        self.last_context_used = None;
        self.context_warned = false;
        CompactOutcome::Compacted { before, after }
    }

    /// The summary provider call shared by `/compact` (T20) and T40
    /// auto-compaction. ONE call: the current history plus `instruction` as
    /// a final user message, tools omitted entirely (no `tool_use`
    /// possible), on the session's own model, `max_tokens`, and system
    /// prompt. Cancel works like a turn: the provider stack polls the same
    /// token.
    ///
    /// FAIL-CLOSED: every `Err` arm carries the outcome the caller returns
    /// unchanged, and no arm touches history: the replacement is the
    /// caller's job, and only after a completed, non-empty summary.
    fn summarize(&mut self, instruction: &str) -> Result<String, CompactOutcome> {
        // Alternation-safe instruction: history normally ends with an
        // assistant message and the instruction is a new user message; when
        // it already ends with a user message (dangling prompt after a
        // provider-error turn), the instruction joins that message as an
        // extra text block instead. The Anthropic wire rejects two
        // consecutive user messages.
        let mut messages = self.history.clone();
        let instruction = ContentBlock::Text {
            text: instruction.to_string(),
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
                self.accrue_usage(&msg.usage);
            }
            return Err(CompactOutcome::Cancelled);
        }
        let msg = match result {
            Ok(m) => m,
            Err(e) => return Err(CompactOutcome::Failed(e.to_string())),
        };
        self.accrue_usage(&msg.usage);
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
            return Err(CompactOutcome::Failed(
                "the model returned an empty summary".into(),
            ));
        }
        Ok(summary.to_string())
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
        let (used, window) = self.context_trigger()?;
        Some(context_advisory_text(used, window))
    }

    /// The advisory TRIGGER, factored out of [`Session::context_advisory`]
    /// for T40: the firing CONDITION and the latch, without the wording.
    /// Returns `(used, window)` on the round-trip that crosses, `None`
    /// otherwise, and sets the latch exactly where the advisory used to set
    /// it, so a caller that auto-compacts and a caller that advises are
    /// latch-equivalent, and the two can never both speak about one
    /// crossing.
    fn context_trigger(&mut self) -> Option<(u64, u64)> {
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
        Some((used, window))
    }

    /// The mid-session cost advisory (T26). Fires when the session estimate
    /// crosses a NEW multiple of `cost_advisory_step_usd` ($5, $10, $15, ...
    /// at the default step), and moves the latch to that multiple. `None`
    /// below the next multiple, when the step is `0` (disabled), and
    /// whenever no estimate can be computed at all: an unpriced, keyless, or
    /// local selection never sees this, because it has no `cost_rates`.
    ///
    /// The estimate itself is the same number `/status` shows, through the
    /// same [`crate::cost::CostRates`] gate; this method only decides when
    /// to say it unprompted. A jump that clears several multiples at once
    /// advises ONCE at the highest (the motivating incident was a single
    /// agentic turn that ran to roughly $26 unnoticed, and five lines in a
    /// row would have been worse than one).
    ///
    /// Because the latch only ever moves forward, calling this more than
    /// once for the same spend is harmless. That is what lets every point
    /// where usage accrues be covered without coordinating them: the turn
    /// loop after each response, the interrupted-turn landing, and the
    /// command layer after `/compact`'s own summary call.
    pub fn cost_advisory(&mut self) -> Option<String> {
        let step = self.cfg.cost_advisory_step_usd;
        let estimate = self.cfg.cost_rates.as_ref()?.estimate(&self.session_usage)?;
        let crossed = crate::cost::advisory_crossing(estimate, step, self.cost_latch)?;
        self.cost_latch = crossed;
        Some(crate::cost::advisory_message(crossed, step, estimate))
    }

    /// Re-latch to the spend already on the books, so nothing already spent
    /// can advise. The between-turns seams call this wherever the usage
    /// totals or the rates change under the session (see `cost_latch`).
    fn reset_cost_latch(&mut self) {
        self.cost_latch = cost_latch_for(&self.cfg, &self.session_usage);
    }

    /// Record provider-reported spend. THE accrual point: every response
    /// that costs money lands here, so the cost advisory's coverage argument
    /// is one grep, not four. Emitting the advisory is the caller's job,
    /// because only the caller knows whether it has a UI sink to emit into.
    fn accrue_usage(&mut self, usage: &Usage) {
        self.session_usage.add(usage);
    }

    /// The "response truncated" notice. Shared by the plain MaxTokens stop
    /// and the T13 F10 case where one response BOTH assembled tool calls and
    /// hit the limit: there the neutral stop reason is ToolUse (the calls
    /// must still run) and the truncation arrives in `stop_details`, so the
    /// user hears about it either way. Wording is identical in both, and
    /// identical to the pre-T13 strings.
    fn truncation_notice(&self) -> String {
        // Near the configured window, max_tokens is the symptom, overflow
        // the likely cause. Providers stay faithful wire mappers; this
        // heuristic lives here. Without a window (or without usage) the
        // wording is EXACTLY the old string.
        let near_window = match (self.cfg.context_window, self.last_context_used) {
            (Some(window), Some(used)) => used + u64::from(self.cfg.max_tokens) >= window,
            _ => false,
        };
        if near_window {
            let used = self.last_context_used.unwrap_or(0);
            let window = self.cfg.context_window.unwrap_or(0);
            format!(
                "response truncated: max_tokens reached near the context window (~{used} of {window} tokens) — likely context overflow; consider starting a new session"
            )
        } else {
            // T16: name the limit and where it came from, so the fix is
            // findable without guessing which config knob applied.
            let source = match &self.cfg.max_tokens_source {
                Some(p) => format!("from profile {p:?}"),
                None => "from config".into(),
            };
            format!(
                "response truncated: max_tokens ({}, {source}) reached; raise max_tokens in config.json",
                self.cfg.max_tokens
            )
        }
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
        // T40: where this turn's prompt sits. Auto-compaction keeps exactly
        // this message verbatim and folds what follows, so the index is
        // captured at the one moment it is unambiguous.
        let turn_start = self.history.len() - 1;

        let mut turn_usage = Usage::default();
        let mut iterations: u32 = 0;
        let mut last_fingerprint = String::new();
        let mut repeat_count: u32 = 0;
        // T4 guard state (all per-turn).
        let mut fingerprint_window: Vec<String> = Vec::new();
        let mut consecutive_failed_batches: u32 = 0;
        let mut consecutive_empty: u32 = 0;
        let mut nudges: u32 = 0;
        // T31 (H1): the last DISPATCHED prose call, so a byte-identical
        // repeat is answered instead of executed again.
        let mut prose_guard = recover::ProseRepeatGuard::default();
        // T35 (D2): did ANY tool run at any point this turn, structured or
        // recovered from prose? A turn that promised work is only suspect
        // when it never actually did any.
        let mut any_tool_dispatched = false;
        // T36: per-turn futile-call state. The map is fingerprint ->
        // hash of the result the model last saw for it; the counter is
        // how many dispatches this turn returned a byte-identical result
        // to one already in context. The counter NEVER resets within the
        // turn (see FUTILE_NOTICE_THRESHOLD).
        let mut futile_results: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        let mut futile_count: u32 = 0;
        let mut futile_notice_sent = false;
        // T40 per-turn auto-compaction state: whether a crossing is waiting
        // to be acted on at the next safe point, and how many compactions
        // this turn has already spent against MAX_AUTO_COMPACTIONS_PER_TURN.
        let mut compact_pending = false;
        let mut auto_compactions: u32 = 0;

        loop {
            iterations += 1;
            if iterations > self.cfg.max_iterations {
                ui(AgentEvent::Notice(format!(
                    "stopped: reached the {}-iteration limit for a single turn",
                    self.cfg.max_iterations
                )));
                break;
            }

            // T40 SAFE POINT. Not at the advisory site: that fires right
            // after a response whose `tool_use` blocks are still unanswered,
            // and cutting there would strand them. Here, the previous
            // round-trip's results are appended and this request is not yet
            // built, so history is a clean run of completed pairs.
            if compact_pending {
                compact_pending = false;
                match self.auto_compact(turn_start) {
                    CompactOutcome::Compacted { before, after } => {
                        auto_compactions += 1;
                        ui(AgentEvent::Notice(compacted_notice_text(before, after)));
                    }
                    // Invariant (e): too few round-trips to fold. Say what
                    // would have been said and count the crossing, so a turn
                    // cannot spin here.
                    CompactOutcome::Nothing => {
                        auto_compactions += 1;
                        if let (Some(used), Some(window)) =
                            (self.last_context_used, self.cfg.context_window)
                        {
                            ui(AgentEvent::Notice(context_advisory_text(used, window)));
                        }
                    }
                    // The summary call failed. The latch stays SET, so this
                    // cannot retry inside the turn; the request goes out on
                    // the uncompacted history, which may 400.
                    CompactOutcome::Failed(reason) => {
                        auto_compactions += 1;
                        ui(AgentEvent::Notice(format!(
                            "auto-compact failed ({reason}); continuing without compacting"
                        )));
                    }
                    CompactOutcome::Cancelled => {
                        ui(AgentEvent::Notice("turn interrupted".into()));
                        break;
                    }
                }
            }

            // T40 P2: the file on disk is current before every request goes
            // out. One site covers every path that loops back here: the
            // tool-results append, all of the recovery nudges, and a
            // just-completed auto-compaction. Combined with the write after
            // each assistant append below, a SIGKILL costs at most the one
            // request that was in flight.
            self.persist_now(ui);

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
            self.accrue_usage(&msg.usage);

            // Advisory context estimate (T3): the most recent response's
            // input+output IS the occupancy after this round-trip. One
            // round-trip stale by nature (no local tokenizer), and left
            // stale when usage isn't reported at all.
            if msg.usage.input_tokens.is_some() || msg.usage.output_tokens.is_some() {
                self.last_context_used = Some(
                    msg.usage.input_tokens.unwrap_or(0) + msg.usage.output_tokens.unwrap_or(0),
                );
            }
            // T20 advisory / T40 auto-compaction: ONE crossing, one of two
            // responses. The latch lives in the trigger, so exactly one of
            // these arms can ever speak about a given crossing.
            if let Some((used, window)) = self.context_trigger() {
                if self.cfg.auto_compact && auto_compactions < MAX_AUTO_COMPACTIONS_PER_TURN {
                    compact_pending = true;
                    ui(AgentEvent::Notice(auto_compact_notice_text(used, window)));
                } else {
                    ui(AgentEvent::Notice(context_advisory_text(used, window)));
                }
            }
            // T26: the spend advisory sits beside the context one, and for
            // the same reason: an agentic turn is many round-trips, and a
            // per-USER-turn check would report the $26 only once it was
            // already spent.
            if let Some(advisory) = self.cost_advisory() {
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
                    // T13: close every tool cell the stream opened before the
                    // refusal landed, preserving the FIFO ToolStart/ToolEnd
                    // pairing (docs/TUI.md) exactly as the interrupt path
                    // does. Unlike an interrupt, nothing is synthesized into
                    // history: the refused output is discarded below, so
                    // these calls never ran and never will. A ToolStart only
                    // fires once a tool_use block has a name, so an unnamed
                    // quirk-server block never opened a cell and gets no
                    // ToolEnd (same rule as land_interrupted).
                    for b in &content {
                        if let ContentBlock::ToolUse { name, .. } = b {
                            if !name.is_empty() {
                                ui(AgentEvent::ToolEnd {
                                    name: name.clone(),
                                    title: name.clone(),
                                    is_error: true,
                                });
                            }
                        }
                    }
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
                    // T13 F10: a response can both assemble tool calls and
                    // hit max_tokens. The calls still run (they are what the
                    // model asked for), and the truncation is still reported,
                    // BEFORE the tools, so the notice reads in the order the
                    // events happened.
                    if stop_details.as_ref().is_some_and(|d| d.kind == "max_tokens") {
                        ui(AgentEvent::Notice(self.truncation_notice()));
                    }
                    self.history.push(RequestMessage {
                        role: Role::Assistant,
                        content: content.clone(),
                    });
                    // T40 P2: the assistant half of this round-trip is now
                    // real history. Persist before the tools run, which is
                    // where a long turn spends its time and where a SIGKILL
                    // used to cost the whole turn.
                    self.persist_now(ui);
                    let calls: Vec<(String, String, serde_json::Value, Option<String>)> = content
                        .iter()
                        .filter_map(|b| match b {
                            // provider_state is round-trip state for the wire
                            // it came from; execution never looks at it.
                            ContentBlock::ToolUse {
                                id,
                                name,
                                input,
                                input_raw,
                                ..
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
                    any_tool_dispatched = true;

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
                        // T36: the per-call fingerprint, in the same
                        // `{name}:{input}` format the batch-joined doom-loop
                        // fingerprint above uses. Taken before `input` moves
                        // into execution.
                        let futile_fingerprint = format!("{name}:{input}");
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
                        // T36: hash the result CONTENT the model sees:
                        // the output for successes, the error text for
                        // failures, so nineteen byte-identical range errors
                        // count exactly like nineteen identical successes.
                        let result_hash = hash_result(&output);
                        match futile_results.get(&futile_fingerprint) {
                            Some(prev) if *prev == result_hash => futile_count += 1,
                            _ => {
                                futile_results.insert(futile_fingerprint, result_hash);
                            }
                        }
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
                        self.persist_now(ui);
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
                    // T36: the futile-call notice rides the SAME user
                    // message as the results it is about, appended AFTER
                    // `all_errored` is decided so the failure cap above
                    // still sees a batch of pure tool results. One notice
                    // per turn; the stop below is what a model that
                    // ignores it runs into.
                    let futile_stop = futile_count >= FUTILE_STOP_THRESHOLD;
                    if futile_count >= FUTILE_NOTICE_THRESHOLD && !futile_notice_sent && !futile_stop
                    {
                        futile_notice_sent = true;
                        results.push(ContentBlock::Text {
                            text: format!(
                                "{futile_count} of the tool calls this turn re-ran a call \
                                 already made in this same turn and returned byte-identical \
                                 results. Nothing has changed, so those calls learned \
                                 nothing. Their results are already above: use them, take a \
                                 different action, or give the final answer."
                            ),
                        });
                        ui(AgentEvent::Notice(format!(
                            "{futile_count} tool calls this turn repeated earlier calls with unchanged results; asked the model to use what it already has"
                        )));
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
                    if futile_stop {
                        ui(AgentEvent::Notice(format!(
                            "stopped: {futile_count} tool calls this turn repeated earlier calls with unchanged results"
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
                    self.persist_now(ui);
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
                    let mut promise = false;
                    let mut unknown_tool: Option<String> = None;
                    let mut tool_names: Vec<String> = Vec::new();
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
                        tool_names = self
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
                        // T31 (H3): a fenced call to a tool that does not
                        // exist used to match neither predicate, so the
                        // turn ended in silence. Last resort only: the
                        // registered paths above keep priority.
                        if prose_call.is_none() && !nudge {
                            unknown_tool =
                                recover::detect_unknown_tool_call(&text, &tool_names);
                        }
                        // T35 (D2): last of the recovery predicates, and
                        // the only one that looks at the whole turn rather
                        // than this message. A reply that ENDS by promising
                        // work, having dispatched no tool anywhere in the
                        // turn, has stopped without starting: nothing runs
                        // between turns, so the promise never resolves.
                        if prose_call.is_none()
                            && !nudge
                            && unknown_tool.is_none()
                            && !any_tool_dispatched
                        {
                            promise = recover::detect_promise_without_call(&text);
                        }
                    }
                    self.history.push(RequestMessage {
                        role: Role::Assistant,
                        content,
                    });
                    self.persist_now(ui);
                    if let Some(call) = prose_call {
                        // T31 (H1): a byte-identical repeat of the call
                        // just dispatched is NOT run again. Re-running it
                        // produced nothing new sixty times over in eval
                        // task 8, only history growth to context overflow.
                        // The notice counts against the nudge cap, so a
                        // model that will not move on ends the turn rather
                        // than trading notices forever.
                        if prose_guard.is_repeat(&call) {
                            nudges += 1;
                            let name = call.name.clone();
                            self.history.push(RequestMessage {
                                role: Role::User,
                                content: vec![ContentBlock::Text {
                                    text: format!(
                                        "You already made that exact {name} tool call and its \
                                         result is above. Nothing was executed this time. \
                                         Repeating the same call changes nothing, so take the \
                                         next step or answer the question."
                                    ),
                                }],
                            });
                            ui(AgentEvent::Notice(format!(
                                "prose-call recovery: the {name} call repeated verbatim; not executed again"
                            )));
                            continue;
                        }
                        prose_guard.record(&call);
                        // T35 (D2): a prose call that reaches execution is
                        // a dispatch like any other, so a later promise in
                        // the same turn is not "stopped without starting".
                        any_tool_dispatched = true;
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
                    if let Some(bogus) = unknown_tool {
                        // T31 (H3): name the mistake and list the registry,
                        // never a hardcoded set, so the correction stays
                        // true as tools come and go.
                        nudges += 1;
                        self.history.push(RequestMessage {
                            role: Role::User,
                            content: vec![ContentBlock::Text {
                                text: format!(
                                    "There is no tool named \"{bogus}\", so nothing was \
                                     executed. The available tools are: {}. Use one of those \
                                     through the structured tool-calling interface.",
                                    tool_names.join(", ")
                                ),
                            }],
                        });
                        ui(AgentEvent::Notice(format!(
                            "the model called a tool that does not exist (\"{bogus}\"); listed the available tools"
                        )));
                        continue;
                    }
                    if promise {
                        // Self-healing wording: say what is actually true
                        // about the runtime, then give both acceptable ways
                        // out. Counts against NUDGE_LIMIT like every other
                        // corrective, so a model that keeps promising ends
                        // the turn instead of promising forever.
                        nudges += 1;
                        self.history.push(RequestMessage {
                            role: Role::User,
                            content: vec![ContentBlock::Text {
                                text: "Nothing runs between turns. Your reply said further \
                                       work was coming but made no tool call, so nothing is \
                                       happening and nothing will happen until you act. Call \
                                       the tool now, or give the final answer if the work is \
                                       already done."
                                    .into(),
                            }],
                        });
                        ui(AgentEvent::Notice(
                            "the model promised work without calling a tool; asked it to act or answer"
                                .into(),
                        ));
                        continue;
                    }
                    match other {
                        Some(StopReason::MaxTokens) => {
                            ui(AgentEvent::Notice(self.truncation_notice()));
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
            self.accrue_usage(&msg.usage);
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
        // T26: a partial response that arrived before the Esc still cost
        // money, and this path leaves the turn loop without reaching its
        // advisory check. Emitted after the interruption notice so the
        // reason the turn ended is read first.
        if let Some(advisory) = self.cost_advisory() {
            ui(AgentEvent::Notice(advisory));
        }
    }
}

/// T36: a 64-bit digest of the result text a tool call fed back, used
/// only to compare a call's result against its own previous result inside
/// one turn. `u64` explicitly rather than `usize`: pointers are 32 bits on
/// the shipped target, and a 32-bit digest collides far too readily for a
/// guard that stops turns. Never persisted, never compared across
/// processes, so the hasher's unspecified stability is irrelevant.
fn hash_result(output: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    output.hash(&mut h);
    h.finish()
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

/// The T20 context advisory, verbatim. One definition, two callers: the
/// advisory itself and T40's bound arm, which must be byte-identical to it.
fn context_advisory_text(used: u64, window: u64) -> String {
    format!(
        "context: ~{used} of {window} tokens used; /compact frees the window by summarizing the conversation, or start a new session"
    )
}

/// T40: the notice emitted where the advisory would have been, when
/// auto-compaction takes the crossing instead. Same two facts, different
/// second clause: nothing is being asked of the reader.
fn auto_compact_notice_text(used: u64, window: u64) -> String {
    format!("context: ~{used} of {window} tokens used; compacting automatically")
}

/// The `/compact` success notice (T20), shared with T40 auto-compaction so
/// one compaction reads the same however it was triggered.
pub fn compacted_notice_text(before: usize, after: usize) -> String {
    format!(
        "compacted: {before} message(s) summarized into {after}; the next request rebuilds the provider's cached prefix (one-time cost)"
    )
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

/// T40 auto-compaction boundary, DISTINCT from [`compact_tail_start`] and
/// deliberately so. `/compact`'s rule walks back to the last plain user
/// message, which mid-turn IS the current task prompt: the whole turn would
/// be "tail" and the compaction would free nothing: exactly the case T39
/// F4 died in. This rule instead cuts inside the turn.
///
/// Returns the index where the retained tail of the last `k` round-trips
/// begins, or `None` when there is nothing to fold (invariant (e): a turn
/// with fewer than `k + 1` completed round-trips is left alone).
///
/// A completed round-trip inside a turn is exactly TWO messages (the
/// assistant response and the user message answering its `tool_use` blocks),
/// so at the safe point `history[turn_start + 1..]` is a run of such
/// pairs and cutting `2 * k` back from the end always lands on an assistant
/// message: invariants (b) and (c) hold by arithmetic, not by inspection.
/// An ODD count means the run is not pairs after all (a `PauseTurn` append,
/// say), and the fail-closed answer is to not compact at all.
fn auto_compact_tail_start(
    history: &[RequestMessage],
    turn_start: usize,
    k: usize,
) -> Option<usize> {
    let after = history.len().checked_sub(turn_start + 1)?;
    if after % 2 != 0 {
        return None;
    }
    if after / 2 < k + 1 {
        return None;
    }
    Some(history.len() - 2 * k)
}

/// T40 merge: the history after a successful auto-compaction.
///
/// `[ task prompt verbatim ] + [ assistant summary, user resume ] + [ tail ]`
///
/// Alternation-safe by construction on both wires: the prompt is a user
/// message, the summary an assistant one, the resume a user one, and the
/// tail always opens with an assistant message (see
/// [`auto_compact_tail_start`]).
///
/// The prompt is cloned VERBATIM (invariant (a)) and never merged into or
/// prefixed: in a one-shot run it is the only statement of the task, and a
/// model handed a paraphrase of its assignment does the wrong job.
fn auto_compacted_history(
    summary: &str,
    history: &[RequestMessage],
    turn_start: usize,
    tail_start: usize,
) -> Vec<RequestMessage> {
    let mut out = Vec::with_capacity(3 + history.len() - tail_start);
    out.push(history[turn_start].clone());
    out.push(RequestMessage {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: format!("{COMPACT_SUMMARY_HEADER}\n{summary}"),
        }],
    });
    out.push(RequestMessage {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: AUTO_COMPACT_RESUME.to_string(),
        }],
    });
    out.extend(history[tail_start..].iter().cloned());
    out
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
                provider_state: None,
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

    // ---- T40 auto-compaction history rule (invariants a-e) ----

    /// [prompt][A1][U1][A2][U2][A3][U3]: three completed round-trips, the
    /// minimum that folds at K=2.
    fn turn_history(round_trips: usize) -> Vec<RequestMessage> {
        let mut h = vec![user_text("the task")];
        for i in 1..=round_trips {
            h.push(assistant_tool_use(&format!("tu_{i}")));
            h.push(user_tool_result(&format!("tu_{i}")));
        }
        h
    }

    #[test]
    fn auto_compact_keeps_the_turn_prompt_byte_identical() {
        // Invariant (a): in a one-shot run the prompt is the only statement
        // of the task, so it is cloned, never merged into or prefixed.
        let h = turn_history(3);
        let tail = auto_compact_tail_start(&h, 0, 2).unwrap();
        let merged = auto_compacted_history("S", &h, 0, tail);
        assert_eq!(merged[0], h[0]);
        match &merged[0].content[..] {
            [ContentBlock::Text { text }] => assert_eq!(text, "the task"),
            other => panic!("prompt must survive verbatim and alone: {other:?}"),
        }
    }

    #[test]
    fn auto_compact_tail_pairs_every_tool_use_with_its_result() {
        // Invariant (b): no tool_use in the retained tail is unanswered, and
        // no tool_result answers a tool_use that was folded away.
        let h = turn_history(4);
        let tail = auto_compact_tail_start(&h, 0, 2).unwrap();
        let merged = auto_compacted_history("S", &h, 0, tail);
        let uses: Vec<&str> = merged
            .iter()
            .flat_map(|m| m.content.iter())
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        let results: Vec<&str> = merged
            .iter()
            .flat_map(|m| m.content.iter())
            .filter_map(|b| match b {
                ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(uses, results, "every tool_use answered, no orphan results");
        assert_eq!(uses, vec!["tu_3", "tu_4"], "the last two round-trips");
    }

    #[test]
    fn auto_compact_cuts_at_a_pair_boundary() {
        // Invariant (c): the retained tail always OPENS with the assistant
        // message of a round-trip, never mid-pair. That is also what keeps
        // the merge alternation-safe after the user-role resume message.
        for rt in 3..8 {
            let h = turn_history(rt);
            let tail = auto_compact_tail_start(&h, 0, 2).unwrap();
            assert_eq!(h[tail].role, Role::Assistant, "round_trips={rt}");
            let merged = auto_compacted_history("S", &h, 0, tail);
            for pair in merged.windows(2) {
                assert_ne!(pair[0].role, pair[1].role, "roles alternate (round_trips={rt})");
            }
            assert_eq!(merged[0].role, Role::User, "history opens on a user message");
        }
    }

    #[test]
    fn auto_compact_retains_exactly_k_round_trips() {
        // Invariant (d). Beyond the K+1 minimum the retained count does not
        // grow with the turn: a longer turn folds more, never keeps more.
        for rt in 3..8 {
            let h = turn_history(rt);
            let tail = auto_compact_tail_start(&h, 0, 2).unwrap();
            assert_eq!(h.len() - tail, 4, "2 round-trips = 4 messages (round_trips={rt})");
            let merged = auto_compacted_history("S", &h, 0, tail);
            // prompt + summary pair + 2 round-trips, whatever the turn length.
            assert_eq!(merged.len(), 7, "round_trips={rt}");
        }
    }

    #[test]
    fn auto_compact_leaves_a_turn_with_too_few_round_trips_alone() {
        // Invariant (e): fewer than K+1 completed round-trips is nothing to
        // fold: the tail would be the whole turn.
        for rt in 0..3 {
            assert_eq!(auto_compact_tail_start(&turn_history(rt), 0, 2), None, "round_trips={rt}");
        }
        assert!(auto_compact_tail_start(&turn_history(3), 0, 2).is_some());
    }

    #[test]
    fn auto_compact_declines_a_history_that_is_not_whole_pairs() {
        // Fail-closed: an odd message count after the prompt means the run is
        // not round-trip pairs (a PauseTurn append, say). Cutting arithmetic
        // would be wrong there, so nothing is cut.
        let mut h = turn_history(3);
        h.push(assistant_text("paused"));
        assert_eq!(auto_compact_tail_start(&h, 0, 2), None);
    }

    #[test]
    fn auto_compact_summary_is_spoken_by_the_assistant_and_carries_the_marker() {
        let h = turn_history(3);
        let tail = auto_compact_tail_start(&h, 0, 2).unwrap();
        let merged = auto_compacted_history("SUMMARY BODY", &h, 0, tail);
        assert_eq!(merged[1].role, Role::Assistant);
        match &merged[1].content[..] {
            [ContentBlock::Text { text }] => {
                assert!(text.starts_with(COMPACT_SUMMARY_HEADER));
                assert!(text.ends_with("\nSUMMARY BODY"));
            }
            other => panic!("summary message: {other:?}"),
        }
        assert_eq!(merged[2].role, Role::User);
        match &merged[2].content[..] {
            [ContentBlock::Text { text }] => assert_eq!(text, AUTO_COMPACT_RESUME),
            other => panic!("resume message: {other:?}"),
        }
    }

    #[test]
    fn auto_compact_honours_a_turn_start_after_earlier_history() {
        // A REPL session with pre-turn history: everything before the
        // CURRENT turn's prompt is folded away too.
        let mut h = vec![user_text("older"), assistant_text("older reply")];
        let turn_start = h.len();
        h.extend(turn_history(3));
        let tail = auto_compact_tail_start(&h, turn_start, 2).unwrap();
        let merged = auto_compacted_history("S", &h, turn_start, tail);
        assert_eq!(merged.len(), 7);
        match &merged[0].content[..] {
            [ContentBlock::Text { text }] => assert_eq!(text, "the task"),
            other => panic!("the CURRENT turn's prompt leads: {other:?}"),
        }
    }

    #[test]
    fn the_two_compaction_rules_stay_distinct() {
        // The whole point of T40's rule: /compact's boundary walks back to
        // the task prompt mid-turn (freeing nothing), and the auto rule cuts
        // inside the turn instead.
        let h = turn_history(4);
        assert_eq!(compact_tail_start(&h), Some(0), "/compact keeps the whole turn");
        assert_eq!(auto_compact_tail_start(&h, 0, 2), Some(5), "auto cuts inside it");
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
