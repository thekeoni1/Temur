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
    #[serde(rename = "replaceAll", default)]
    replace_all: bool,
}

pub struct EditTool;

impl Tool for EditTool {
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
        let matches = content.matches(&p.old_string).count();
        if matches == 0 {
            return self.execute_fuzzy(&p, &path, &content);
        }
        if matches > 1 && !p.replace_all {
            return Err(ToolError::failed(format!(
                "oldString appears {matches} times in the file. Provide more surrounding context to make it unique, or set replaceAll to true."
            )));
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
