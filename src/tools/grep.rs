use super::{
    parse_input, resolve_path, Tool, ToolCtx, ToolError, ToolOutput, WalkBudget, WalkLimits,
    WalkStop,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

const MAX_MATCHES: usize = 100;
const MAX_LINE_CHARS: usize = 250;
/// T62 P1(a): how much of a document grep turns into text before searching
/// it. A bound is needed because grep reads EVERY file it walks, so without
/// one a directory of long reports would each be extracted whole. 20,000
/// lines is far past the 414 a default read shows and past every fixture
/// here, so in practice it bounds the pathological case only. The walk
/// deadline below bounds the aggregate cost separately.
const GREP_DOC_LINES: u64 = 20_000;
/// T53/D21: bounds on the WALK, not on the match caps above. Same values
/// and the same measurements as glob.rs, and they matter more here: grep
/// READS every file it visits, so the 248,916 files under the operator's
/// drvfs mount are 248,916 reads, not just stats. Either limit returns
/// what was found plus a closing line.
const WALK_MAX_ENTRIES: u64 = 200_000;
const WALK_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Deserialize)]
struct Params {
    pattern: String,
    path: Option<String>,
    include: Option<String>,
}

pub struct GrepTool;

impl Tool for GrepTool {
    fn name(&self) -> &'static str {
        "grep"
    }
    fn description(&self) -> &'static str {
        include_str!("prompts/grep.txt")
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "The regex pattern to search for in file contents"},
                "path": {"type": "string", "description": "The directory to search in. Defaults to the current working directory."},
                "include": {"type": "string", "description": "File pattern to include in the search (e.g. \"*.js\", \"*.{ts,tsx}\")"}
            },
            "required": ["pattern"]
        })
    }

    fn execute(&self, input: Value, ctx: &mut ToolCtx) -> Result<ToolOutput, ToolError> {
        let p: Params = parse_input(input)?;
        let re = regex::Regex::new(&p.pattern)
            .map_err(|e| ToolError::InvalidInput(format!("invalid regex: {e}")))?;
        let root = match &p.path {
            Some(path) => resolve_path(ctx, path),
            None => ctx.cwd.clone(),
        };
        let include = match &p.include {
            Some(g) => Some(
                globset::GlobBuilder::new(g)
                    .build()
                    .map_err(|e| ToolError::InvalidInput(format!("invalid include glob: {e}")))?
                    .compile_matcher(),
            ),
            None => None,
        };

        // T18: one guard snapshot per execution; protected identities are
        // stat'ed here once, then every walked file is checked against it.
        // A grep reads EVERY file it walks, so an unguarded walk would
        // exfiltrate a key wholesale.
        let guard = ctx.guard.snapshot();

        // T53/D21: interruptible and bounded, exactly as glob. Limits come
        // from ToolCtx when a test injects them, else the constants above.
        let limits = ctx.walk_limits.unwrap_or(WalkLimits {
            max_entries: WALK_MAX_ENTRIES,
            deadline: WALK_DEADLINE,
        });
        let mut budget = WalkBudget::start(limits);
        let mut stop: Option<WalkStop> = None;
        let mut interrupted = false;

        let mut matches: Vec<String> = Vec::new();
        let mut total = 0usize;
        // T62 P1: the one skip a document still has once grep searches its
        // text. Extraction can fail on a corrupt or encrypted file, and a
        // silent skip there is the same false answer the raw-byte skip used to
        // give: "No matches found" for a document nobody searched.
        let mut docs_unreadable = 0usize;
        'walk: for entry in ignore::WalkBuilder::new(&root).build().flatten() {
            // Before the read, and for every entry including directories.
            if ctx.cancel.is_set() {
                interrupted = true;
                break 'walk;
            }
            if let Err(s) = budget.tick() {
                stop = Some(s);
                break 'walk;
            }
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            if guard.denies(entry.path()) {
                continue; // key isolation: never read, never matched
            }
            if let Some(inc) = &include {
                let name_hit = entry
                    .path()
                    .file_name()
                    .map(|n| inc.is_match(n))
                    .unwrap_or(false);
                let rel = entry.path().strip_prefix(&root).unwrap_or(entry.path());
                if !name_hit && !inc.is_match(rel) {
                    continue;
                }
            }
            // T62 P1(a): a document is searched as the TEXT read would show,
            // not as raw bytes. Raw bytes are the wrong thing twice over: a
            // compressed document matches nothing, and an uncompressed one
            // matches inside a content stream and reports a line number
            // read's offset rejects. Extraction failure is not a match and
            // not an error: the file is skipped, exactly as the byte path
            // skips a binary.
            let is_doc = entry
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .map(super::office::is_document)
                .unwrap_or(false);
            let owned: String;
            if is_doc {
                let Ok((doc, _)) = super::office::extract(entry.path(), 0, GREP_DOC_LINES) else {
                    docs_unreadable += 1;
                    continue;
                };
                owned = doc;
            } else {
                let Ok(bytes) = std::fs::read(entry.path()) else {
                    continue;
                };
                if bytes[..bytes.len().min(4096)].contains(&0) {
                    continue; // binary
                }
                owned = String::from_utf8_lossy(&bytes).into_owned();
            }
            let text = owned;
            for (lineno, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    total += 1;
                    if matches.len() < MAX_MATCHES {
                        let shown: String = line.chars().take(MAX_LINE_CHARS).collect();
                        matches.push(format!(
                            "{}:{}: {shown}",
                            entry.path().display(),
                            lineno + 1
                        ));
                    } else {
                        break 'walk;
                    }
                }
            }
        }

        // T53/D21: an interrupted walk returns in bash's shape (bash.rs):
        // what was found, the same marker, and an error result.
        if interrupted {
            let mut output = matches.join("\n");
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str("(interrupted by user)");
            return Err(ToolError::Failed(output));
        }

        // "No matches found" is a finished search's answer; a search cut
        // short says so through the closing line instead.
        let mut output = if matches.is_empty() {
            if stop.is_some() {
                String::new()
            } else {
                "No matches found".to_string()
            }
        } else {
            let mut out = format!("Found {total}{} matches\n", if total >= MAX_MATCHES { "+" } else { "" });
            out.push_str(&matches.join("\n"));
            if total >= MAX_MATCHES {
                out.push_str(&format!("\n(Showing first {MAX_MATCHES} matches)"));
            }
            out
        };
        // Before any walk-stop line, so "the search was cut short" stays the
        // last word when both apply.
        if docs_unreadable > 0 {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            let (noun, verb, obj) = if docs_unreadable == 1 {
                ("file", "was", "it")
            } else {
                ("files", "were", "one")
            };
            output.push_str(&format!(
                "{docs_unreadable} document {noun} could not be read as text and {verb} skipped; read {obj} to see the error."
            ));
        }
        if let Some(s) = stop {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(&s.closing_line(&root, budget.visited()));
        }
        Ok(ToolOutput {
            title: p.pattern,
            output,
        })
    }
}
