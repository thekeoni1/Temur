//! v1 edit semantics (deliberately simpler than OpenCode's fuzzy fallbacks):
//! exact unique match, or `replaceAll`.

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
        include_str!("prompts/edit.txt")
    }
    fn description_compact(&self) -> &'static str {
        include_str!("prompts/compact/edit.txt")
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
        let content = std::fs::read_to_string(&path)
            .map_err(|_| ToolError::failed(format!("File not found: {}", path.display())))?;
        let matches = content.matches(&p.old_string).count();
        if matches == 0 {
            return Err(ToolError::failed(
                "oldString was not found in the file. Make sure it matches exactly, including whitespace and indentation.",
            ));
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
