//! UI seam: the agent core emits `AgentEvent`s and asks for input through
//! this trait. The line REPL is one implementation; a TUI replaces it later.

pub mod oneshot;
pub mod repl;
pub mod tui;

use crate::agent::events::AgentEvent;

/// Token-count display honoring absent-vs-zero: `None` means "the provider
/// never reported this" (routine for local servers) and renders as "—" —
/// a fake 0 would claim free tokens.
pub fn fmt_tokens(t: Option<u64>) -> String {
    match t {
        Some(v) => v.to_string(),
        None => "—".into(),
    }
}

pub trait Ui {
    fn event(&mut self, ev: &AgentEvent);
    /// Next user input; `None` means EOF/quit.
    fn read_input(&mut self) -> Option<String>;
    /// Called once after the last turn, before exit. Default: nothing;
    /// only the one-shot UI (T14) uses it, to terminate its prose stream.
    fn finish(&mut self) {}
}
