//! One-shot UI (T14 `-p`/`--prompt`): assistant prose goes to stdout;
//! everything else (tool chrome, notices, usage stats, resumed
//! backscroll) goes to stderr. stdout IS the answer, so `temur -p` composes
//! in shell pipelines; the chrome stays visible on a terminal without
//! polluting a capture. Line formats mirror the plain REPL exactly, they
//! just land on the other stream.

use super::Ui;
use crate::agent::events::AgentEvent;
use crate::session_store::ReplayItem;
use std::io::Write;

pub struct OneShotUi<O: Write, E: Write> {
    out: O,
    err: E,
    /// True while a streamed prose line on `out` is unterminated.
    mid_text: bool,
}

impl OneShotUi<std::io::Stdout, std::io::Stderr> {
    pub fn stdio() -> Self {
        Self::new(std::io::stdout(), std::io::stderr())
    }
}

impl<O: Write, E: Write> OneShotUi<O, E> {
    /// Generic over the sinks so tests can assert the split on plain
    /// buffers; main only ever uses [`OneShotUi::stdio`].
    pub fn new(out: O, err: E) -> Self {
        OneShotUi {
            out,
            err,
            mid_text: false,
        }
    }

    /// Terminate an unfinished prose line on stdout (before chrome appears
    /// on stderr, and once more at exit via [`Ui::finish`]) so the prose
    /// always ends in a newline and interleaved viewing stays readable.
    fn break_line(&mut self) {
        if self.mid_text {
            let _ = writeln!(self.out);
            let _ = self.out.flush();
            self.mid_text = false;
        }
    }
}

impl<O: Write, E: Write> Ui for OneShotUi<O, E> {
    fn event(&mut self, ev: &AgentEvent) {
        match ev {
            AgentEvent::TextDelta(t) => {
                let _ = write!(self.out, "{t}");
                let _ = self.out.flush();
                self.mid_text = true;
            }
            AgentEvent::ThinkingDelta(_) => {
                let _ = write!(self.err, ".");
                let _ = self.err.flush();
            }
            AgentEvent::ToolStart { name } => {
                self.break_line();
                let _ = writeln!(self.err, "  → {name}");
            }
            AgentEvent::ToolEnd {
                name,
                title,
                is_error,
            } => {
                self.break_line();
                let mark = if *is_error { "✗" } else { "✓" };
                let _ = writeln!(self.err, "  {mark} {name}: {title}");
            }
            AgentEvent::Notice(n) => {
                self.break_line();
                let _ = writeln!(self.err, "  [!] {n}");
            }
            // Unreachable in one-shot (commands never run), rendered anyway
            // for totality.
            AgentEvent::ModelsListed(ids) => {
                let _ = writeln!(self.err, "  {} model id(s) from the provider:", ids.len());
                for id in ids {
                    let _ = writeln!(self.err, "    {id}");
                }
            }
            AgentEvent::SessionsListed { lines, .. } => {
                let _ = writeln!(self.err, "  {} session(s):", lines.len());
                for l in lines {
                    let _ = writeln!(self.err, "    {l}");
                }
            }
            // --continue/--resume backscroll: context, not this turn's
            // answer, so it must NOT contaminate stdout.
            AgentEvent::SessionLoaded { items, notice } => {
                for item in items {
                    match item {
                        ReplayItem::User(t) => {
                            let _ = writeln!(self.err, "> {t}");
                        }
                        ReplayItem::Assistant(t) => {
                            let _ = writeln!(self.err, "{t}");
                        }
                        ReplayItem::Tool { name } => {
                            let _ = writeln!(self.err, "  ⚙ {name}");
                        }
                    }
                }
                let _ = writeln!(self.err, "  [!] {notice}");
            }
            AgentEvent::ModelSwitched { .. }
            | AgentEvent::ThinkingChanged(_)
            | AgentEvent::SessionCleared => {}
            AgentEvent::TurnComplete {
                turn_usage,
                session_usage,
            } => {
                self.break_line();
                let _ = writeln!(
                    self.err,
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

    /// One-shot never reads input; the single prompt arrives via argv.
    fn read_input(&mut self) -> Option<String> {
        None
    }

    fn finish(&mut self) {
        self.break_line();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Usage;

    fn strings(out: &[u8], err: &[u8]) -> (String, String) {
        (
            String::from_utf8_lossy(out).into_owned(),
            String::from_utf8_lossy(err).into_owned(),
        )
    }

    #[test]
    fn prose_to_out_chrome_to_err_and_finish_terminates() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        {
            let mut ui = OneShotUi::new(&mut out, &mut err);
            ui.event(&AgentEvent::TextDelta("first ".into()));
            ui.event(&AgentEvent::TextDelta("segment".into()));
            ui.event(&AgentEvent::ToolStart { name: "bash".into() });
            ui.event(&AgentEvent::ToolEnd {
                name: "bash".into(),
                title: "ls".into(),
                is_error: false,
            });
            ui.event(&AgentEvent::TextDelta("answer".into()));
            ui.event(&AgentEvent::TurnComplete {
                turn_usage: Usage::default(),
                session_usage: Usage::default(),
            });
            ui.finish();
        }
        let (o, e) = strings(&out, &err);
        // Prose only, each segment newline-terminated (the tool break, then
        // finish); chrome only on err.
        assert_eq!(o, "first segment\nanswer\n");
        assert!(e.contains("→ bash") && e.contains("✓ bash: ls"), "{e}");
        assert!(e.contains("(turn:"), "{e}");
        assert!(!o.contains('→') && !o.contains("(turn:"), "{o}");
    }

    #[test]
    fn backscroll_and_notices_stay_off_stdout() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        {
            let mut ui = OneShotUi::new(&mut out, &mut err);
            ui.event(&AgentEvent::SessionLoaded {
                items: vec![
                    ReplayItem::User("earlier prompt".into()),
                    ReplayItem::Assistant("earlier answer".into()),
                ],
                notice: "resumed session (2 messages)".into(),
            });
            ui.event(&AgentEvent::Notice("advisory".into()));
            ui.finish();
        }
        let (o, e) = strings(&out, &err);
        assert!(o.is_empty(), "stdout stays pure: {o}");
        assert!(
            e.contains("> earlier prompt")
                && e.contains("earlier answer")
                && e.contains("[!] resumed session")
                && e.contains("[!] advisory"),
            "{e}"
        );
    }
}
