//! Tool trait + registry. Prompts under `prompts/` are ported near-verbatim
//! from sst/opencode v1.2.25 (MIT), per the project brief.

mod bash;
mod coerce;
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

pub use bash::{sandbox_available, APPROVAL_DENIED, SANDBOX_REFUSAL};
pub use guard::KeyGuard;
pub use skill::SkillTool;
pub use todo::TodoItem;

/// Central output cap applied by the registry (chars), mirroring OpenCode's
/// centralized truncation. This is the ceiling; with a configured
/// context_window the cap scales down (T19), see [`Registry::set_context_window`].
const MAX_OUTPUT_CHARS: usize = 30_000;
/// Floor for the context-scaled cap (T19): below this a tool result cannot
/// carry enough of a build log or grep sweep to act on.
const MIN_OUTPUT_CHARS: usize = 4_000;

/// Below this a registered redaction key is never matched (T18): replacing
/// tiny strings would mangle ordinary output, and no real API key is this
/// short.
pub const MIN_REDACTABLE_KEY_CHARS: usize = 8;

/// T46: where a tool's mutation approval is asked.
///
/// `Tool` asks for itself. Only `bash` does, and it must: its T21 no-key-
/// sandbox question is decided inside `bash.rs` after `decide_sandbox`, and
/// asking the mutation question anywhere else would put TWO prompts in front
/// of one command, which is exactly what the composition rule forbids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalSite {
    /// Never asks (read, glob, grep, skill, the todo pair).
    None,
    /// The registry asks before dispatch (write, edit).
    Registry,
    /// The tool asks for itself, composing its own questions (bash).
    Tool,
}

/// T46: one approval question, carrying every fact the prompt must state.
pub struct ApprovalRequest {
    /// The tool being approved. Session allows are PER TOOL, keyed on this.
    pub tool: String,
    /// One line: the bash command, or the write/edit path with its size or
    /// change description.
    pub summary: String,
    /// T46 COMPOSITION: this bash command ALSO needs the T21 no-key-sandbox
    /// approval, so one prompt carries both facts.
    ///
    /// It also fixes the ANSWER SET. A composed prompt offers y/N only and
    /// never a session allow, which is a design decision and not a
    /// concession to the tests that happen to enforce it: sandbox-needing
    /// AND mutating is the highest-risk combination this product has, and a
    /// session-wide allow is the wrong shape of answer for it. A session
    /// allow already held for bash answers the MUTATION question only; the
    /// T21 question still gets asked.
    pub no_key_sandbox: bool,
    /// Display-only emphasis (T46 / D13). `Some` names the matched pattern
    /// class. A miss is cosmetic BY DESIGN: the base rule already asked, so
    /// this is never a gate and never a bypass.
    pub danger: Option<&'static str>,
}

/// T46: the answer to an [`ApprovalRequest`]. Default on anything that is
/// not an explicit allow is `Deny`, as T21.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalAnswer {
    Deny,
    AllowOnce,
    /// Allow this TOOL for the rest of the session. Never offered when
    /// `no_key_sandbox` is set.
    AllowSession,
}

/// T46 (D13) destructive-command emphasis: a short FIXED list, display only.
///
/// Enumeration cannot be complete, which is why the sound base is
/// all-mutations-ask and this is only a highlighted line on a prompt that
/// was already going to appear. A miss costs nothing; a false positive
/// costs a scarier prompt on a command the user was being asked about
/// anyway.
pub fn danger_class(command: &str) -> Option<&'static str> {
    let c = command.to_lowercase();
    let rm_recursive = c.contains("rm ")
        && (c.contains(" -r") || c.contains(" -fr") || c.contains(" -rf"));
    if rm_recursive {
        return Some("recursive delete");
    }
    if c.contains("mkfs") {
        return Some("filesystem format");
    }
    if c.contains("dd ") && c.contains("of=/dev/") {
        return Some("raw write to a device");
    }
    if c.contains("git reset --hard") || c.contains("git clean -f") {
        return Some("discards uncommitted work");
    }
    Some("shred").filter(|_| c.contains("shred "))
}

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
    /// T21 bash approval, GENERALIZED by T46 into one mutation approver.
    ///
    /// An interactive UI's approver, called with a structured
    /// [`ApprovalRequest`]. `None` (the default everywhere) means no UI can
    /// ask: the T21 Ask arm still refuses as it always did, and T46's
    /// mutation question is simply not asked, so every non-interactive
    /// construction site is untouched and PERMISSIVE exactly as before. The
    /// ask-by-default flip lives at session construction in `main.rs`, not
    /// here: moving it into this default would break every MockProvider
    /// test for no safety gain, since the only entry points a user reaches
    /// go through `main`.
    pub approver: Option<Box<dyn FnMut(&ApprovalRequest) -> ApprovalAnswer>>,
    /// T46: tools the user has allowed for the rest of the session, keyed by
    /// tool name. Per tool by design: allowing bash does not allow write.
    pub session_allows: std::collections::HashSet<String>,
    /// T46: refuse mutating calls outright instead of asking.
    ///
    /// One-shot `-p` has nobody to ask, and the operator's decision is that
    /// silence there means DENY, not allow. Set at session construction in
    /// `main.rs` (never here: the default stays permissive so every
    /// approver-free construction is byte-identical), and cleared by
    /// `--allow-mutations` or `"approve_mutations": "allow"`. When set, a
    /// mutating call fails loud with [`mutation_refusal_text`], the sibling
    /// of `SANDBOX_REFUSAL`.
    pub refuse_mutations: bool,
    /// T28: the per-result output cap in force for THIS dispatch, which
    /// [`Registry::execute`] sets from its own (context-scaled, T19) cap
    /// before calling the tool. A tool that can produce a smaller answer
    /// instead of being cut in half reads it and decides for itself; every
    /// other tool ignores it and is truncated centrally as before. The
    /// default here is the ceiling, so a `ToolCtx` built outside a session
    /// behaves exactly as it did before this field existed.
    pub output_cap: usize,
    /// T19 read-first enforcement: canonicalized paths whose content this
    /// session has seen (read tool, edit reads its file, a successful write
    /// knows what it wrote). `write` refuses to overwrite an EXISTING file
    /// not in this set. Starts empty on `--continue`/`--resume`
    /// DELIBERATELY: the file may have changed on disk since the saved
    /// session read it, so a resumed session must re-read before
    /// overwriting.
    read_paths: std::collections::HashSet<PathBuf>,
}

impl ToolCtx {
    pub fn new(cwd: PathBuf) -> Self {
        ToolCtx {
            cwd,
            todos: vec![],
            cancel: CancelToken::new(),
            guard: KeyGuard::empty(),
            allow_unsandboxed_bash: false,
            approver: None,
            session_allows: std::collections::HashSet::new(),
            refuse_mutations: false,
            output_cap: MAX_OUTPUT_CHARS,
            read_paths: std::collections::HashSet::new(),
        }
    }

    /// Record that this session has seen `path`'s current content.
    /// Canonicalized so `./a.txt`, `a.txt`, and a symlinked spelling agree;
    /// the fallback (path as resolved) only matters in a delete race.
    pub fn record_read(&mut self, path: &std::path::Path) {
        self.read_paths
            .insert(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()));
    }

    /// Whether [`ToolCtx::record_read`] has seen this path.
    pub fn was_read(&self, path: &std::path::Path) -> bool {
        self.read_paths
            .contains(&std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
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
/// for small-context local models.
///
/// T41 CHANGED how one is selected. Through v0.29.1 this was
/// explicit-only: config named the profile and nothing else could pick
/// it. It is now selected by `prompt_profile`, whose default spelling is
/// `"auto"`; see [`crate::config::PromptProfileSpec`] and
/// [`crate::config::auto_prompt_profile`] for the rule. An explicit
/// `"full"` / `"compact"` still wins and is never second-guessed, and the
/// only inference is the documented window threshold.
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
    /// The advice the central truncation marker gives when THIS tool's
    /// output is cut (T28). The default is grep/head-tail narrowing, which
    /// is right for a command or a file read and nonsense for a tool whose
    /// output is one indivisible document: those override it.
    fn truncation_hint(&self) -> &'static str {
        "narrow the command, e.g. grep or head/tail, to see the elided middle"
    }
    /// T46: does this tool mutate, and who asks about it? Default `None`
    /// keeps every read-only tool (read, glob, grep, skill, the todo pair)
    /// out of the approval path by construction rather than by a name list
    /// that could drift from the registry.
    fn approval_site(&self) -> ApprovalSite {
        ApprovalSite::None
    }
    fn input_schema(&self) -> Value;
    fn execute(&self, input: Value, ctx: &mut ToolCtx) -> Result<ToolOutput, ToolError>;
}

/// Parse a tool's input `Value` into its typed params, mapping serde errors
/// to model-facing `InvalidInput`.
fn parse_input<T: serde::de::DeserializeOwned>(input: Value) -> Result<T, ToolError> {
    serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))
}

/// T46: ask the registry-sited mutation question for one call.
///
/// Returns `Some(error)` when the call must NOT run. `None` means proceed,
/// which covers every path that does not ask at all: no approver installed
/// (non-interactive, and the permissive default every MockProvider test
/// constructs), or a session allow already held for this tool.
fn approve_or_deny(tool: &str, input: &Value, ctx: &mut ToolCtx) -> Option<ToolError> {
    // T46: a run that cannot ask refuses BEFORE it consults anything else,
    // so no session allow and no missing approver can turn the refusal into
    // silent permission.
    if ctx.refuse_mutations {
        return Some(ToolError::failed(mutation_refusal_text(tool)));
    }
    if ctx.approver.is_none() || ctx.session_allows.contains(tool) {
        return None;
    }
    // An interrupt already requested denies without prompting, as T21.
    if ctx.cancel.is_set() {
        return Some(ToolError::failed(approval_denied_text(tool)));
    }
    let req = ApprovalRequest {
        tool: tool.to_string(),
        summary: summarize(tool, input),
        no_key_sandbox: false,
        danger: None,
    };
    let answer = ctx
        .approver
        .as_mut()
        .map(|ask| ask(&req))
        .unwrap_or(ApprovalAnswer::Deny);
    match answer {
        ApprovalAnswer::AllowOnce => None,
        ApprovalAnswer::AllowSession => {
            ctx.session_allows.insert(tool.to_string());
            None
        }
        ApprovalAnswer::Deny => Some(ToolError::failed(approval_denied_text(tool))),
    }
}

/// T46: the denied-call tool result, one fixed sentence class for every
/// tool. NO guard exemption rides with it: an identical resend produces an
/// identical result, and T36's futile-call guard and the doom-loop guard
/// ending that turn is the DESIGNED backstop. This wording's job is to make
/// round one count.
pub fn approval_denied_text(tool: &str) -> String {
    format!(
        "the user declined this {tool} call; do not retry it unchanged - adjust the approach or finish the turn"
    )
}

/// T46: the refusal a NON-interactive run gives instead of asking, and the
/// deliberate sibling of [`SANDBOX_REFUSAL`]: same shape, same job,
/// same promise that the way out is named in the message itself.
///
/// The operator's decision for one-shot `-p` is DENY-unless-told, because a
/// new user's first `-p` is exactly the moment the protection pays. Every
/// in-repo script that drives temur non-interactively gets the flag in the
/// same milestone.
pub fn mutation_refusal_text(tool: &str) -> String {
    format!(
        "{tool} is disabled: this call would change your system, and mutating tool calls need approval. In an interactive session temur asks per call; this non-interactive session cannot ask. Read-only tools are unaffected. For non-interactive use, pass --allow-mutations, or set \"approve_mutations\": \"allow\" in config.json, to accept mutating tool calls WITHOUT approval."
    )
}

/// T46: the one summary line a prompt shows for a registry-sited tool.
/// bash builds its own (the command itself).
fn summarize(tool: &str, input: &Value) -> String {
    let path = input
        .get("filePath")
        .or_else(|| input.get("file_path"))
        .or_else(|| input.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or("(unnamed path)");
    match tool {
        "write" => {
            let n = input
                .get("content")
                .and_then(|v| v.as_str())
                .map(|c| c.len())
                .unwrap_or(0);
            format!("write {path} ({n} bytes)")
        }
        "edit" => {
            let old = input
                .get("oldString")
                .or_else(|| input.get("old_string"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let one = old.lines().next().unwrap_or("").trim();
            let shown: String = one.chars().take(48).collect();
            if shown.is_empty() {
                format!("edit {path}")
            } else {
                format!("edit {path} (replaces \"{shown}\"...)")
            }
        }
        other => format!("{other} {path}"),
    }
}

pub struct Registry {
    tools: Vec<Box<dyn Tool>>,
    profile: PromptProfile,
    /// T18 layer 3: the ACTIVE provider's credential, registered so every
    /// tool result is scrubbed of it. `None` = nothing to redact (keyless,
    /// mock). Only the active key can be registered honestly: inactive
    /// profiles' keys are never read, so they are never redactable.
    redact_key: Option<String>,
    /// T19 context-scaled per-result output cap (chars). Defaults to
    /// [`MAX_OUTPUT_CHARS`]; [`Registry::set_context_window`] scales it to
    /// the active model's window.
    cap_chars: usize,
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
            redact_key: None,
            cap_chars: MAX_OUTPUT_CHARS,
        }
    }

    pub fn with_tools(tools: Vec<Box<dyn Tool>>) -> Self {
        Registry {
            tools,
            profile: PromptProfile::Full,
            redact_key: None,
            cap_chars: MAX_OUTPUT_CHARS,
        }
    }

    /// Register (or clear, with `None`) the active provider's key for
    /// output redaction (T18). Keys shorter than
    /// [`MIN_REDACTABLE_KEY_CHARS`] are stored but never matched: replacing
    /// tiny strings would mangle ordinary output, and a 7-char credential
    /// is not a real API key.
    pub fn set_redaction_key(&mut self, key: Option<String>) {
        self.redact_key = key;
    }

    /// Scale the per-result output cap to the active model's context window
    /// (T19). Derivation: budget a quarter of the window in tokens for one
    /// tool result, at ~4 chars/token that is `context_window` chars, clamped
    /// to [`MIN_OUTPUT_CHARS`]..=[`MAX_OUTPUT_CHARS`]. `None` (no window
    /// configured) keeps the [`MAX_OUTPUT_CHARS`] ceiling, exactly the
    /// pre-T19 cap. Same lifecycle as the T18 redaction key: set at startup
    /// and on every successful provider switch.
    pub fn set_context_window(&mut self, window: Option<u64>) {
        self.cap_chars = match window {
            Some(w) => w.clamp(MIN_OUTPUT_CHARS as u64, MAX_OUTPUT_CHARS as u64) as usize,
            None => MAX_OUTPUT_CHARS,
        };
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

    /// Execute by name with central key redaction and output truncation.
    /// Redaction runs FIRST, on success and failure alike, so a key can
    /// never leak split across the truncation cut or ride an error message.
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
        // T46: the mutation question, for the tools the REGISTRY asks about
        // (write, edit). bash asks for itself, composing this question with
        // its T21 sandbox question so one command never draws two prompts.
        if tool.approval_site() == ApprovalSite::Registry {
            if let Some(denied) = approve_or_deny(tool.name(), &input, ctx) {
                return Err(denied);
            }
        }
        // T28: tell the tool what it has to fit in, before it runs.
        ctx.output_cap = self.cap_chars;
        let mut out = tool.execute(input, ctx).map_err(|e| match e {
            ToolError::InvalidInput(s) => ToolError::InvalidInput(self.redact(s)),
            ToolError::Failed(s) => ToolError::Failed(self.redact(s)),
        })?;
        out.output = self.redact(out.output);
        out.title = self.redact(out.title);
        // T19 head+tail keep: build output puts errors at the END, so a
        // head-only cut discards exactly the informative part. Keep the true
        // head and true tail, elide the middle, and say how to narrow.
        let total = out.output.chars().count();
        if total > self.cap_chars {
            let head_n = self.cap_chars / 2;
            let tail_n = self.cap_chars - head_n;
            let head: String = out.output.chars().take(head_n).collect();
            let tail: String = out.output.chars().skip(total - tail_n).collect();
            let hint = tool.truncation_hint();
            out.output = format!(
                "{head}\n\n(output truncated: showing the first {head_n} and last {tail_n} of {total} chars; {hint})\n\n{tail}"
            );
        }
        Ok(out)
    }

    /// Scrub every occurrence of the registered key from one string (T18).
    /// No-op without a registered key of redactable length.
    fn redact(&self, s: String) -> String {
        match &self.redact_key {
            Some(key)
                if key.chars().count() >= MIN_REDACTABLE_KEY_CHARS
                    && s.contains(key.as_str()) =>
            {
                s.replace(key.as_str(), "[redacted]")
            }
            _ => s,
        }
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
