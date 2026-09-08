//! T55: project instructions, read once at startup from the working
//! directory (and its repository root) into the system prompt.
//!
//! Two names, one feature. `TEMUR.md` is temur's own; `AGENTS.md` is the
//! cross-tool convention other agents already read, so a repository that
//! has one works here unmodified, the way `.opencode/skills` drops in.
//! When both sit in the SAME directory, TEMUR.md wins and AGENTS.md is
//! ignored there; the other location is unaffected.
//!
//! Read ONCE per process. The block is part of the prompt prefix, so
//! re-reading it per turn would break the T28 prefix-stability invariant
//! that keeps a provider's prompt cache warm; a `/model` prompt-profile
//! swap rebuilds the prompt from the text captured here, never from disk.

use crate::tools::KeyGuard;
use std::path::{Path, PathBuf};

/// Candidate file names in precedence order WITHIN one directory.
pub const NAMES: [&str; 2] = ["TEMUR.md", "AGENTS.md"];

/// The cap is context-scaled exactly like tool output (`tools::MIN_OUTPUT_CHARS`
/// / `MAX_OUTPUT_CHARS`): a compact-profile window lands at the low end by
/// construction, a Claude-class window at the high end.
pub const MIN_CAP_CHARS: usize = 4_000;
pub const MAX_CAP_CHARS: usize = 30_000;

/// Chars of project instructions allowed for a given context window.
pub fn cap_chars(context_window: Option<u64>) -> usize {
    match context_window {
        Some(w) => w.clamp(MIN_CAP_CHARS as u64, MAX_CAP_CHARS as u64) as usize,
        None => MAX_CAP_CHARS,
    }
}

/// What one location contributed.
struct Found {
    name: &'static str,
    /// "root" or "cwd", for the banner line when both contributed.
    where_: &'static str,
    text: String,
}

/// The result of one startup load. `block` is appended to the system
/// prompt; `summary` is the banner / `/status` / doctor line. Both are
/// absent when no file exists, because absence is the common case and
/// must stay silent.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub block: String,
    pub summary: Option<String>,
}

impl Loaded {
    pub fn is_empty(&self) -> bool {
        self.block.is_empty()
    }
}

/// The repository root containing `cwd`, by walking up for a `.git` entry.
///
/// DEVIATION from the brief, which specified `git rev-parse
/// --show-toplevel`: temur shells out to git NOWHERE else (verified), and
/// it is a zero-runtime-dependency static binary, so making project
/// instructions depend on a `git` on PATH would be a new runtime
/// dependency for a directory lookup. `.git` is matched as a file or a
/// directory, so worktrees and submodules (where it is a file) resolve
/// too. Nothing between root and cwd is read; this only finds the root.
fn repo_root(cwd: &Path) -> Option<PathBuf> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        if d.join(".git").exists() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// TEMUR.md, else AGENTS.md, from one directory.
///
/// The guard runs BEFORE any open, exactly as the read tool does it, so a
/// name symlinked into a secrets directory is never opened and its
/// existence never leaks. A read error that is not absence is one stderr
/// line and otherwise silent: a project file is a convenience, never a
/// reason to fail startup.
fn find_in(dir: &Path, where_: &'static str, guard: &KeyGuard) -> Option<Found> {
    for name in NAMES {
        let path = dir.join(name);
        if guard.check(&path).is_err() {
            continue;
        }
        if !path.is_file() {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                return Some(Found {
                    name,
                    where_,
                    text: text.trim_end().to_string(),
                })
            }
            Err(e) => {
                eprintln!("temur: could not read {}: {e}", path.display());
                continue;
            }
        }
    }
    None
}

/// Digits with thousands separators, for the banner line.
fn commas(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Load project instructions for `cwd`. `enabled` false yields
/// [`Loaded::default`] without touching the filesystem.
pub fn load(cwd: &Path, guard: &KeyGuard, cap: usize, enabled: bool) -> Loaded {
    if !enabled {
        return Loaded::default();
    }
    let mut found: Vec<Found> = Vec::new();
    // Root first, and only when it is a DIFFERENT directory from the cwd:
    // a cwd that is itself the repository root contributes once, not twice.
    if let Some(root) = repo_root(cwd) {
        if root != cwd {
            found.extend(find_in(&root, "root", guard));
        }
    }
    found.extend(find_in(cwd, "cwd", guard));
    if found.is_empty() {
        return Loaded::default();
    }

    let total_bytes: u64 = found.iter().map(|f| f.text.len() as u64).sum();

    // The cap is on the COMBINED instructions: keep the head, drop the
    // tail, in discovery order, so the root's file survives a cwd file
    // that would otherwise push it out.
    let mut budget = cap;
    let mut truncated = false;
    let mut kept: Vec<(&'static str, String)> = Vec::new();
    for f in &found {
        if budget == 0 {
            truncated = true;
            break;
        }
        let n = f.text.chars().count();
        if n <= budget {
            budget -= n;
            kept.push((f.name, f.text.clone()));
        } else {
            let head: String = f.text.chars().take(budget).collect();
            budget = 0;
            truncated = true;
            kept.push((f.name, head));
        }
    }

    let mut block = String::new();
    for (name, text) in &kept {
        if text.is_empty() {
            continue;
        }
        block.push_str(&format!("\n\n<project_instructions source=\"{name}\">\n"));
        block.push_str(text);
        block.push_str("\n</project_instructions>");
    }
    if truncated && !block.is_empty() {
        block.push_str(&format!(
            "\n[project instructions truncated to the first {} characters; the files total {} bytes]",
            commas(cap as u64),
            commas(total_bytes)
        ));
    }

    // "TEMUR.md (1,204 bytes)" for one; "AGENTS.md (root) + TEMUR.md
    // (cwd), 3,410 bytes" for two; ", truncated" whenever text was cut,
    // so a shortened prompt is never silent about it.
    let mut summary = if found.len() == 1 {
        format!("{} ({} bytes)", found[0].name, commas(total_bytes))
    } else {
        let names: Vec<String> = found
            .iter()
            .map(|f| format!("{} ({})", f.name, f.where_))
            .collect();
        format!("{}, {} bytes", names.join(" + "), commas(total_bytes))
    };
    if truncated {
        summary.push_str(", truncated");
    }

    Loaded {
        block,
        summary: Some(format!("project instructions: {summary}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(dir: &Path) {
        std::fs::create_dir_all(dir.join(".git")).unwrap();
    }
    fn load_here(dir: &Path) -> Loaded {
        load(dir, &KeyGuard::empty(), MAX_CAP_CHARS, true)
    }

    #[test]
    fn cwd_file_only() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("TEMUR.md"), "build with cargo").unwrap();
        let l = load_here(d.path());
        assert!(l.block.contains("<project_instructions source=\"TEMUR.md\">"));
        assert!(l.block.contains("build with cargo"));
        assert_eq!(l.summary.as_deref(), Some("project instructions: TEMUR.md (16 bytes)"));
    }

    #[test]
    fn root_file_only_from_a_subdirectory() {
        let d = tempfile::tempdir().unwrap();
        repo(d.path());
        std::fs::write(d.path().join("TEMUR.md"), "root rules").unwrap();
        let sub = d.path().join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        let l = load_here(&sub);
        assert!(l.block.contains("root rules"), "{}", l.block);
        // One file, so the summary carries no location words.
        assert_eq!(l.summary.as_deref(), Some("project instructions: TEMUR.md (10 bytes)"));
    }

    #[test]
    fn both_locations_load_root_first() {
        let d = tempfile::tempdir().unwrap();
        repo(d.path());
        std::fs::write(d.path().join("TEMUR.md"), "ROOTTEXT").unwrap();
        let sub = d.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("TEMUR.md"), "CWDTEXT").unwrap();
        let l = load_here(&sub);
        let root_at = l.block.find("ROOTTEXT").expect("root text present");
        let cwd_at = l.block.find("CWDTEXT").expect("cwd text present");
        assert!(root_at < cwd_at, "root must come first:\n{}", l.block);
        assert_eq!(
            l.summary.as_deref(),
            Some("project instructions: TEMUR.md (root) + TEMUR.md (cwd), 15 bytes")
        );
    }

    #[test]
    fn temur_md_beats_agents_md_in_the_same_directory_only() {
        let d = tempfile::tempdir().unwrap();
        repo(d.path());
        // Root has both: TEMUR.md wins THERE.
        std::fs::write(d.path().join("TEMUR.md"), "ROOTWINS").unwrap();
        std::fs::write(d.path().join("AGENTS.md"), "ROOTLOSES").unwrap();
        // The other location has only AGENTS.md, which still loads.
        let sub = d.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("AGENTS.md"), "CWDAGENTS").unwrap();
        let l = load_here(&sub);
        assert!(l.block.contains("ROOTWINS"), "{}", l.block);
        assert!(!l.block.contains("ROOTLOSES"), "{}", l.block);
        assert!(l.block.contains("CWDAGENTS"), "{}", l.block);
        assert_eq!(
            l.summary.as_deref(),
            Some("project instructions: TEMUR.md (root) + AGENTS.md (cwd), 17 bytes")
        );
    }

    #[test]
    fn a_cwd_that_is_the_root_contributes_once() {
        let d = tempfile::tempdir().unwrap();
        repo(d.path());
        std::fs::write(d.path().join("TEMUR.md"), "once").unwrap();
        let l = load_here(d.path());
        assert_eq!(l.block.matches("<project_instructions").count(), 1, "{}", l.block);
    }

    #[test]
    fn absence_is_silent_and_contributes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let l = load_here(d.path());
        assert_eq!(l, Loaded::default());
        assert!(l.is_empty());
        assert!(l.summary.is_none());
        assert_eq!(l.block, "");
    }

    #[test]
    fn a_cwd_outside_any_repository_contributes_only_itself() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("AGENTS.md"), "just me").unwrap();
        let l = load_here(d.path());
        assert_eq!(l.block.matches("<project_instructions").count(), 1);
        assert!(l.block.contains("just me"));
    }

    #[test]
    fn over_cap_keeps_the_head_drops_the_tail_and_says_so() {
        let d = tempfile::tempdir().unwrap();
        let text = format!("{}TAILMARKER", "x".repeat(200));
        std::fs::write(d.path().join("TEMUR.md"), &text).unwrap();
        let l = load(d.path(), &KeyGuard::empty(), 50, true);
        assert!(l.block.contains(&"x".repeat(50)), "head kept");
        assert!(!l.block.contains("TAILMARKER"), "tail dropped:\n{}", l.block);
        assert!(l.block.contains("truncated to the first 50 characters"), "{}", l.block);
        assert!(l.block.contains("210 bytes"), "{}", l.block);
        assert!(
            l.summary.as_deref().unwrap().ends_with(", truncated"),
            "{:?}",
            l.summary
        );
    }

    #[test]
    fn exactly_at_the_cap_is_untouched() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("TEMUR.md"), "y".repeat(50)).unwrap();
        let l = load(d.path(), &KeyGuard::empty(), 50, true);
        assert!(!l.block.contains("truncated"), "{}", l.block);
        assert!(!l.summary.as_deref().unwrap().contains("truncated"));
        assert!(l.block.contains(&"y".repeat(50)));
    }

    #[test]
    fn the_cap_is_context_scaled_like_tool_output() {
        assert_eq!(cap_chars(None), MAX_CAP_CHARS);
        assert_eq!(cap_chars(Some(1_000)), MIN_CAP_CHARS);
        assert_eq!(cap_chars(Some(8_192)), 8_192);
        assert_eq!(cap_chars(Some(1_000_000)), MAX_CAP_CHARS);
    }

    #[test]
    fn a_guarded_file_is_never_read_and_never_reaches_the_prompt() {
        let d = tempfile::tempdir().unwrap();
        let secrets = d.path().join("secrets");
        std::fs::create_dir_all(&secrets).unwrap();
        let key = secrets.join("api.key");
        std::fs::write(&key, "SUPERSECRETKEYMATERIAL").unwrap();
        // The project file IS the guarded file, by symlink.
        std::os::unix::fs::symlink(&key, d.path().join("TEMUR.md")).unwrap();
        let guard = KeyGuard::from_paths(vec![key.clone()]);
        let l = load(d.path(), &guard, MAX_CAP_CHARS, true);
        assert!(!l.block.contains("SUPERSECRETKEYMATERIAL"), "{}", l.block);
        assert_eq!(l, Loaded::default(), "a guarded name contributes nothing");
    }

    #[test]
    fn disabled_touches_nothing() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("TEMUR.md"), "should not load").unwrap();
        let l = load(d.path(), &KeyGuard::empty(), MAX_CAP_CHARS, false);
        assert_eq!(l, Loaded::default());
    }

    #[test]
    fn trailing_whitespace_is_trimmed_but_the_text_is_otherwise_byte_for_byte() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("TEMUR.md"), "  keep   inner  \nlines\n\n\n").unwrap();
        let l = load_here(d.path());
        assert!(l.block.contains("  keep   inner  \nlines"), "{}", l.block);
        assert!(l.block.contains("lines\n</project_instructions>"), "{}", l.block);
    }

    /// Read ONCE per process: the captured block is what a `/model`
    /// prompt-profile swap reassembles from, in EITHER profile, and it
    /// does not follow the file if the file changes underneath.
    #[test]
    fn the_captured_block_survives_a_profile_swap_and_never_re_reads_disk() {
        use crate::tools::PromptProfile;
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("TEMUR.md"), "ORIGINALTEXT").unwrap();
        let loaded = load_here(d.path());
        std::fs::write(d.path().join("TEMUR.md"), "CHANGEDTEXT").unwrap();

        for profile in [PromptProfile::Full, PromptProfile::Compact] {
            let out = crate::prompt::assemble(
                crate::prompt::system_prompt_template(profile),
                None,
                &loaded.block,
            );
            assert!(out.contains("ORIGINALTEXT"), "{profile:?}: {out}");
            assert!(!out.contains("CHANGEDTEXT"), "{profile:?} re-read disk");
            assert!(out.contains("</project_instructions>"), "{profile:?}");
        }
    }

    #[test]
    fn commas_group_digits() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1_204), "1,204");
        assert_eq!(commas(3_410), "3,410");
        assert_eq!(commas(1_234_567), "1,234,567");
    }
}
