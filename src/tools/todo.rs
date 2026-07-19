use super::{parse_input, Tool, ToolCtx, ToolError, ToolOutput};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodoItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub content: String,
    /// "pending" | "in_progress" | "completed" (free-form tolerated).
    pub status: String,
}

#[derive(Deserialize)]
struct WriteParams {
    todos: Vec<TodoItem>,
}

pub struct TodoWriteTool;

impl Tool for TodoWriteTool {
    fn name(&self) -> &'static str {
        "todowrite"
    }
    fn description(&self) -> &'static str {
        include_str!("prompts/todowrite.txt")
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The updated todo list",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string", "description": "Unique identifier for the todo item"},
                            "content": {"type": "string", "description": "Brief description of the task"},
                            "status": {"type": "string", "enum": ["pending", "in_progress", "completed"], "description": "Current status of the task"}
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    fn execute(&self, input: Value, ctx: &mut ToolCtx) -> Result<ToolOutput, ToolError> {
        let p: WriteParams = parse_input(input)?;
        ctx.todos = p.todos;
        let open = ctx.todos.iter().filter(|t| t.status != "completed").count();
        Ok(ToolOutput {
            title: format!("{open} todos"),
            output: serde_json::to_string_pretty(&ctx.todos)
                .map_err(|e| ToolError::failed(e.to_string()))?,
        })
    }
}

pub struct TodoReadTool;

impl Tool for TodoReadTool {
    fn name(&self) -> &'static str {
        "todoread"
    }
    fn description(&self) -> &'static str {
        include_str!("prompts/todoread.txt")
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn execute(&self, _input: Value, ctx: &mut ToolCtx) -> Result<ToolOutput, ToolError> {
        let open = ctx.todos.iter().filter(|t| t.status != "completed").count();
        Ok(ToolOutput {
            title: format!("{open} todos"),
            output: serde_json::to_string_pretty(&ctx.todos)
                .map_err(|e| ToolError::failed(e.to_string()))?,
        })
    }
}
