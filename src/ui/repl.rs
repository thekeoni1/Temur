use super::Ui;
use crate::agent::events::AgentEvent;
use crate::session_store::ReplayItem;
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

/// The plain REPL's bash approver (T21): prompt on the terminal between
/// tool events, showing the exact command and why it needs approval, and
/// read one y/N line. Default is DENY: empty input, anything but y/yes
/// (case-insensitive), EOF, and read errors all deny. Installed by main
/// ONLY when stdin and stdout are real terminals, so piped runs (the mock
/// e2e suites) never see it.
pub fn stdin_bash_approver() -> Box<dyn FnMut(&str) -> bool> {
    Box::new(|command: &str| {
        println!("  [?] bash approval needed: the key sandbox is unavailable on this host,");
        println!("      so this command would run with NO key isolation:");
        for line in command.lines() {
            println!("        {line}");
        }
        print!("      run it? [y/N] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        match std::io::stdin().lock().read_line(&mut answer) {
            Ok(0) | Err(_) => false,
            Ok(_) => {
                let answer = answer.trim();
                answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
            }
        }
    })
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
            // T9 `/models`: a count line, then one indented line per id.
            AgentEvent::ModelsListed(ids) => {
                self.break_line();
                println!("  {} model id(s) from the provider:", ids.len());
                for id in ids {
                    println!("    {id}");
                }
            }
            // T10 `/sessions`: a count line, then one indented line per
            // session (the active marker is already inside each line).
            AgentEvent::SessionsListed { lines, .. } => {
                self.break_line();
                println!("  {} session(s):", lines.len());
                for l in lines {
                    println!("    {l}");
                }
            }
            // T10 resume: plain backscroll — user prompts as "> "-prefixed
            // lines, assistant text verbatim, tools as one-liners — then the
            // resume summary in the exact Notice shape (so the summary line
            // stays byte-identical to its pre-T10 rendering).
            AgentEvent::SessionLoaded { items, notice } => {
                self.break_line();
                for item in items {
                    match item {
                        ReplayItem::User(t) => println!("> {t}"),
                        ReplayItem::Assistant(t) => println!("{t}"),
                        ReplayItem::Tool { name } => println!("  ⚙ {name}"),
                    }
                }
                println!("  [!] {notice}");
            }
            // Chrome/state signals (T8): the plain REPL has no chrome; the
            // human-readable confirmation arrives as a separate Notice.
            AgentEvent::ModelSwitched { .. }
            | AgentEvent::ThinkingChanged(_)
            | AgentEvent::SessionCleared => {}
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
