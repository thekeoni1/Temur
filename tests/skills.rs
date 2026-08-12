//! Hermetic tests for the skill mechanism: dir resolution, enumeration, the
//! `skill` tool round-trip, and the system-prompt `<available_skills>` block.
//! Uses a throwaway `.temur/skills` tree under a tempdir — no network, no
//! real skills required.

use temur::skills;
use temur::tools::{SkillTool, Tool, ToolCtx};
use serde_json::json;
use std::path::Path;

fn write_skill(root: &Path, name: &str, frontmatter: &str, body: &str) {
    write_skill_in(root, ".temur/skills", name, frontmatter, body);
}

fn write_skill_in(root: &Path, subdir: &str, name: &str, frontmatter: &str, body: &str) {
    let dir = root.join(subdir).join(name);
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
        a.path().join(".temur/skills"),
        b.path().join(".temur/skills"),
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
    let dirs = vec![a.path().join(".temur/skills")];
    let skills = skills::enumerate(&dirs);
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "good");
}

#[test]
fn enumerate_falls_back_to_dirname_without_name() {
    let a = tempfile::tempdir().unwrap();
    write_skill(a.path(), "nameless", "# no frontmatter\n", "just markdown");
    let dirs = vec![a.path().join(".temur/skills")];
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
    let dirs = vec![a.path().join(".temur/skills")];
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
    let dirs = vec![a.path().join(".temur/skills")];
    let tool = SkillTool::new(dirs);
    let mut ctx = ToolCtx::new(a.path().to_path_buf());

    assert!(tool.execute(json!({"name": "nope"}), &mut ctx).is_err());
    assert!(tool.execute(json!({"name": "../secret"}), &mut ctx).is_err());
    assert!(tool.execute(json!({"name": ""}), &mut ctx).is_err());
}

#[test]
fn legacy_opencode_dir_still_found_via_defaults() {
    // A skill installed only under the pre-rename `.opencode/skills` layout
    // must still resolve through the default search list (one-release compat),
    // and the primary `.temur/skills` layout must shadow it by name.
    let a = tempfile::tempdir().unwrap();
    write_skill_in(
        a.path(),
        ".opencode/skills",
        "legacy",
        "---\nname: legacy\ndescription: From the old layout.\n---\n",
        "body",
    );
    write_skill_in(
        a.path(),
        ".opencode/skills",
        "shadowed",
        "---\nname: shadowed\ndescription: OLD.\n---\n",
        "old",
    );
    write_skill(
        a.path(),
        "shadowed",
        "---\nname: shadowed\ndescription: NEW.\n---\n",
        "new",
    );

    let dirs = skills::skill_dirs(None, a.path(), None);
    let skills = skills::enumerate(&dirs);
    assert_eq!(skills.len(), 2);
    let legacy = skills.iter().find(|s| s.name == "legacy").unwrap();
    assert_eq!(legacy.description, "From the old layout.");
    let shadowed = skills.iter().find(|s| s.name == "shadowed").unwrap();
    assert_eq!(shadowed.description, "NEW."); // primary dir wins
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
    let dirs = vec![a.path().join(".temur/skills")];
    let skills = skills::enumerate(&dirs);
    let section = skills::system_prompt_section(&skills).unwrap();
    assert!(section.contains("<available_skills>"));
    assert!(section.contains("<name>example-cli</name>"));
    assert!(section.contains("Drives the example CLI."));
    assert!(section.contains("file://"));
}

// ------------------------------------------------------------- T28 (P2)

/// Build a skill whose body is over `target` chars, with realistic heading
/// structure: a short intro, then numbered chapters with subsections.
fn big_body(target: usize) -> String {
    let mut s = String::from("This skill drives the widget CLI end to end.\n\n");
    let mut i = 1;
    while s.len() < target {
        s.push_str(&format!("## Chapter {i}\n\nOverview of chapter {i}.\n\n"));
        for j in 1..=3 {
            s.push_str(&format!("### Chapter {i}.{j}\n\n"));
            for k in 0..6 {
                s.push_str(&format!(
                    "Step {k} of chapter {i}.{j}: run the widget command and check the output carefully.\n"
                ));
            }
            s.push('\n');
        }
        i += 1;
    }
    s
}

const FM: &str = "---\nname: demo\ndescription: A demo skill.\n---\n";

fn tool_over(root: &Path) -> SkillTool {
    SkillTool::new(vec![root.join(".temur/skills")])
}

#[test]
fn small_skill_returns_minified_content_with_the_frontmatter_gone() {
    let a = tempfile::tempdir().unwrap();
    write_skill(a.path(), "demo", FM, "# Demo\n\n\n\nDo the thing.   \n");
    let tool = tool_over(a.path());
    let mut ctx = ToolCtx::new(a.path().to_path_buf());
    let out = tool.execute(json!({"name": "demo"}), &mut ctx).unwrap();
    assert!(out.output.contains("<skill_content name=\"demo\">"), "{}", out.output);
    // Minified: frontmatter dropped, blank run collapsed, trailing ws gone.
    assert!(!out.output.contains("description: A demo skill."), "{}", out.output);
    assert!(out.output.contains("# Demo\n\nDo the thing.\n"), "{:?}", out.output);
}

/// THE effectiveness pin. An over-cap skill must come back as an index that
/// is both dramatically smaller than the file AND small enough to survive
/// the cap it was built to respect, or the feature does nothing.
#[test]
fn oversized_skill_returns_an_index_that_is_small_and_never_truncated() {
    let a = tempfile::tempdir().unwrap();
    let body = big_body(40_000);
    write_skill(a.path(), "demo", FM, &body);
    let raw_len = std::fs::read_to_string(
        a.path().join(".temur/skills/demo/SKILL.md"),
    )
    .unwrap()
    .chars()
    .count();
    assert!(raw_len > 40_000, "fixture is {raw_len} chars");

    let tool = tool_over(a.path());
    let mut ctx = ToolCtx::new(a.path().to_path_buf());
    let cap = ctx.output_cap; // 30,000, the default ceiling
    let out = tool.execute(json!({"name": "demo"}), &mut ctx).unwrap();
    let n = out.output.chars().count();

    assert!(out.output.starts_with("<skill_index name=\"demo\">"), "{}", out.output);
    assert!(out.output.ends_with("</skill_index>"), "tail: {:?}", &out.output[n - 40..]);
    assert!(
        n * 10 < raw_len,
        "index must be under 10% of the {raw_len}-char skill, got {n}"
    );
    assert!(n <= cap, "the index must never be truncated: {n} > {cap}");
    // It says what it is and how to get the rest, and claims no summary.
    assert!(out.output.contains("Nothing is summarized and nothing is omitted"), "{}", out.output);
    assert!(out.output.contains("\"section\": \"<number or heading>\""), "{}", out.output);
    // The intro survives verbatim, and every chapter is listed with a size.
    assert!(out.output.contains("This skill drives the widget CLI end to end."), "{}", out.output);
    assert!(out.output.contains("1. ## Chapter 1 ("), "{}", out.output);
    assert!(out.output.contains("2. ### Chapter 1.1 ("), "{}", out.output);
}

#[test]
fn section_fetch_by_number_by_text_and_case_insensitively() {
    let a = tempfile::tempdir().unwrap();
    write_skill(
        a.path(),
        "demo",
        FM,
        "# Top\nintro\n## Setup\nsetup body\n## Usage\nusage body\n",
    );
    let tool = tool_over(a.path());
    let mut ctx = ToolCtx::new(a.path().to_path_buf());

    // 2 is "## Setup" (1 is "# Top", which owns everything).
    let by_num = tool.execute(json!({"name": "demo", "section": "2"}), &mut ctx).unwrap();
    assert!(by_num.output.contains("<skill_section name=\"demo\" number=\"2\" title=\"Setup\">"), "{}", by_num.output);
    assert!(by_num.output.contains("setup body"), "{}", by_num.output);
    assert!(!by_num.output.contains("usage body"), "{}", by_num.output);

    // A JSON number, not a string.
    let as_number = tool.execute(json!({"name": "demo", "section": 2}), &mut ctx).unwrap();
    assert_eq!(as_number.output, by_num.output, "number and string agree");

    // Exact heading text, and a sloppier spelling of it.
    let by_text = tool.execute(json!({"name": "demo", "section": "Setup"}), &mut ctx).unwrap();
    assert_eq!(by_text.output, by_num.output);
    for spelling in ["setup", "  SETUP ", "## Setup"] {
        let out = tool.execute(json!({"name": "demo", "section": spelling}), &mut ctx).unwrap();
        assert_eq!(out.output, by_num.output, "spelling {spelling:?}");
    }
}

#[test]
fn section_text_is_the_exact_slice_of_the_minified_body() {
    let a = tempfile::tempdir().unwrap();
    write_skill(a.path(), "demo", FM, "# Top\nt\n## A\naaa\n### A1\nnested\n## B\nbbb\n");
    let tool = tool_over(a.path());
    let mut ctx = ToolCtx::new(a.path().to_path_buf());
    let out = tool.execute(json!({"name": "demo", "section": "A"}), &mut ctx).unwrap();
    // Hierarchical: A carries A1 with it, and stops before B.
    let payload_start = out.output.find("\n\n").unwrap() + 2;
    let expected = "## A\naaa\n### A1\nnested\n";
    assert_eq!(&out.output[payload_start..payload_start + expected.len()], expected);
}

#[test]
fn duplicate_headings_take_the_first_and_say_how_to_reach_the_others() {
    let a = tempfile::tempdir().unwrap();
    write_skill(a.path(), "demo", FM, "## Options\nfirst\n## Other\nx\n## Options\nsecond\n");
    let tool = tool_over(a.path());
    let mut ctx = ToolCtx::new(a.path().to_path_buf());
    let out = tool.execute(json!({"name": "demo", "section": "Options"}), &mut ctx).unwrap();
    assert!(out.output.contains("first"), "{}", out.output);
    assert!(!out.output.contains("second"), "{}", out.output);
    assert!(out.output.contains("2 sections share this heading"), "{}", out.output);
    assert!(out.output.contains("1, 3"), "{}", out.output);
    // Asking by number reaches the other one, with no note.
    let third = tool.execute(json!({"name": "demo", "section": 3}), &mut ctx).unwrap();
    assert!(third.output.contains("second"), "{}", third.output);
    assert!(!third.output.contains("share this heading"), "{}", third.output);
}

#[test]
fn a_bad_section_relists_the_sections_instead_of_just_failing() {
    let a = tempfile::tempdir().unwrap();
    write_skill(a.path(), "demo", FM, "## Setup\ns\n## Usage\nu\n");
    let tool = tool_over(a.path());
    let mut ctx = ToolCtx::new(a.path().to_path_buf());
    let err = tool
        .execute(json!({"name": "demo", "section": "Instalation"}), &mut ctx)
        .unwrap_err()
        .to_string();
    assert!(err.contains("has no section 'Instalation'"), "{err}");
    assert!(err.contains("1. ## Setup"), "{err}");
    assert!(err.contains("2. ## Usage"), "{err}");
}

#[test]
fn section_never_touches_the_filesystem_so_a_path_is_just_a_miss() {
    let a = tempfile::tempdir().unwrap();
    write_skill(a.path(), "demo", FM, "## Setup\ns\n");
    let tool = tool_over(a.path());
    let mut ctx = ToolCtx::new(a.path().to_path_buf());
    let err = tool
        .execute(json!({"name": "demo", "section": "../../etc/passwd"}), &mut ctx)
        .unwrap_err()
        .to_string();
    assert!(err.contains("has no section"), "a miss, not a path error: {err}");
    assert!(!err.contains("must be a bare skill name"), "{err}");
    // The name guard is untouched and still fires on the name itself.
    let name_err = tool
        .execute(json!({"name": "../secret", "section": "1"}), &mut ctx)
        .unwrap_err()
        .to_string();
    assert!(name_err.contains("must be a bare skill name"), "{name_err}");
}

#[test]
fn a_heading_less_skill_says_so_and_points_at_the_bare_call() {
    let a = tempfile::tempdir().unwrap();
    write_skill(a.path(), "demo", FM, "just prose, no headings at all\n");
    let tool = tool_over(a.path());
    let mut ctx = ToolCtx::new(a.path().to_path_buf());
    let err = tool
        .execute(json!({"name": "demo", "section": "1"}), &mut ctx)
        .unwrap_err()
        .to_string();
    assert!(err.contains("has no headings to select from"), "{err}");
    assert!(err.contains("{\"name\": \"demo\"}"), "{err}");
}

#[test]
fn sectioning_works_on_a_small_skill_too() {
    let a = tempfile::tempdir().unwrap();
    write_skill(a.path(), "demo", FM, "## One\n1\n## Two\n2\n");
    let tool = tool_over(a.path());
    let mut ctx = ToolCtx::new(a.path().to_path_buf());
    let out = tool.execute(json!({"name": "demo", "section": "Two"}), &mut ctx).unwrap();
    assert!(out.output.contains("## Two\n2\n"), "{}", out.output);
}

#[test]
fn at_the_cap_exactly_is_full_mode_not_an_index() {
    let a = tempfile::tempdir().unwrap();
    // Big enough that an index is genuinely the smaller answer, so the
    // boundary being tested is the cap and nothing else.
    write_skill(a.path(), "demo", FM, &big_body(20_000));
    let tool = tool_over(a.path());
    let mut ctx = ToolCtx::new(a.path().to_path_buf());
    let full = tool.execute(json!({"name": "demo"}), &mut ctx).unwrap();
    let exact = full.output.chars().count();
    // Cap set to exactly the wrapped size: still full, byte for byte. The
    // rule is <= cap, so at-cap output is never sent through truncation.
    ctx.output_cap = exact;
    let at_cap = tool.execute(json!({"name": "demo"}), &mut ctx).unwrap();
    assert_eq!(at_cap.output, full.output);
    // One char less and it flips to an index.
    ctx.output_cap = exact - 1;
    let over = tool.execute(json!({"name": "demo"}), &mut ctx).unwrap();
    assert!(over.output.starts_with("<skill_index"), "{}", over.output);
}

/// The other side of that fallback: when a skill IS over the cap but an
/// index of it would not fit either (all its prose precedes the first
/// heading), indexing is not an improvement, so the full body goes out and
/// is centrally truncated. Same ruling as a heading-less skill, because
/// that is effectively what this is.
#[test]
fn an_index_that_would_not_fit_falls_back_to_full() {
    let a = tempfile::tempdir().unwrap();
    let mut body = "intro prose that never ends. ".repeat(400);
    body.push_str("\n## Tiny\nx\n");
    write_skill(a.path(), "demo", FM, &body);
    let tool = tool_over(a.path());
    let mut ctx = ToolCtx::new(a.path().to_path_buf());
    ctx.output_cap = 2_000; // under the intro alone
    let out = tool.execute(json!({"name": "demo"}), &mut ctx).unwrap();
    assert!(out.output.starts_with("<skill_content"), "{}", &out.output[..80]);
}

#[test]
fn no_headings_over_cap_stays_full_and_is_centrally_truncated() {
    use temur::tools::Registry;
    let a = tempfile::tempdir().unwrap();
    write_skill(a.path(), "demo", FM, &"prose with no headings at all. ".repeat(2_000));
    let reg = Registry::with_tools(vec![Box::new(tool_over(a.path()))]);
    let mut ctx = ToolCtx::new(a.path().to_path_buf());
    let out = reg
        .execute("skill", json!({"name": "demo"}), &mut ctx)
        .unwrap();
    // No index is possible, so the central truncation does its job, but the
    // advice it gives is now the skill tool's own.
    assert!(out.output.contains("(output truncated:"), "{}", out.output);
    assert!(
        out.output.contains("call skill again with a \"section\" argument"),
        "{}",
        out.output
    );
    assert!(!out.output.contains("grep or head/tail"), "{}", out.output);
}

#[test]
fn an_oversized_single_section_is_truncated_with_the_skill_hint() {
    use temur::tools::Registry;
    let a = tempfile::tempdir().unwrap();
    let mut body = String::from("## Small\ns\n## Huge\n");
    body.push_str(&"one very long line of skill instructions. ".repeat(1_500));
    write_skill(a.path(), "demo", FM, &body);
    let mut reg = Registry::with_tools(vec![Box::new(tool_over(a.path()))]);
    reg.set_context_window(Some(4_000)); // cap 4000, well under that section
    let mut ctx = ToolCtx::new(a.path().to_path_buf());
    let out = reg
        .execute("skill", json!({"name": "demo", "section": "Huge"}), &mut ctx)
        .unwrap();
    assert!(out.output.contains("(output truncated:"), "{}", out.output);
    assert!(out.output.contains("call skill again with a \"section\""), "{}", out.output);
    // No recursive sub-index: what comes back is the section, cut centrally.
    assert!(!out.output.contains("<skill_index"), "{}", out.output);
}
