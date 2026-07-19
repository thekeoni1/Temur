//! Skill resolution + enumeration.
//!
//! A "skill" is an instruction file the model loads on demand via the `skill`
//! tool: `<skill-dir>/<name>/SKILL.md`, with optional `playbooks/` and assets
//! beside it. This module resolves the effective skill-directory search list,
//! enumerates installed skills (parsing minimal YAML frontmatter for `name`
//! and `description`), and renders the `<available_skills>` block advertised in
//! the system prompt.
//!
//! No YAML library — the minimal frontmatter parser keeps this a small binary.

use std::path::{Path, PathBuf};

/// Directory searched under the workspace cwd and under `$HOME`. Matches the
/// layout an external CLI skill ships in, so real skills drop in unmodified.
const SKILLS_SUBDIR: &str = ".temur/skills";
/// Pre-rename layout, still searched (after the primary) for one release.
const LEGACY_SKILLS_SUBDIR: &str = ".opencode/skills";

/// Metadata for one installed skill.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    /// The skill's own directory: `<skill-dir>/<name>`.
    pub dir: PathBuf,
}

fn add_unique(dirs: &mut Vec<PathBuf>, p: PathBuf) {
    if !dirs.iter().any(|d| d == &p) {
        dirs.push(p);
    }
}

/// Resolve the effective skill-directory search list.
///
/// Order: explicit `override_list` entries (`:`-separated, blanks skipped) in
/// the order given, then `<cwd>/.temur/skills` and its legacy fallback
/// `<cwd>/.opencode/skills`, then the same pair under `<home>` — each appended
/// only if not already present (dedup, first occurrence wins). The defaults
/// are always searched.
pub fn skill_dirs(override_list: Option<&str>, cwd: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(list) = override_list {
        for entry in list.split(':') {
            if !entry.is_empty() {
                add_unique(&mut dirs, PathBuf::from(entry));
            }
        }
    }
    add_unique(&mut dirs, cwd.join(SKILLS_SUBDIR));
    add_unique(&mut dirs, cwd.join(LEGACY_SKILLS_SUBDIR));
    if let Some(h) = home {
        add_unique(&mut dirs, h.join(SKILLS_SUBDIR));
        add_unique(&mut dirs, h.join(LEGACY_SKILLS_SUBDIR));
    }
    dirs
}

/// Parse minimal YAML frontmatter. Returns `(name, description)` — either may
/// be empty when absent. Returns `None` only when a frontmatter block is
/// opened (leading `---`) but never closed (treated as malformed).
pub fn parse_frontmatter(md: &str) -> Option<(String, String)> {
    let rest = match md
        .strip_prefix("---\n")
        .or_else(|| md.strip_prefix("---\r\n"))
    {
        Some(r) => r,
        None => return Some((String::new(), String::new())), // no frontmatter block
    };
    let mut name = String::new();
    let mut description = String::new();
    let mut closed = false;
    for line in rest.lines() {
        let trimmed = line.trim_end();
        if trimmed == "---" {
            closed = true;
            break;
        }
        if let Some(v) = trimmed.strip_prefix("name:") {
            name = unquote(v);
        } else if let Some(v) = trimmed.strip_prefix("description:") {
            description = unquote(v);
        }
    }
    if !closed {
        return None; // unterminated frontmatter — malformed
    }
    Some((name, description))
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 2
        && ((b[0] == b'"' && b[b.len() - 1] == b'"')
            || (b[0] == b'\'' && b[b.len() - 1] == b'\''))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Enumerate installed skills across `dirs`. For each `<dir>/<name>/SKILL.md`,
/// parse `name`/`description` from frontmatter (falling back to the directory
/// name when `name:` is absent). Dedup by name, first-dir-wins (same
/// precedence as loading). Malformed SKILL.md files are skipped with a stderr
/// warning; enumeration never fails.
pub fn enumerate(dirs: &[PathBuf]) -> Vec<SkillInfo> {
    let mut out: Vec<SkillInfo> = Vec::new();
    for dir in dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => continue, // dir absent — normal
        };
        let mut subdirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect();
        subdirs.sort(); // deterministic order within a directory
        for sub in subdirs {
            let md = sub.join("SKILL.md");
            let content = match std::fs::read_to_string(&md) {
                Ok(c) => c,
                Err(_) => continue, // no SKILL.md here — not a skill dir
            };
            let dirname = sub
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let (name, description) = match parse_frontmatter(&content) {
                Some(fm) => fm,
                None => {
                    eprintln!(
                        "temur: skipping malformed skill (unterminated frontmatter): {}",
                        md.display()
                    );
                    continue;
                }
            };
            let name = if name.is_empty() { dirname } else { name };
            if name.is_empty() {
                continue;
            }
            if out.iter().any(|s| s.name == name) {
                continue; // first-dir-wins
            }
            out.push(SkillInfo {
                name,
                description,
                dir: sub,
            });
        }
    }
    out
}

/// Load a skill's SKILL.md by name. Returns `(skill_dir, content)` for the
/// first matching `<dir>/<name>/SKILL.md`. `None` if not found in any dir.
pub fn load(dirs: &[PathBuf], name: &str) -> Option<(PathBuf, String)> {
    for dir in dirs {
        let sub = dir.join(name);
        let md = sub.join("SKILL.md");
        if let Ok(content) = std::fs::read_to_string(&md) {
            return Some((sub, content));
        }
    }
    None
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Render the `<available_skills>` block for the system prompt. Returns `None`
/// when no skills are installed (nothing to advertise). Shape mirrors the
/// captured OpenCode prompt so behavior stays familiar to the model.
pub fn system_prompt_section(skills: &[SkillInfo]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let mut s = String::new();
    s.push_str("\n\nSkills provide specialized instructions and workflows for specific tasks.\n");
    s.push_str("Use the skill tool to load a skill when a task matches its description.\n");
    s.push_str("<available_skills>\n");
    for sk in skills {
        s.push_str("  <skill>\n");
        s.push_str(&format!("    <name>{}</name>\n", xml_escape(&sk.name)));
        s.push_str(&format!(
            "    <description>{}</description>\n",
            xml_escape(&sk.description)
        ));
        s.push_str(&format!("    <location>file://{}</location>\n", sk.dir.display()));
        s.push_str("  </skill>\n");
    }
    s.push_str("</available_skills>");
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_dedups_and_orders() {
        let cwd = Path::new("/work");
        let home = Path::new("/home/dev");
        let dirs = skill_dirs(Some("/a::/b:/work/.temur/skills"), cwd, Some(home));
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/a"),
                PathBuf::from("/b"),
                PathBuf::from("/work/.temur/skills"), // override entry == cwd default: kept once
                PathBuf::from("/work/.opencode/skills"),
                PathBuf::from("/home/dev/.temur/skills"),
                PathBuf::from("/home/dev/.opencode/skills"),
            ]
        );
    }

    #[test]
    fn resolution_defaults_only() {
        let dirs = skill_dirs(None, Path::new("/w"), None);
        // Primary layout first, legacy pre-rename fallback second.
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/w/.temur/skills"),
                PathBuf::from("/w/.opencode/skills"),
            ]
        );
    }

    #[test]
    fn frontmatter_parses_quoted_and_bare() {
        let (n, d) = parse_frontmatter("---\nname: \"foo\"\ndescription: bar baz\n---\nbody").unwrap();
        assert_eq!(n, "foo");
        assert_eq!(d, "bar baz");
    }

    #[test]
    fn frontmatter_absent_is_empty_not_error() {
        let (n, d) = parse_frontmatter("# just markdown\n").unwrap();
        assert!(n.is_empty() && d.is_empty());
    }

    #[test]
    fn frontmatter_unterminated_is_malformed() {
        assert!(parse_frontmatter("---\nname: foo\nno closing fence\n").is_none());
    }

    #[test]
    fn section_none_when_empty() {
        assert!(system_prompt_section(&[]).is_none());
    }
}
