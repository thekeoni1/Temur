//! The default system prompts, one per [`crate::tools::PromptProfile`].
//!
//! These lived in `main.rs` through v0.29.1, where nothing outside the
//! binary could see them. T41 moved them here UNCHANGED, byte for byte,
//! because `temur doctor` has to weigh the real prompt to report a prompt
//! floor, and a doctor that measured its own approximation of the prompt
//! would be measuring nothing. `main`'s `rebuild_system` and the `/model`
//! prompt-profile swap read them through [`system_prompt_template`]; the
//! config `system_prompt` override still wins over either, in either
//! profile, and that rule stays in `main` where it always was.
//!
//! `{cwd}` is a placeholder the caller substitutes: the templates here are
//! the raw text, so a caller that has no working directory (doctor, when
//! it estimates) can still weigh them.

/// Compact default system prompt for v1; overridable via config.
/// (`{cwd}` is substituted at startup.)
pub const DEFAULT_SYSTEM: &str = "You are temur, a terminal coding agent. You help with software \
engineering tasks: reading and editing code, running commands, and searching the codebase.\n\
Use the provided tools (read, write, edit, bash, glob, grep, todowrite, todoread, skill) to act; \
prefer tools over guessing. Keep responses concise and direct — this is a terminal. \
When you edit files, verify your changes. \
You can see the local filesystem through these tools, so list or read a path before saying you \
cannot access it. \
The user's files are usually already in the working directory; find them with glob or ls \
before asking for them, since there is no upload. \
The current working directory is: {cwd}";

/// Shorter default system prompt used when `prompt_profile` resolves to
/// `"compact"` AND no config `system_prompt` override exists; an explicit
/// override always wins, in either profile.
pub const DEFAULT_SYSTEM_COMPACT: &str = "You are temur, a coding agent in a terminal. Act through \
the provided tools; always call them with valid JSON arguments — never write a tool call as \
plain text. Prefer tools over guessing, keep answers short, verify edits. \
You can see the local filesystem through these tools, so list or read a path before saying you \
cannot access it. \
The user's files are usually already in the working directory; find them with glob or ls \
before asking for them, since there is no upload. \
Working directory: {cwd}";

/// The ONE assembly rule for a session's system prompt (T55).
///
/// Order is the contract: the base (a config `system_prompt` override, or
/// the profile template with `{cwd}` substituted), then the skills
/// section, then the project-instructions block. `main`'s `rebuild_system`
/// and `doctor`'s floor both call this, so the prompt doctor weighs is the
/// prompt a session sends; before this existed the two assembled the same
/// ingredients separately and could drift.
pub fn assemble(base: &str, skills: Option<&str>, project_block: &str) -> String {
    let mut out = String::with_capacity(base.len() + project_block.len() + 256);
    out.push_str(base);
    if let Some(s) = skills {
        out.push_str(s);
    }
    out.push_str(project_block);
    out
}

/// The default system-prompt template for a profile, `{cwd}` unsubstituted.
/// The ONE place the profile-to-prompt mapping lives.
pub fn system_prompt_template(profile: crate::tools::PromptProfile) -> &'static str {
    match profile {
        crate::tools::PromptProfile::Full => DEFAULT_SYSTEM,
        crate::tools::PromptProfile::Compact => DEFAULT_SYSTEM_COMPACT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::PromptProfile;

    /// The byte lock on both templates. It began as T41's proof that
    /// moving them out of main.rs changed nothing, and it now pins the
    /// T53 strings; the version marker moved with them ON PURPOSE. The
    /// guard exists so a prompt edit has to change this test deliberately
    /// rather than drift past it, and D22's sentence is exactly such an
    /// edit.
    #[test]
    fn both_templates_are_byte_identical_to_the_pinned_strings() {
        let full_t53 = "You are temur, a terminal coding agent. You help with software \
engineering tasks: reading and editing code, running commands, and searching the codebase.\n\
Use the provided tools (read, write, edit, bash, glob, grep, todowrite, todoread, skill) to act; \
prefer tools over guessing. Keep responses concise and direct \u{2014} this is a terminal. \
When you edit files, verify your changes. \
You can see the local filesystem through these tools, so list or read a path before saying you \
cannot access it. \
The user's files are usually already in the working directory; find them with glob or ls \
before asking for them, since there is no upload. \
The current working directory is: {cwd}";
        let compact_t53 = "You are temur, a coding agent in a terminal. Act through \
the provided tools; always call them with valid JSON arguments \u{2014} never write a tool call as \
plain text. Prefer tools over guessing, keep answers short, verify edits. \
You can see the local filesystem through these tools, so list or read a path before saying you \
cannot access it. \
The user's files are usually already in the working directory; find them with glob or ls \
before asking for them, since there is no upload. \
Working directory: {cwd}";
        assert_eq!(system_prompt_template(PromptProfile::Full), full_t53);
        assert_eq!(
            system_prompt_template(PromptProfile::Compact),
            compact_t53
        );
        // The substitution the callers do, on the template they get back.
        assert!(system_prompt_template(PromptProfile::Full).contains("{cwd}"));
        assert!(system_prompt_template(PromptProfile::Compact).contains("{cwd}"));
    }

    /// The order in [`assemble`] IS the contract: base, then skills, then
    /// project instructions. A model that reads the project's rules before
    /// it knows what tools exist is reading them out of context.
    #[test]
    fn assemble_orders_base_then_skills_then_project() {
        let out = assemble("BASE", Some("SKILLS"), "PROJECT");
        assert_eq!(out, "BASESKILLSPROJECT");
        assert_eq!(assemble("BASE", None, "PROJECT"), "BASEPROJECT");
        assert_eq!(assemble("BASE", None, ""), "BASE");
        assert_eq!(assemble("BASE", Some("SKILLS"), ""), "BASESKILLS");
    }

    /// The reason the compact profile is worth having at all, in bytes.
    #[test]
    fn the_compact_template_is_the_shorter_one() {
        assert!(
            system_prompt_template(PromptProfile::Compact).len()
                < system_prompt_template(PromptProfile::Full).len()
        );
    }
}
