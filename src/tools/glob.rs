use super::{
    parse_input, resolve_path, Tool, ToolCtx, ToolError, ToolOutput, WalkBudget, WalkLimits,
    WalkStop,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{Duration, SystemTime};

const MAX_RESULTS: usize = 100;
/// T53/D21: bounds on the WALK, not on the output above. Measured on this
/// box with the same ignore::WalkBuilder the tools use: the temur repo is
/// 192 entries in 10 ms, /home/dev is 93,035 entries in 0.7 s warm and
/// 6.3 s cold, and /mnt/c/Users/<user> (drvfs, the mount the operator hit)
/// is 350,473 entries in 240 s, which is the roughly five minutes they sat
/// through. The deadline is 24x under that and about a thousand times the
/// repo walk, so no ordinary search can reach it; on drvfs it stops after
/// roughly 15,000 entries. The entries cap is above /home/dev's 93,035, so
/// a legitimate home-directory walk still completes, and it catches the
/// other shape: a tree that is enormous but fast, where the clock would
/// not trip. Either one returns what was found plus a closing line.
const WALK_MAX_ENTRIES: u64 = 200_000;
const WALK_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Deserialize)]
struct Params {
    pattern: String,
    path: Option<String>,
}

pub struct GlobTool;

impl Tool for GlobTool {
    fn name(&self) -> &'static str {
        "glob"
    }
    fn description(&self) -> &'static str {
        include_str!("prompts/glob.txt")
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "The glob pattern to match files against"},
                "path": {"type": "string", "description": "The directory to search in. If not specified, the current working directory will be used. IMPORTANT: Omit this field to use the default directory. DO NOT enter \"undefined\" or \"null\" - simply omit it for the default behavior. Must be a valid directory path if provided."}
            },
            "required": ["pattern"]
        })
    }

    fn execute(&self, input: Value, ctx: &mut ToolCtx) -> Result<ToolOutput, ToolError> {
        let p: Params = parse_input(input)?;
        let root = match &p.path {
            Some(path) => resolve_path(ctx, path),
            None => ctx.cwd.clone(),
        };
        let glob = globset::GlobBuilder::new(&p.pattern)
            .literal_separator(false)
            .build()
            .map_err(|e| ToolError::InvalidInput(format!("invalid glob pattern: {e}")))?
            .compile_matcher();

        // T18: one guard snapshot per execution. Protected files (and
        // anything under a secrets dir) are omitted from listings: names
        // and mtimes are a leak surface too.
        let guard = ctx.guard.snapshot();

        // T53/D21: the walk is now interruptible and bounded. The limits
        // come from ToolCtx when a test injects them, otherwise from the
        // shipped constants above.
        let limits = ctx.walk_limits.unwrap_or(WalkLimits {
            max_entries: WALK_MAX_ENTRIES,
            deadline: WALK_DEADLINE,
        });
        let mut budget = WalkBudget::start(limits);
        let mut stop: Option<WalkStop> = None;
        let mut interrupted = false;

        let mut hits: Vec<(std::path::PathBuf, SystemTime)> = Vec::new();
        for entry in ignore::WalkBuilder::new(&root).build().flatten() {
            // Checked before any work on this entry, and for every entry
            // including directories: the traversal is what runs long.
            if ctx.cancel.is_set() {
                interrupted = true;
                break;
            }
            if let Err(s) = budget.tick() {
                stop = Some(s);
                break;
            }
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            if guard.denies(entry.path()) {
                continue; // key isolation: never listed
            }
            let rel = entry.path().strip_prefix(&root).unwrap_or(entry.path());
            if glob.is_match(rel) || glob.is_match(entry.path()) {
                let mtime = entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                hits.push((entry.path().to_path_buf(), mtime));
            }
        }
        // Most recently modified first, like OpenCode. Sorting and the
        // MAX_RESULTS cut apply to whatever was collected, cut short or not.
        hits.sort_by(|a, b| b.1.cmp(&a.1));
        let total = hits.len();
        let truncated = total > MAX_RESULTS;
        hits.truncate(MAX_RESULTS);
        let listing = hits
            .iter()
            .map(|(p, _)| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");

        // T53/D21: an interrupted walk returns in the same shape bash uses
        // for an Esc mid-command (bash.rs): partial output, the same
        // marker, and an error result, so the model cannot read a partial
        // listing as the finished answer.
        if interrupted {
            let mut output = listing;
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str("(interrupted by user)");
            return Err(ToolError::Failed(output));
        }

        // "No files found" is the answer of a search that finished. One cut
        // short says so instead, and the closing line carries the reason.
        let mut output = if hits.is_empty() {
            if stop.is_some() {
                String::new()
            } else {
                "No files found".to_string()
            }
        } else {
            listing
        };
        if truncated {
            output.push_str(&format!(
                "\n(Showing first {MAX_RESULTS} of {total} results)"
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
