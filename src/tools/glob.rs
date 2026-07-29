use super::{parse_input, resolve_path, Tool, ToolCtx, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::SystemTime;

const MAX_RESULTS: usize = 100;

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

        let mut hits: Vec<(std::path::PathBuf, SystemTime)> = Vec::new();
        for entry in ignore::WalkBuilder::new(&root).build().flatten() {
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
        // Most recently modified first, like OpenCode.
        hits.sort_by(|a, b| b.1.cmp(&a.1));
        let total = hits.len();
        let truncated = total > MAX_RESULTS;
        hits.truncate(MAX_RESULTS);

        let mut output = if hits.is_empty() {
            "No files found".to_string()
        } else {
            hits.iter()
                .map(|(p, _)| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n")
        };
        if truncated {
            output.push_str(&format!(
                "\n(Showing first {MAX_RESULTS} of {total} results)"
            ));
        }
        Ok(ToolOutput {
            title: p.pattern,
            output,
        })
    }
}
