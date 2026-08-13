use super::{parse_input, resolve_path, Tool, ToolCtx, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct Params {
    #[serde(rename = "filePath")]
    file_path: String,
    content: String,
}

pub struct WriteTool;

impl Tool for WriteTool {
    fn name(&self) -> &'static str {
        "write"
    }
    fn description(&self) -> &'static str {
        include_str!("prompts/write.txt")
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "filePath": {"type": "string", "description": "The absolute path to the file to write (must be absolute, not relative)"},
                "content": {"type": "string", "description": "The content to write to the file"}
            },
            "required": ["filePath", "content"]
        })
    }

    fn execute(&self, input: Value, ctx: &mut ToolCtx) -> Result<ToolOutput, ToolError> {
        let p: Params = parse_input(input)?;
        let path = resolve_path(ctx, &p.file_path);
        // T18: writes deny too (overwriting a key is destruction and a
        // poisoning vector), and the check runs before create_dir_all so
        // nothing is ever created under a secrets dir.
        ctx.guard.check(&path)?;
        let existed = path.exists();
        // T19 read-first enforcement: the write prompt has always promised
        // this failure; for weak models the promise must be real. New files
        // are unaffected.
        if existed && !ctx.was_read(&path) {
            return Err(ToolError::failed(format!(
                "{} exists but has not been read in this session. Read it first, or use edit for targeted changes.",
                path.display()
            )));
        }
        // T30 (T29 queue finding 6, measured 2026-08-12): a weak model
        // grepped, read three files, then wrote 8 bytes over the 30-byte
        // file holding the answer and reported success. The read-first
        // guard permitted it correctly (the model HAD just read it), so
        // nothing here is a guard defect; what was missing is any trace
        // that content was destroyed. The successful result now says so
        // whenever there was content to lose, with no smallness threshold:
        // deciding which overwrites are worth mentioning is a judgment the
        // tool has no standing to make. u64 because a file length is a file
        // length, not a pointer width.
        let prior_bytes: u64 = if existed {
            std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ToolError::failed(e.to_string()))?;
        }
        std::fs::write(&path, &p.content).map_err(|e| ToolError::failed(e.to_string()))?;
        // A successful write knows the file's content: overwrites of its own
        // output (e.g. iterating on a generated file) need no re-read.
        ctx.record_read(&path);
        let replaced = if prior_bytes > 0 {
            format!(", replaced {prior_bytes} bytes of prior content")
        } else {
            String::new()
        };
        Ok(ToolOutput {
            title: p.file_path,
            output: format!(
                "{} {} ({} bytes{replaced})",
                if existed { "Overwrote" } else { "Created" },
                path.display(),
                p.content.len() as u64
            ),
        })
    }
}
