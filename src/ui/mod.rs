//! UI seam: the agent core emits `AgentEvent`s and asks for input through
//! this trait. The line REPL is one implementation; a TUI replaces it later.

pub mod repl;
pub mod tui;

use crate::agent::events::AgentEvent;

pub trait Ui {
    fn event(&mut self, ev: &AgentEvent);
    /// Next user input; `None` means EOF/quit.
    fn read_input(&mut self) -> Option<String>;
}
