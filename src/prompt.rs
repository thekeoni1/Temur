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
The current working directory is: {cwd}";

/// Shorter default system prompt used when `prompt_profile` resolves to
/// `"compact"` AND no config `system_prompt` override exists; an explicit
/// override always wins, in either profile.
pub const DEFAULT_SYSTEM_COMPACT: &str = "You are temur, a coding agent in a terminal. Act through \
the provided tools; always call them with valid JSON arguments — never write a tool call as \
plain text. Prefer tools over guessing, keep answers short, verify edits. \
You can see the local filesystem through these tools, so list or read a path before saying you \
cannot access it. \
Working directory: {cwd}";

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

    /// The move is a MOVE: both templates render byte-identically to what
    /// main.rs served at v0.29.1. The expected values are the pre-move
    /// strings, captured here so a later edit to the prompts has to change
    /// this test on purpose rather than drift past it.
    #[test]
    fn both_templates_are_byte_identical_to_the_pre_move_strings() {
        let full_v0_29_1 = "You are temur, a terminal coding agent. You help with software \
engineering tasks: reading and editing code, running commands, and searching the codebase.\n\
Use the provided tools (read, write, edit, bash, glob, grep, todowrite, todoread, skill) to act; \
prefer tools over guessing. Keep responses concise and direct \u{2014} this is a terminal. \
When you edit files, verify your changes. \
You can see the local filesystem through these tools, so list or read a path before saying you \
cannot access it. \
The current working directory is: {cwd}";
        let compact_v0_29_1 = "You are temur, a coding agent in a terminal. Act through \
the provided tools; always call them with valid JSON arguments \u{2014} never write a tool call as \
plain text. Prefer tools over guessing, keep answers short, verify edits. \
You can see the local filesystem through these tools, so list or read a path before saying you \
cannot access it. \
Working directory: {cwd}";
        assert_eq!(system_prompt_template(PromptProfile::Full), full_v0_29_1);
        assert_eq!(
            system_prompt_template(PromptProfile::Compact),
            compact_v0_29_1
        );
        // The substitution the callers do, on the template they get back.
        assert!(system_prompt_template(PromptProfile::Full).contains("{cwd}"));
        assert!(system_prompt_template(PromptProfile::Compact).contains("{cwd}"));
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
