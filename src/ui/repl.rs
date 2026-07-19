use super::Ui;
use crate::agent::events::AgentEvent;
use std::io::{BufRead, Write};

pub struct ReplUi {
    stdin: std::io::Stdin,
    /// True once the current line has streamed text (to place newlines).
    mid_text: bool,
}

impl ReplUi {
    pub fn new() -> Self {
        ReplUi {
            stdin: std::io::stdin(),
            mid_text: false,
        }
    }

    fn break_line(&mut self) {
        if self.mid_text {
            println!();
            self.mid_text = false;
        }
    }
}

impl Default for ReplUi {
    fn default() -> Self {
        Self::new()
    }
}

impl Ui for ReplUi {
    fn event(&mut self, ev: &AgentEvent) {
        match ev {
            AgentEvent::TextDelta(t) => {
                print!("{t}");
                let _ = std::io::stdout().flush();
                self.mid_text = true;
            }
            AgentEvent::ThinkingDelta(_) => {
                // v1: thinking is off by default; when enabled, summaries are
                // shown as a passive indicator, not full text.
                print!(".");
                let _ = std::io::stdout().flush();
            }
            AgentEvent::ToolStart { name } => {
                self.break_line();
                println!("  → {name}");
            }
            AgentEvent::ToolEnd {
                name,
                title,
                is_error,
            } => {
                self.break_line();
                let mark = if *is_error { "✗" } else { "✓" };
                println!("  {mark} {name}: {title}");
            }
            AgentEvent::Notice(n) => {
                self.break_line();
                println!("  [!] {n}");
            }
            AgentEvent::TurnComplete {
                turn_usage,
                session_usage,
            } => {
                self.break_line();
                // Cache r/w is the canary for the moving cache breakpoint:
                // healthy turns show cache read ≈ history-sized per
                // iteration; if it collapses to ~the tools+system prefix,
                // message caching silently broke.
                println!(
                    "  (turn: {} in / {} out, cache read {} write {} — session: {} in / {} out, cache read {} write {})",
                    super::fmt_tokens(turn_usage.input_tokens),
                    super::fmt_tokens(turn_usage.output_tokens),
                    super::fmt_tokens(turn_usage.cache_read_input_tokens),
                    super::fmt_tokens(turn_usage.cache_creation_input_tokens),
                    super::fmt_tokens(session_usage.input_tokens),
                    super::fmt_tokens(session_usage.output_tokens),
                    super::fmt_tokens(session_usage.cache_read_input_tokens),
                    super::fmt_tokens(session_usage.cache_creation_input_tokens),
                );
            }
        }
    }

    fn read_input(&mut self) -> Option<String> {
        loop {
            print!("> ");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            match self.stdin.lock().read_line(&mut line) {
                Ok(0) | Err(_) => return None, // EOF
                Ok(_) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if line == "exit" || line == "quit" {
                        return None;
                    }
                    return Some(line.to_string());
                }
            }
        }
    }
}
