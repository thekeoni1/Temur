//! Edit semantics: EXACT match first — a unique exact match (or
//! `replaceAll`) behaves byte-identically to v1. Only when an exact search
//! finds NOTHING (and `replaceAll` is off) are the fuzzy fallbacks in
//! [`matchers`] consulted: line-trimmed, then block-anchor, each erroring
//! on ambiguity rather than guessing. Fuzzy successes are marked in the
//! output so they are never mistaken for exact edits; the tool prompt
//! still demands exactness — the fallback is a net, not an invitation.

pub mod matchers;

use super::{parse_input, resolve_path, Tool, ToolCtx, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct Params {
    #[serde(rename = "filePath")]
    file_path: String,
    #[serde(rename = "oldString")]
    old_string: String,
    #[serde(rename = "newString")]
    new_string: String,
    #[serde(rename = "replaceAll", default, deserialize_with = "super::coerce::lenient_bool")]
    replace_all: bool,
}

pub struct EditTool;

impl Tool for EditTool {
    /// T46: The registry asks before dispatch: this tool has one
    /// question and no second one to compose with.
    fn approval_site(&self) -> crate::tools::ApprovalSite {
        crate::tools::ApprovalSite::Registry
    }

    fn name(&self) -> &'static str {
        "edit"
    }
    fn description(&self) -> &'static str {
        include_str!("../prompts/edit.txt")
    }
    fn description_compact(&self) -> &'static str {
        include_str!("../prompts/compact/edit.txt")
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "filePath": {"type": "string", "description": "The absolute path to the file to modify"},
                "oldString": {"type": "string", "description": "The text to replace"},
                "newString": {"type": "string", "description": "The text to replace it with (must be different from oldString)"},
                "replaceAll": {"type": "boolean", "description": "Replace all occurrences of oldString (default false)"}
            },
            "required": ["filePath", "oldString", "newString"]
        })
    }

    fn execute(&self, input: Value, ctx: &mut ToolCtx) -> Result<ToolOutput, ToolError> {
        let p: Params = parse_input(input)?;
        if p.old_string.is_empty() {
            return Err(ToolError::InvalidInput("oldString must not be empty".into()));
        }
        if p.old_string == p.new_string {
            return Err(ToolError::InvalidInput(
                "newString must be different from oldString".into(),
            ));
        }
        let path = resolve_path(ctx, &p.file_path);
        // T18: before the read (an edit both reads and rewrites the file).
        ctx.guard.check(&path)?;
        let content = std::fs::read_to_string(&path)
            .map_err(|_| ToolError::failed(format!("File not found: {}", path.display())))?;
        // T19: an edit reads the file (just above), so it arms write's
        // read-first check like the read tool does.
        ctx.record_read(&path);
        let matches = content.matches(&p.old_string).count();
        if matches == 0 {
            return self.execute_fuzzy(&p, &path, &content);
        }
        if matches > 1 && !p.replace_all {
            return Err(ToolError::failed(format!(
                "oldString appears {matches} times in the file. Provide more surrounding context to make it unique, or set replaceAll to true."
            )));
        }
        // T63 P2a, the EDIT IDEMPOTENCY GUARD: a resent edit whose newString
        // extends oldString in place would apply a second time, because
        // oldString still matches inside the edited text. The fuzzy path is
        // not reached here and is unchanged.
        let applied = applied_sites(&content, &p.old_string, &p.new_string);
        if applied > 0 {
            let mut msg = "this edit looks already applied: newString is already present at the match site. Read the file before editing it again."
                .to_string();
            // A mixed replaceAll (some sites done, some not) is refused
            // outright, never partially applied, and says how many are done
            // so the model reads the file instead of guessing.
            if p.replace_all {
                msg.push_str(&format!(
                    " With replaceAll, {applied} of {matches} match sites already carry newString, so nothing was changed."
                ));
            }
            return Err(ToolError::failed(msg));
        }
        let (new_content, count) = if p.replace_all {
            (content.replace(&p.old_string, &p.new_string), matches)
        } else {
            (content.replacen(&p.old_string, &p.new_string, 1), 1)
        };
        std::fs::write(&path, new_content).map_err(|e| ToolError::failed(e.to_string()))?;
        Ok(ToolOutput {
            title: p.file_path,
            output: format!("Edited {} ({count} replacement(s))", path.display()),
        })
    }
}

/// T63 P2a: how many matches of `old` sit where replacing `old` with `new`
/// has already happened. For each offset k at which `old` sits inside `new`,
/// a match of `old` at byte i is already applied when the text starting at
/// i - k is exactly `new`. Any such match refuses the edit, with or without
/// `replaceAll` (Ruling on P2a: replacing all would apply that site a second
/// time). A `new` that contains `old` but is not yet in the file (a wrap
/// edit) has no such match, so it goes through. A `new` that contains `old`
/// more than once counts each of its matches.
fn applied_sites(content: &str, old: &str, new: &str) -> usize {
    let offsets: Vec<usize> = new.match_indices(old).map(|(k, _)| k).collect();
    if offsets.is_empty() {
        return 0;
    }
    content
        .match_indices(old)
        .filter(|(i, _)| {
            offsets
                .iter()
                .any(|&k| *i >= k && content.get(*i - k..).is_some_and(|rest| rest.starts_with(new)))
        })
        .count()
}

impl EditTool {
    /// The exact search found nothing — consult the fuzzy pipeline (T6).
    /// `replaceAll` never edits fuzzily (a fuzzy replace-all is incoherent);
    /// it only borrows the pipeline to word its error precisely.
    fn execute_fuzzy(
        &self,
        p: &Params,
        path: &std::path::Path,
        content: &str,
    ) -> Result<ToolOutput, ToolError> {
        let result = matchers::fuzzy_match(content, &p.old_string);
        if p.replace_all {
            return Err(match result {
                matchers::FuzzyResult::NoMatch => ToolError::failed(NOT_FOUND_MSG),
                _ => ToolError::failed(
                    "replaceAll requires an exact match. Re-read the file and copy the text exactly, or make individual edits without replaceAll.",
                ),
            });
        }
        match result {
            matchers::FuzzyResult::NoMatch => Err(ToolError::failed(NOT_FOUND_MSG)),
            matchers::FuzzyResult::Ambiguous { count } => Err(ToolError::failed(format!(
                "oldString matched {count} locations approximately (whitespace-tolerant). Provide more surrounding lines to make the match unique."
            ))),
            matchers::FuzzyResult::Unique { range, matcher } => {
                // Line-trimmed path (F3): re-apply the uniform
                // leading-whitespace delta between oldString and the matched
                // lines to newString, so the FILE's indentation style
                // survives the splice (indentation-significant languages
                // corrupt otherwise). An inconsistent delta rejects the
                // candidate — not-found beats a guessed splice. Block-anchor
                // splices verbatim: its middle differs by definition, so no
                // per-line pairing exists.
                let adjusted = match matcher {
                    matchers::Matcher::LineTrimmed => {
                        matchers::reindent_replacement(content, &range, &p.old_string, &p.new_string)
                            .ok_or_else(|| ToolError::failed(NOT_FOUND_MSG))?
                    }
                    matchers::Matcher::BlockAnchor => p.new_string.clone(),
                };
                // Splice over the ORIGINAL byte range; on a CRLF file the
                // (typically LF-shaped) replacement is converted so the
                // untouched regions and the new block agree.
                let replacement = if matchers::is_crlf(content) {
                    matchers::to_crlf(&adjusted)
                } else {
                    adjusted
                };
                let mut new_content =
                    String::with_capacity(content.len() + replacement.len());
                new_content.push_str(&content[..range.start]);
                new_content.push_str(&replacement);
                new_content.push_str(&content[range.end..]);
                std::fs::write(path, new_content)
                    .map_err(|e| ToolError::failed(e.to_string()))?;
                let note = match matcher {
                    matchers::Matcher::LineTrimmed => "whitespace-tolerant match",
                    matchers::Matcher::BlockAnchor => {
                        "block-anchor match — oldString differed from the file; re-read before further edits"
                    }
                };
                Ok(ToolOutput {
                    title: p.file_path.clone(),
                    output: format!("Edited {} (1 replacement(s), {note})", path.display()),
                })
            }
        }
    }
}

const NOT_FOUND_MSG: &str = "oldString was not found in the file, even with whitespace-tolerant matching. Re-read the file and copy the text exactly.";
