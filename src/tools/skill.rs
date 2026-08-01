use super::{parse_input, Tool, ToolCtx, ToolError, ToolOutput};
use crate::skills;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Deserialize)]
struct SkillParams {
    name: String,
}

/// Loads a named skill's instructions into context. Holds the resolved skill
/// search dirs (fixed at startup), so it needs nothing from the session ctx.
pub struct SkillTool {
    dirs: Vec<PathBuf>,
}

impl SkillTool {
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        SkillTool { dirs }
    }
}

impl Tool for SkillTool {
    fn name(&self) -> &'static str {
        "skill"
    }
    fn description(&self) -> &'static str {
        include_str!("prompts/skill.txt")
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The name of the skill to load (its directory name), exactly as listed in <available_skills>."
                }
            },
            "required": ["name"]
        })
    }

    fn execute(&self, input: Value, _ctx: &mut ToolCtx) -> Result<ToolOutput, ToolError> {
        let p: SkillParams = parse_input(input)?;
        let name = p.name.trim();
        if name.is_empty() {
            return Err(ToolError::InvalidInput("skill: 'name' is empty".into()));
        }
        // A skill name is a single directory component: reject any path
        // separators or parent refs so the model can't escape the skill dirs.
        if name.contains('/') || name.contains('\\') || name.contains("..") {
            return Err(ToolError::InvalidInput(format!(
                "skill: invalid name '{name}' (must be a bare skill name, no path)"
            )));
        }
        match skills::load(&self.dirs, name) {
            Some((dir, content)) => {
                // Tell the model where the skill lives so its relative playbook
                // / asset reads resolve correctly.
                let output = format!(
                    "<skill_content name=\"{name}\">\nBase directory for this skill: {}\n\n{content}\n</skill_content>",
                    dir.display()
                );
                Ok(ToolOutput {
                    title: format!("skill: {name}"),
                    output,
                })
            }
            None => Err(ToolError::failed(format!(
                "skill '{name}' not found in any skill directory"
            ))),
        }
    }
}
