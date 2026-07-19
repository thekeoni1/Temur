use crate::provider::Usage;

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
    TurnComplete {
        turn_usage: Usage,
        session_usage: Usage,
    },
}
