use crate::tools as temur_approval;
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

/// T65 P0: what a plain REPL does about mutating tool calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationPolicy {
    /// Install [`stdin_approver`] and ask the human at the terminal.
    Ask,
    /// Refuse like one-shot `-p`, with `mutation_refusal_text`.
    Refuse,
    /// Do nothing: mutating calls run, as they do everywhere no approver
    /// is installed.
    Permit,
}

/// T65 P0: decide, from the two terminal tests and the resolved approval
/// mode, which of the three a plain REPL gets.
///
/// Pure, and in the lib rather than in `main.rs`, so the whole table is a
/// unit test. `approve_ask` is already the resolved answer (the flag and
/// `"approve_mutations": "allow"` are folded into it in `main.rs`), so
/// false here means the operator said allow and nothing may tighten past
/// it, whatever the terminals look like.
///
/// The shape this phase changes is the third: before T65 a piped plain
/// REPL could not ask and did not refuse, so mutating tools ran as if
/// `--allow-mutations` had been given. A run that cannot ask now denies,
/// which is the answer one-shot `-p` has given since T46.
pub fn plain_repl_mutation_policy(
    stdin_is_terminal: bool,
    stdout_is_terminal: bool,
    approve_ask: bool,
) -> MutationPolicy {
    if !approve_ask {
        MutationPolicy::Permit
    } else if stdin_is_terminal && stdout_is_terminal {
        MutationPolicy::Ask
    } else {
        MutationPolicy::Refuse
    }
}

/// The plain REPL's approver (T21, generalized by T46): prompt on the
/// terminal between tool events, showing what is being approved and why,
/// and read one answer line. Default is DENY: empty input, an unrecognized
/// answer, EOF, and read errors all deny. Installed by main ONLY when stdin
/// and stdout are real terminals, so piped runs (the mock e2e suites) never
/// see it and refuse mutating calls instead (T65 P0).
///
/// TWO FORMS, and the difference is the answer set:
/// - composed (`no_key_sandbox`): the T21 lines BYTE-IDENTICAL, with T46's
///   facts added around them, and y/N only. No session allow is offered,
///   because sandbox-needing AND mutating is the highest-risk combination
///   this product has.
/// - mutation only: the tool, its summary, and y / a / n, where `a` allows
///   that ONE tool for the rest of the session.
pub fn stdin_approver(
) -> Box<dyn FnMut(&temur_approval::ApprovalRequest) -> temur_approval::ApprovalAnswer> {
    use temur_approval::ApprovalAnswer;
    Box::new(|req: &temur_approval::ApprovalRequest| {
        if req.no_key_sandbox {
            println!("  [?] bash approval needed: the key sandbox is unavailable on this host,");
            println!("      so this command would run with NO key isolation:");
            for line in req.summary.lines() {
                println!("        {line}");
            }
            println!("      it also changes your system, so it needs approval either way.");
            if let Some(d) = req.danger {
                println!("      !! {d}");
            }
            print!("      run it? [y/N] ");
        } else {
            println!("  [?] {} approval needed:", req.tool);
            for line in req.summary.lines() {
                println!("        {line}");
            }
            if let Some(d) = req.danger {
                println!("      !! {d}");
            }
            print!(
                "      allow? [y/a/N] (y once, a every {} this session) ",
                req.tool
            );
        }
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        match std::io::stdin().lock().read_line(&mut answer) {
            Ok(0) | Err(_) => ApprovalAnswer::Deny,
            Ok(_) => {
                let a = answer.trim();
                if a.eq_ignore_ascii_case("y") || a.eq_ignore_ascii_case("yes") {
                    ApprovalAnswer::AllowOnce
                } else if !req.no_key_sandbox
                    && (a.eq_ignore_ascii_case("a") || a.eq_ignore_ascii_case("all"))
                {
                    ApprovalAnswer::AllowSession
                } else {
                    ApprovalAnswer::Deny
                }
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

#[cfg(test)]
mod tests {
    use super::{plain_repl_mutation_policy as policy, MutationPolicy};

    /// T65 P0: all eight combinations of (stdin tty, stdout tty,
    /// approve_ask), as a table. The four `approve_ask == false` rows are
    /// the "nothing can tighten past the operator's allow" half; of the
    /// four that ask, only the both-terminals row can ask, and the other
    /// three are the shape this phase changed from Permit to Refuse.
    #[test]
    fn the_plain_repl_policy_table() {
        let table = [
            (false, false, false, MutationPolicy::Permit),
            (false, true, false, MutationPolicy::Permit),
            (true, false, false, MutationPolicy::Permit),
            (true, true, false, MutationPolicy::Permit),
            (false, false, true, MutationPolicy::Refuse),
            (false, true, true, MutationPolicy::Refuse),
            (true, false, true, MutationPolicy::Refuse),
            (true, true, true, MutationPolicy::Ask),
        ];
        for (stdin_tty, stdout_tty, approve_ask, want) in table {
            assert_eq!(
                policy(stdin_tty, stdout_tty, approve_ask),
                want,
                "stdin_tty={stdin_tty} stdout_tty={stdout_tty} approve_ask={approve_ask}"
            );
        }
    }
}
