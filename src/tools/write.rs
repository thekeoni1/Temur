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
        // T18: writes deny too — overwriting a key is destruction and a
        // poisoning vector — and the check runs before create_dir_all so
        // nothing is ever created under a secrets dir.
        ctx.guard.check(&path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ToolError::failed(e.to_string()))?;
        }
        let existed = path.exists();
        std::fs::write(&path, &p.content).map_err(|e| ToolError::failed(e.to_string()))?;
        Ok(ToolOutput {
            title: p.file_path,
            output: format!(
                "{} {} ({} bytes)",
                if existed { "Overwrote" } else { "Created" },
                path.display(),
                p.content.len()
            ),
        })
    }
}
