//! Tool trait + registry. Prompts under `prompts/` are ported near-verbatim
//! from sst/opencode v1.2.25 (MIT), per the project brief.

mod bash;
mod edit;
mod glob;
mod grep;
pub mod guard;
mod read;
mod skill;
mod todo;
mod write;

use crate::cancel::CancelToken;
use crate::provider::ToolDef;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

pub use bash::{sandbox_available, SANDBOX_REFUSAL};
pub use guard::KeyGuard;
pub use skill::SkillTool;
pub use todo::TodoItem;

/// Central output cap applied by the registry (chars), mirroring OpenCode's
/// centralized truncation.
const MAX_OUTPUT_CHARS: usize = 30_000;

/// Mutable per-session state tools may use.
pub struct ToolCtx {
    pub cwd: PathBuf,
    pub todos: Vec<TodoItem>,
    /// T6 cooperative interruption. Long-running tools (bash) poll it; the
    /// default is an inert token that is never set, so tools outside a
    /// session behave exactly as before. The session wires its own token in.
    pub cancel: CancelToken,
    /// T18 key isolation: file-identity guard over the configured key
    /// files. `ToolCtx::new` yields an EMPTY guard (checks nothing), so
    /// keyless configs and tools outside a keyed session behave exactly as
    /// before. Startup installs the real one via `Session::set_key_guard`.
    pub guard: KeyGuard,
    /// T18 escape hatch (config `allow_bash_without_key_sandbox`): with
    /// keys guarded but no working sandbox, bash refuses unless this is
    /// set. Meaningless while the guard is empty.
    pub allow_unsandboxed_bash: bool,
}

impl ToolCtx {
    pub fn new(cwd: PathBuf) -> Self {
        ToolCtx {
            cwd,
            todos: vec![],
            cancel: CancelToken::new(),
            guard: KeyGuard::empty(),
            allow_unsandboxed_bash: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    /// Short human-readable label for the UI (e.g. relative path).
    pub title: String,
    /// Full text fed back to the model as the tool_result.
    pub output: String,
}

/// All tool failures become `tool_result` blocks with `is_error: true` —
/// they are model-facing feedback, never crashes.
#[derive(thiserror::Error, Debug)]
pub enum ToolError {
    #[error("The tool was called with invalid arguments: {0}. Please rewrite the input so it satisfies the expected schema.")]
    InvalidInput(String),
    #[error("{0}")]
    Failed(String),
}

impl ToolError {
    pub fn failed(msg: impl Into<String>) -> Self {
        ToolError::Failed(msg.into())
    }
}

/// Which description set [`Registry::definitions`] serves (T4). `Full` is
/// the OpenCode-ported prompts (Claude-sized); `Compact` is hand-trimmed
/// for small-context local models. Selected explicitly via config only —
/// never inferred from context_window or anything else.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PromptProfile {
    #[default]
    Full,
    Compact,
}

pub trait Tool {
    fn name(&self) -> &'static str;
    /// The model-facing prompt (ported .txt), used as the tool description.
    fn description(&self) -> &'static str;
    /// Trimmed description for the compact profile. Defaults to the full
    /// description, so tools without a hand-written compact prompt need no
    /// changes.
    fn description_compact(&self) -> &'static str {
        self.description()
    }
    fn input_schema(&self) -> Value;
    fn execute(&self, input: Value, ctx: &mut ToolCtx) -> Result<ToolOutput, ToolError>;
}

/// Parse a tool's input `Value` into its typed params, mapping serde errors
/// to model-facing `InvalidInput`.
fn parse_input<T: serde::de::DeserializeOwned>(input: Value) -> Result<T, ToolError> {
    serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))
}

pub struct Registry {
    tools: Vec<Box<dyn Tool>>,
    profile: PromptProfile,
}

impl Registry {
    pub fn standard() -> Self {
        Registry {
            tools: vec![
                Box::new(read::ReadTool),
                Box::new(write::WriteTool),
                Box::new(edit::EditTool),
                Box::new(bash::BashTool),
                Box::new(glob::GlobTool),
                Box::new(grep::GrepTool),
                Box::new(todo::TodoWriteTool),
                Box::new(todo::TodoReadTool),
            ],
            profile: PromptProfile::Full,
        }
    }

    pub fn with_tools(tools: Vec<Box<dyn Tool>>) -> Self {
        Registry {
            tools,
            profile: PromptProfile::Full,
        }
    }

    /// Builder: select which description set `definitions()` serves. Tool
    /// set and ORDER are untouched — only the description text varies.
    pub fn with_profile(mut self, profile: PromptProfile) -> Self {
        self.profile = profile;
        self
    }

    /// In-place profile switch (T9 `/model` across profiles with different
    /// prompt profiles). Same contract as [`Registry::with_profile`]: only
    /// the description text served by `definitions()` changes — tool set,
    /// order, and schemas are untouched.
    pub fn set_profile(&mut self, profile: PromptProfile) {
        self.profile = profile;
    }

    /// The standard set plus the `skill` tool, which loads instruction files
    /// from the given resolved skill directories. Registered last so the
    /// stable prompt-cache prefix (standard tools) is unaffected.
    pub fn standard_with_skills(skill_dirs: Vec<PathBuf>) -> Self {
        let mut r = Self::standard();
        r.tools.push(Box::new(skill::SkillTool::new(skill_dirs)));
        r
    }

    /// Tool definitions in deterministic (registration) order — stable order
    /// keeps the prompt cache prefix stable.
    pub fn definitions(&self) -> Vec<ToolDef> {
        self.tools
            .iter()
            .map(|t| ToolDef {
                name: t.name().to_string(),
                description: match self.profile {
                    PromptProfile::Full => t.description(),
                    PromptProfile::Compact => t.description_compact(),
                }
                .to_string(),
                input_schema: t.input_schema(),
            })
            .collect()
    }

    /// Execute by name with central output truncation.
    pub fn execute(
        &self,
        name: &str,
        input: Value,
        ctx: &mut ToolCtx,
    ) -> Result<ToolOutput, ToolError> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.name() == name)
            .ok_or_else(|| ToolError::InvalidInput(format!("unknown tool: {name}")))?;
        let mut out = tool.execute(input, ctx)?;
        if out.output.chars().count() > MAX_OUTPUT_CHARS {
            let total = out.output.chars().count();
            let truncated: String = out.output.chars().take(MAX_OUTPUT_CHARS).collect();
            out.output = format!(
                "{truncated}\n\n(output truncated: showing first {MAX_OUTPUT_CHARS} of {total} chars)"
            );
        }
        Ok(out)
    }
}

/// Resolve a possibly-relative path against the session cwd.
fn resolve_path(ctx: &ToolCtx, p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    if path.is_absolute() {
        path
    } else {
        ctx.cwd.join(path)
    }
}
