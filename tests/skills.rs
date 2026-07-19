//! Hermetic tests for the skill mechanism: dir resolution, enumeration, the
//! `skill` tool round-trip, and the system-prompt `<available_skills>` block.
//! Uses a throwaway `.opencode/skills` tree under a tempdir — no network, no
//! real skills required.

use opencode_rust::skills;
use opencode_rust::tools::{SkillTool, Tool, ToolCtx};
use serde_json::json;
use std::path::Path;

fn write_skill(root: &Path, name: &str, frontmatter: &str, body: &str) {
    let dir = root.join(".opencode/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), format!("{frontmatter}{body}")).unwrap();
}

#[test]
fn enumerate_parses_and_dedups() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    write_skill(
        a.path(),
        "demo",
        "---\nname: demo\ndescription: A demo skill.\n---\n",
        "# Demo\nhello\n",
    );
    // A second "demo" in a lower-precedence dir must be shadowed (first wins).
    write_skill(
        b.path(),
        "demo",
        "---\nname: demo\ndescription: SHADOWED.\n---\n",
        "nope\n",
    );
    write_skill(
        b.path(),
        "other",
        "---\nname: other\ndescription: Another.\n---\n",
        "x\n",
    );

    let dirs = vec![
        a.path().join(".opencode/skills"),
        b.path().join(".opencode/skills"),
    ];
    let skills = skills::enumerate(&dirs);
    assert_eq!(skills.len(), 2);
    let demo = skills.iter().find(|s| s.name == "demo").unwrap();
    assert_eq!(demo.description, "A demo skill."); // not SHADOWED
    assert!(skills.iter().any(|s| s.name == "other"));
}

#[test]
fn enumerate_skips_malformed() {
    let a = tempfile::tempdir().unwrap();
    // Unterminated frontmatter → skipped, not fatal.
    write_skill(a.path(), "broken", "---\nname: broken\n(no closing fence)\n", "");
    write_skill(a.path(), "good", "---\nname: good\ndescription: ok\n---\n", "body");
    let dirs = vec![a.path().join(".opencode/skills")];
    let skills = skills::enumerate(&dirs);
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "good");
}

#[test]
fn enumerate_falls_back_to_dirname_without_name() {
    let a = tempfile::tempdir().unwrap();
    write_skill(a.path(), "nameless", "# no frontmatter\n", "just markdown");
    let dirs = vec![a.path().join(".opencode/skills")];
    let skills = skills::enumerate(&dirs);
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "nameless");
    assert!(skills[0].description.is_empty());
}

#[test]
fn skill_tool_loads_content_wrapped() {
    let a = tempfile::tempdir().unwrap();
    write_skill(
        a.path(),
        "demo",
        "---\nname: demo\ndescription: A demo skill.\n---\n",
        "# Demo\nDo the thing.\n",
    );
    let dirs = vec![a.path().join(".opencode/skills")];
    let tool = SkillTool::new(dirs);
    let mut ctx = ToolCtx::new(a.path().to_path_buf());

    let out = tool.execute(json!({"name": "demo"}), &mut ctx).unwrap();
    assert!(out.output.contains("<skill_content name=\"demo\">"));
    assert!(out.output.contains("Base directory for this skill:"));
    assert!(out.output.contains("Do the thing."));
    assert!(out.output.contains("</skill_content>"));
}

#[test]
fn skill_tool_errors_on_missing_and_traversal() {
    let a = tempfile::tempdir().unwrap();
    let dirs = vec![a.path().join(".opencode/skills")];
    let tool = SkillTool::new(dirs);
    let mut ctx = ToolCtx::new(a.path().to_path_buf());

    assert!(tool.execute(json!({"name": "nope"}), &mut ctx).is_err());
    assert!(tool.execute(json!({"name": "../secret"}), &mut ctx).is_err());
    assert!(tool.execute(json!({"name": ""}), &mut ctx).is_err());
}

#[test]
fn system_prompt_section_lists_installed() {
    let a = tempfile::tempdir().unwrap();
    write_skill(
        a.path(),
        "example-cli",
        "---\nname: example-cli\ndescription: Drives the example CLI.\n---\n",
        "body",
    );
    let dirs = vec![a.path().join(".opencode/skills")];
    let skills = skills::enumerate(&dirs);
    let section = skills::system_prompt_section(&skills).unwrap();
    assert!(section.contains("<available_skills>"));
    assert!(section.contains("<name>example-cli</name>"));
    assert!(section.contains("Drives the example CLI."));
    assert!(section.contains("file://"));
}
