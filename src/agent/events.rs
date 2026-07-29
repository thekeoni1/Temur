use crate::provider::Usage;
use crate::session_store::ReplayItem;

/// Events the agent core emits toward the UI seam. A line REPL renders these
/// today; a TUI can replace it without touching the core.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    TextDelta(String),
    ThinkingDelta(String),
    // NOTE (seam assumption, see docs/TUI.md): ToolStart/ToolEnd carry no
    // call id, so UIs pair them FIFO. That is sound only while the turn
    // loop executes tool calls sequentially in stream order. If execution
    // ever becomes concurrent/out-of-order, add the provider's tool_use id
    // to both events and pair by id instead.
    ToolStart {
        name: String,
    },
    ToolEnd {
        name: String,
        title: String,
        is_error: bool,
    },
    /// Out-of-band condition the user should see (refusal, truncation,
    /// guard trips). Never contains secret material.
    Notice(String),
    /// T8 `/model`: the active model changed. A chrome/state signal — the
    /// human-readable confirmation travels as a separate [`Notice`](Self::Notice).
    /// Carries the provider (T16) so UIs can drop `/models`-cached ids when
    /// a switch changes the provider: a llama.cpp listing must never judge
    /// anthropic ids, and vice versa.
    ModelSwitched { model: String, provider: String },
    /// T8 `/thinking`: session thinking flipped (chrome/state signal, like
    /// [`ModelSwitched`](Self::ModelSwitched)).
    ThinkingChanged(bool),
    /// T9 `/models`: the active provider's model listing, already parsed to
    /// bare ids. Each UI renders it; the TUI also caches the ids as Tab
    /// completion candidates. Never contains key material.
    ModelsListed(Vec<String>),
    /// T8 `/clear`: the conversation was wiped. UIs reset transcript state;
    /// the confirmation Notice follows this event.
    SessionCleared,
    /// T10 `/sessions`: preformatted listing lines (active marker included)
    /// plus the keys a UI caches as `/resume` Tab-completion candidates —
    /// the same display+cache split as [`ModelsListed`](Self::ModelsListed).
    /// Never contains key material.
    SessionsListed {
        lines: Vec<String>,
        keys: Vec<String>,
    },
    /// T10 resume (startup `--continue`/`--resume` and `/resume`): the
    /// seeded history flattened for display, plus the one-line resume
    /// summary. UIs wipe their transcript state and rebuild from `items`;
    /// advisory notices follow as separate [`Notice`](Self::Notice)s.
    SessionLoaded {
        items: Vec<ReplayItem>,
        notice: String,
    },
    TurnComplete {
        turn_usage: Usage,
        session_usage: Usage,
    },
}
