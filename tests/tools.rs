//! M3 tool tests — temp dirs on native tmpfs/ext4, run as i686 binaries.

use temur::tools::{PromptProfile, Registry, Tool, ToolCtx, ToolError, ToolOutput};
use serde_json::json;

fn ctx_in(dir: &std::path::Path) -> ToolCtx {
    ToolCtx::new(dir.to_path_buf())
}

// --- T4 prompt profiles ----------------------------------------------------

/// MUST-HOLD: the default registry serves byte-identical definitions to an
/// explicit Full profile — the default path is provably unchanged by T4.
#[test]
fn default_definitions_byte_equal_explicit_full_profile() {
    let default_defs = Registry::standard().definitions();
    let full_defs = Registry::standard()
        .with_profile(PromptProfile::Full)
        .definitions();
    assert_eq!(default_defs.len(), full_defs.len());
    for (d, f) in default_defs.iter().zip(full_defs.iter()) {
        assert_eq!(d.name, f.name);
        assert_eq!(d.description, f.description, "description differs for {}", d.name);
        assert_eq!(d.input_schema, f.input_schema, "schema differs for {}", d.name);
    }
}

#[test]
fn compact_profile_swaps_descriptions_only() {
    let full = Registry::standard().definitions();
    let compact = Registry::standard()
        .with_profile(PromptProfile::Compact)
        .definitions();
    // Tool set and ORDER untouched; schemas identical.
    assert_eq!(
        full.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        compact.iter().map(|d| d.name.as_str()).collect::<Vec<_>>()
    );
    for (f, c) in full.iter().zip(compact.iter()) {
        assert_eq!(f.input_schema, c.input_schema, "schema differs for {}", f.name);
    }
    let get = |defs: &[temur::provider::ToolDef], name: &str| {
        defs.iter().find(|d| d.name == name).unwrap().description.clone()
    };
    // Hand-trimmed prompts differ and honor their size caps.
    for (name, cap) in [("bash", 1000), ("todowrite", 700), ("edit", 700)] {
        let c = get(&compact, name);
        assert_ne!(c, get(&full, name), "{name} compact prompt must differ");
        assert!(
            c.chars().count() <= cap,
            "{name} compact prompt exceeds {cap} chars ({})",
            c.chars().count()
        );
    }
    // Tools without an override serve the full text unchanged.
    for name in ["read", "write", "glob", "grep", "todoread"] {
        assert_eq!(get(&compact, name), get(&full, name), "{name} must be unchanged");
    }
    // The point of the profile: total tool text within the small-context
    // budget (~24.4KB full today).
    let total: usize = compact.iter().map(|d| d.description.len()).sum();
    assert!(total <= 8 * 1024, "compact tool text is {total} bytes, budget 8KB");
}

/// T9: the in-place mutator serves byte-identical definitions to the
/// builder path, both directions — so a `/model` prompt swap is exactly the
/// startup profile selection, description-swap-only contract included.
#[test]
fn set_profile_matches_with_profile_both_directions() {
    for profile in [PromptProfile::Compact, PromptProfile::Full] {
        let mut mutated = Registry::standard();
        mutated.set_profile(profile);
        let built = Registry::standard().with_profile(profile).definitions();
        let mutated = mutated.definitions();
        assert_eq!(mutated.len(), built.len());
        for (m, b) in mutated.iter().zip(built.iter()) {
            assert_eq!(m.name, b.name);
            assert_eq!(m.description, b.description, "description differs for {}", m.name);
            assert_eq!(m.input_schema, b.input_schema, "schema differs for {}", m.name);
        }
    }
}

/// T34 interop pin: no tool schema anywhere in the registry may declare a
/// UNION type. JSON Schema allows `"type": ["string", "number"]`, but some
/// shipped chat templates stringify a schema by dict lookup on the "type"
/// value and cannot key on a list: llama.cpp re-renders the template on
/// every request when no specialized handler matches, so one union type in
/// one always-registered tool turns into HTTP 400 on every real turn. That
/// is exactly what the `skill` tool's "section" did until 2026-08-18
/// (archive: template-experiment-2026-08-17/E2/a1-hermes-root-cause.txt).
/// Tolerance for non-string spellings belongs at the argument boundary
/// (T33 coercion), never in the declared type.
///
/// Walks BOTH prompt profiles and every nested schema level, so a union
/// added to any tool, at any depth, fails here rather than in the field.
#[test]
fn no_tool_schema_declares_a_union_type() {
    fn walk(v: &serde_json::Value, tool: &str, path: &str) {
        match v {
            serde_json::Value::Object(map) => {
                if let Some(t) = map.get("type") {
                    assert!(
                        t.is_string(),
                        "{tool}: schema at {path}.type is {t}, not a plain string; \
                         a union type is unrenderable by templates that key on it"
                    );
                }
                for (k, child) in map {
                    walk(child, tool, &format!("{path}.{k}"));
                }
            }
            serde_json::Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    walk(child, tool, &format!("{path}[{i}]"));
                }
            }
            _ => {}
        }
    }
    for profile in [PromptProfile::Full, PromptProfile::Compact] {
        let reg = Registry::standard_with_skills(vec![std::path::PathBuf::from("/nonexistent")])
            .with_profile(profile);
        let defs = reg.definitions();
        // The tool this pin exists for must actually be in the set walked.
        assert!(defs.iter().any(|d| d.name == "skill"), "skill tool missing");
        for d in &defs {
            walk(&d.input_schema, &d.name, "");
        }
    }
}

/// The other half of the same contract: the schema says "string", and a
/// JSON number still selects a section. Pinned here beside the schema pin
/// so the two can never drift apart. (The behavior itself is exercised
/// end to end in tests/skills.rs.)
#[test]
fn skill_section_schema_is_a_string_and_the_execute_path_still_takes_numbers() {
    let defs = Registry::standard_with_skills(vec![]).definitions();
    let skill = defs.iter().find(|d| d.name == "skill").unwrap();
    assert_eq!(skill.input_schema["properties"]["section"]["type"], "string");

    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join("demo");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: demo\ndescription: d\n---\n# Top\nintro\n## Setup\nsetup body\n",
    )
    .unwrap();
    let reg = Registry::standard_with_skills(vec![dir.path().to_path_buf()]);
    let mut ctx = ctx_in(dir.path());
    let as_number = reg
        .execute("skill", json!({"name": "demo", "section": 2}), &mut ctx)
        .unwrap();
    let as_string = reg
        .execute("skill", json!({"name": "demo", "section": "2"}), &mut ctx)
        .unwrap();
    assert!(as_number.output.contains("setup body"), "{}", as_number.output);
    assert_eq!(as_number.output, as_string.output);
}

fn run(reg: &Registry, ctx: &mut ToolCtx, name: &str, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
    reg.execute(name, input, ctx)
}

#[test]
fn read_numbered_lines_offset_limit() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("f.txt");
    std::fs::write(&f, "alpha\nbeta\ngamma\n").unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());

    let out = run(&reg, &mut ctx, "read", json!({"filePath": f.to_str().unwrap()})).unwrap();
    assert!(out.output.contains("1: alpha"));
    assert!(out.output.contains("3: gamma"));
    assert!(out.output.contains("(End of file - total 3 lines)"));

    let out = run(&reg, &mut ctx, "read", json!({"filePath": f.to_str().unwrap(), "offset": 2, "limit": 1})).unwrap();
    assert!(out.output.contains("2: beta"));
    assert!(!out.output.contains("1: alpha"));
    assert!(out.output.contains("Use offset=3 to continue"));
}

#[test]
fn read_byte_cap_pagination_hint_survives_registry_truncation() {
    // A file well past every cap: read must stop at its own 28 KB rendered
    // cap and emit its pagination footer, and the whole output must fit
    // under the registry's 30,000-char central truncation so the
    // "Use offset=N to continue" hint reaches the model intact.
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("big.csv");
    let line = "v".repeat(99); // 100 bytes rendered incl. newline, plus "N: "
    let content: String = (0..1200).map(|_| format!("{line}\n")).collect(); // ~120 KB
    std::fs::write(&f, &content).unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());

    let out = run(&reg, &mut ctx, "read", json!({"filePath": f.to_str().unwrap()})).unwrap();
    assert!(
        out.output.contains("(Output capped at 28 KB."),
        "read's own cap message present"
    );
    assert!(
        out.output.contains("Use offset=") && out.output.contains("to continue"),
        "pagination hint present"
    );
    assert!(
        !out.output.contains("(output truncated:"),
        "registry truncation must NOT fire"
    );
    assert!(
        out.output.chars().count() < 30_000,
        "whole rendered output stays under the central cap ({} chars)",
        out.output.chars().count()
    );
    assert!(out.output.ends_with("</content>"), "footer intact");
}

#[test]
fn read_missing_binary_and_directory() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());

    let err = run(&reg, &mut ctx, "read", json!({"filePath": dir.path().join("nope.txt").to_str().unwrap()})).unwrap_err();
    assert!(err.to_string().contains("File not found"));

    let bin = dir.path().join("blob.dat");
    std::fs::write(&bin, [0u8, 159, 146, 150]).unwrap();
    let err = run(&reg, &mut ctx, "read", json!({"filePath": bin.to_str().unwrap()})).unwrap_err();
    assert!(err.to_string().contains("binary"));

    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/x.txt"), "x").unwrap();
    let out = run(&reg, &mut ctx, "read", json!({"filePath": dir.path().join("sub").to_str().unwrap()})).unwrap();
    assert!(out.output.contains("<type>directory</type>"));
    assert!(out.output.contains("x.txt"));
}

#[test]
fn write_creates_nested_paths() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let target = dir.path().join("a/b/c.txt");
    run(&reg, &mut ctx, "write", json!({"filePath": target.to_str().unwrap(), "content": "hello"})).unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
}

/// T30 (T29 queue finding 6, measured 2026-08-12): an overwrite that
/// destroyed content says how much. The guard is untouched; this is the
/// missing trace, since the model that did it read the file first and was
/// allowed through correctly.
#[test]
fn write_over_existing_content_reports_the_bytes_it_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());

    // The eval-task-5 shape: a 30-byte needle file overwritten with 8.
    let needle = dir.path().join("beta.txt");
    std::fs::write(&needle, "the needle lives on line two\n\n").unwrap();
    assert_eq!(std::fs::metadata(&needle).unwrap().len(), 30);
    run(&reg, &mut ctx, "read", json!({"filePath": needle.to_str().unwrap()})).unwrap();
    let out = run(
        &reg,
        &mut ctx,
        "write",
        json!({"filePath": needle.to_str().unwrap(), "content": "beta.txt"}),
    )
    .unwrap();
    assert!(
        out.output.contains("(8 bytes, replaced 30 bytes of prior content)"),
        "{}",
        out.output
    );

    // A new file destroys nothing, and says nothing.
    let fresh = dir.path().join("fresh.txt");
    let out = run(
        &reg,
        &mut ctx,
        "write",
        json!({"filePath": fresh.to_str().unwrap(), "content": "hello"}),
    )
    .unwrap();
    assert!(out.output.starts_with("Created "), "{}", out.output);
    assert!(out.output.contains("(5 bytes)"), "{}", out.output);
    assert!(!out.output.contains("replaced"), "{}", out.output);

    // Neither does replacing an EMPTY file: there was no prior content.
    let empty = dir.path().join("empty.txt");
    std::fs::write(&empty, "").unwrap();
    run(&reg, &mut ctx, "read", json!({"filePath": empty.to_str().unwrap()})).unwrap();
    let out = run(
        &reg,
        &mut ctx,
        "write",
        json!({"filePath": empty.to_str().unwrap(), "content": "now it has content"}),
    )
    .unwrap();
    assert!(out.output.starts_with("Overwrote "), "{}", out.output);
    assert!(!out.output.contains("replaced"), "{}", out.output);
}

// --------------------------------------------------------- T19 (P2)
// write's read-first rule: the prompt has always promised "this tool will
// fail if you did not read the file first"; now it does.

#[test]
fn write_unread_existing_file_fails() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let target = dir.path().join("existing.txt");
    std::fs::write(&target, "original").unwrap();
    let err = run(
        &reg,
        &mut ctx,
        "write",
        json!({"filePath": target.to_str().unwrap(), "content": "clobbered"}),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("has not been read in this session"),
        "{err}"
    );
    assert!(
        err.to_string().contains("use edit for targeted changes"),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
}

#[test]
fn write_after_read_succeeds_and_new_files_are_unaffected() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let target = dir.path().join("existing.txt");
    std::fs::write(&target, "original").unwrap();
    // Read arms the check for this exact file.
    run(&reg, &mut ctx, "read", json!({"filePath": target.to_str().unwrap()})).unwrap();
    run(
        &reg,
        &mut ctx,
        "write",
        json!({"filePath": target.to_str().unwrap(), "content": "updated"}),
    )
    .unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "updated");
    // A brand-new file needs no read.
    let fresh = dir.path().join("fresh.txt");
    run(
        &reg,
        &mut ctx,
        "write",
        json!({"filePath": fresh.to_str().unwrap(), "content": "new"}),
    )
    .unwrap();
    // And a successful write knows what it wrote: overwriting its own
    // output needs no re-read.
    run(
        &reg,
        &mut ctx,
        "write",
        json!({"filePath": fresh.to_str().unwrap(), "content": "new2"}),
    )
    .unwrap();
    assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "new2");
}

#[test]
fn write_read_first_agrees_across_path_spellings() {
    // Read via absolute path, write via relative: canonicalization makes
    // the spellings agree.
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let target = dir.path().join("same.txt");
    std::fs::write(&target, "v1").unwrap();
    run(&reg, &mut ctx, "read", json!({"filePath": target.to_str().unwrap()})).unwrap();
    run(&reg, &mut ctx, "write", json!({"filePath": "same.txt", "content": "v2"})).unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v2");
}

#[test]
fn edit_arms_write_and_works_standalone_on_unread_files() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let target = dir.path().join("code.rs");
    std::fs::write(&target, "fn main() {}").unwrap();
    // edit needs no prior read (it reads the file itself)...
    run(
        &reg,
        &mut ctx,
        "edit",
        json!({"filePath": target.to_str().unwrap(), "oldString": "main", "newString": "start"}),
    )
    .unwrap();
    // ...and having read it, it arms write's check.
    run(
        &reg,
        &mut ctx,
        "write",
        json!({"filePath": target.to_str().unwrap(), "content": "fn start() {}\n"}),
    )
    .unwrap();
}

#[test]
fn read_binary_denial_names_bash_inspection() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let refuse = |ctx: &mut _, name: &str| {
        let target = dir.path().join(name);
        std::fs::write(&target, b"\x1f\x8b\x08\x00binary\x00stuff").unwrap();
        let err = run(&reg, ctx, "read", json!({"filePath": target.to_str().unwrap()}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("Cannot read binary file"), "{err}");
        err
    };

    // An unknown binary type keeps the pre-T31 sentence, byte-identical.
    let err = refuse(&mut ctx, "blob.dat");
    assert!(
        err.ends_with("Inspect it with bash instead (e.g. file, unzip -l, strings)."),
        "{err}"
    );
    let err = refuse(&mut ctx, "noext");
    assert!(err.contains("Inspect it with bash instead"), "{err}");

    // T31 (D3): known types get a remedy they can actually run. Sending a
    // model toward `unzip -l` on a PDF is what this replaces.
    let err = refuse(&mut ctx, "paper.pdf");
    assert!(err.contains("pdftotext"), "{err}");
    assert!(!err.contains("unzip -l"), "{err}");
    let err = refuse(&mut ctx, "bundle.zip");
    assert!(err.contains("unzip -l"), "{err}");
    let err = refuse(&mut ctx, "blob.gz");
    assert!(err.contains("zcat"), "{err}");
    let err = refuse(&mut ctx, "shot.png");
    assert!(err.contains("cannot see images"), "{err}");
    assert!(!err.contains("strings"), "{err}");
}

#[test]
fn edit_unique_replace_all_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("e.txt");
    std::fs::write(&f, "foo bar foo").unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let fp = f.to_str().unwrap();

    // ambiguous without replaceAll
    let err = run(&reg, &mut ctx, "edit", json!({"filePath": fp, "oldString": "foo", "newString": "baz"})).unwrap_err();
    assert!(err.to_string().contains("2 times"));

    // replaceAll
    run(&reg, &mut ctx, "edit", json!({"filePath": fp, "oldString": "foo", "newString": "baz", "replaceAll": true})).unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "baz bar baz");

    // unique
    run(&reg, &mut ctx, "edit", json!({"filePath": fp, "oldString": "bar", "newString": "qux"})).unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "baz qux baz");

    // not found
    let err = run(&reg, &mut ctx, "edit", json!({"filePath": fp, "oldString": "zzz", "newString": "y"})).unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[test]
fn bash_output_exit_code_and_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());

    let out = run(&reg, &mut ctx, "bash", json!({"command": "echo hi; echo err >&2"})).unwrap();
    assert!(out.output.contains("hi"));
    assert!(out.output.contains("err"));

    let out = run(&reg, &mut ctx, "bash", json!({"command": "exit 3"})).unwrap();
    assert!(out.output.contains("(exit code 3)"));

    let out = run(&reg, &mut ctx, "bash", json!({"command": "sleep 5", "timeout": 200})).unwrap();
    assert!(out.output.contains("timed out"));

    // workdir respected
    let out = run(&reg, &mut ctx, "bash", json!({"command": "pwd", "workdir": dir.path().to_str().unwrap()})).unwrap();
    assert!(out.output.contains(dir.path().file_name().unwrap().to_str().unwrap()));
}

/// T31 (H2, operator dogfood 2026-08-14, eval task 6): the model filled the
/// optional workdir in with "", which reached the spawn verbatim and failed
/// with "No such file or directory (os error 2)"; it then parroted that
/// error into its next call's arguments. Empty means absent.
#[test]
fn bash_empty_workdir_falls_back_to_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let leaf = dir.path().file_name().unwrap().to_str().unwrap();

    for empty in ["", "   ", "\t\n"] {
        let out = run(
            &reg,
            &mut ctx,
            "bash",
            json!({"command": "pwd", "workdir": empty}),
        )
        .unwrap_or_else(|e| panic!("workdir {empty:?} must not fail the spawn: {e}"));
        assert!(
            !out.output.contains("failed to spawn shell"),
            "workdir {empty:?}: {}",
            out.output
        );
        assert!(out.output.contains(leaf), "workdir {empty:?}: {}", out.output);
    }

    // A real workdir is still honored, and a bogus one still fails loudly
    // rather than being silently swallowed by the new fallback.
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let out = run(
        &reg,
        &mut ctx,
        "bash",
        json!({"command": "pwd", "workdir": sub.to_str().unwrap()}),
    )
    .unwrap();
    assert!(out.output.contains("sub"), "{}", out.output);
    let bogus = run(
        &reg,
        &mut ctx,
        "bash",
        json!({"command": "pwd", "workdir": "/no/such/dir/anywhere"}),
    );
    assert!(bogus.is_err(), "a named but missing workdir must still error");
}

#[test]
fn glob_matches_and_sorts() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "x").unwrap();
    std::fs::write(dir.path().join("b.txt"), "x").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/c.rs"), "x").unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());

    let out = run(&reg, &mut ctx, "glob", json!({"pattern": "**/*.rs"})).unwrap();
    assert!(out.output.contains("a.rs"));
    assert!(out.output.contains("c.rs"));
    assert!(!out.output.contains("b.txt"));

    let out = run(&reg, &mut ctx, "glob", json!({"pattern": "*.nope"})).unwrap();
    assert_eq!(out.output, "No files found");
}

#[test]
fn grep_regex_include_and_binary_skip() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn main() {}\nlet x = 42;\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "main street\n").unwrap();
    std::fs::write(dir.path().join("bin.dat"), [0u8, 1, 2]).unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());

    let out = run(&reg, &mut ctx, "grep", json!({"pattern": "ma.n"})).unwrap();
    assert!(out.output.contains("a.rs:1:"));
    assert!(out.output.contains("b.txt:1:"));

    let out = run(&reg, &mut ctx, "grep", json!({"pattern": "main", "include": "*.rs"})).unwrap();
    assert!(out.output.contains("a.rs"));
    assert!(!out.output.contains("b.txt"));

    let err = run(&reg, &mut ctx, "grep", json!({"pattern": "("})).unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)));
}

#[test]
fn todo_write_then_read_via_ctx() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    run(&reg, &mut ctx, "todowrite", json!({"todos": [
        {"content": "port tools", "status": "completed"},
        {"content": "agent loop", "status": "pending"}
    ]})).unwrap();
    assert_eq!(ctx.todos.len(), 2);
    let out = run(&reg, &mut ctx, "todoread", json!({})).unwrap();
    assert!(out.output.contains("agent loop"));
    assert_eq!(out.title, "1 todos");
}

#[test]
fn registry_unknown_tool_and_bad_input() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let err = run(&reg, &mut ctx, "teleport", json!({})).unwrap_err();
    assert!(err.to_string().contains("unknown tool"));
    let err = run(&reg, &mut ctx, "read", json!({"filepath": "wrong-case"})).unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)));
}

#[test]
fn registry_truncates_oversized_output() {
    struct BigTool;
    impl Tool for BigTool {
        fn name(&self) -> &'static str { "big" }
        fn description(&self) -> &'static str { "emits a lot" }
        fn input_schema(&self) -> serde_json::Value { json!({"type":"object","properties":{}}) }
        fn execute(&self, _i: serde_json::Value, _c: &mut ToolCtx) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput { title: "big".into(), output: "x".repeat(40_000) })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::with_tools(vec![Box::new(BigTool)]);
    let mut ctx = ctx_in(dir.path());
    let out = run(&reg, &mut ctx, "big", json!({})).unwrap();
    // No context_window configured: the cap is 30,000 exactly as pre-T19,
    // now kept as a true head + true tail around the T19 marker.
    assert!(out.output.contains(
        "(output truncated: showing the first 15000 and last 15000 of 40000 chars; \
         narrow the command, e.g. grep or head/tail, to see the elided middle)"
    ), "{}", out.output);
    assert!(out.output.len() < 40_000);
}

// --------------------------------------------------------------- T19 (P1)

/// A tool whose output makes head and tail distinguishable: 'a' x 10_000,
/// then 'b' x 10_000, then 'c' x 10_000.
struct AbcTool;
impl Tool for AbcTool {
    fn name(&self) -> &'static str { "abc" }
    fn description(&self) -> &'static str { "emits abc bands" }
    fn input_schema(&self) -> serde_json::Value { json!({"type":"object","properties":{}}) }
    fn execute(&self, _i: serde_json::Value, _c: &mut ToolCtx) -> Result<ToolOutput, ToolError> {
        let mut s = "a".repeat(10_000);
        s.push_str(&"b".repeat(10_000));
        s.push_str(&"c".repeat(10_000));
        Ok(ToolOutput { title: "abc".into(), output: s })
    }
}

#[test]
fn context_scaled_cap_keeps_true_head_and_true_tail() {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = Registry::with_tools(vec![Box::new(AbcTool)]);
    reg.set_context_window(Some(8_000)); // cap 8000: head 4000, tail 4000
    let mut ctx = ctx_in(dir.path());
    let out = run(&reg, &mut ctx, "abc", json!({})).unwrap();
    let marker = "\n\n(output truncated: showing the first 4000 and last 4000 of 30000 chars; \
                  narrow the command, e.g. grep or head/tail, to see the elided middle)\n\n";
    // Exact shape: true head, one marker line, true tail, nothing else.
    assert_eq!(out.output, format!("{}{marker}{}", "a".repeat(4_000), "c".repeat(4_000)));
}

#[test]
fn context_scaled_cap_odd_split_arithmetic_is_exact() {
    // Odd cap: head = cap/2, tail = cap - head, and the marker states both.
    let dir = tempfile::tempdir().unwrap();
    let mut reg = Registry::with_tools(vec![Box::new(AbcTool)]);
    reg.set_context_window(Some(4_001)); // head 2000, tail 2001
    let mut ctx = ctx_in(dir.path());
    let out = run(&reg, &mut ctx, "abc", json!({})).unwrap();
    assert!(out.output.contains("showing the first 2000 and last 2001 of 30000 chars"));
    assert!(out.output.starts_with(&"a".repeat(2_000)));
    assert!(out.output.ends_with(&"c".repeat(2_001)));
    assert!(!out.output.starts_with(&"a".repeat(2_001)), "head must be exactly 2000");
}

#[test]
fn context_window_clamp_floor_and_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    // Floor: a 1000-token window still gets a 4000-char cap.
    let mut reg = Registry::with_tools(vec![Box::new(AbcTool)]);
    reg.set_context_window(Some(1_000));
    let mut ctx = ctx_in(dir.path());
    let out = run(&reg, &mut ctx, "abc", json!({})).unwrap();
    assert!(out.output.contains("showing the first 2000 and last 2000 of 30000 chars"));
    // Ceiling: a huge window never raises the cap above 30,000; under-cap
    // output passes through untouched.
    let mut reg = Registry::with_tools(vec![Box::new(AbcTool)]);
    reg.set_context_window(Some(1_000_000));
    let out = run(&reg, &mut ctx, "abc", json!({})).unwrap();
    assert!(!out.output.contains("(output truncated:"), "30000 chars fit a 30000 cap");
    assert_eq!(out.output.chars().count(), 30_000);
}

#[test]
fn no_window_output_at_cap_passes_untouched() {
    // Exactly-at-cap output is not truncated (strictly-greater rule).
    struct AtCap;
    impl Tool for AtCap {
        fn name(&self) -> &'static str { "atcap" }
        fn description(&self) -> &'static str { "emits exactly 30000" }
        fn input_schema(&self) -> serde_json::Value { json!({"type":"object","properties":{}}) }
        fn execute(&self, _i: serde_json::Value, _c: &mut ToolCtx) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput { title: "atcap".into(), output: "z".repeat(30_000) })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::with_tools(vec![Box::new(AtCap)]);
    let mut ctx = ctx_in(dir.path());
    let out = run(&reg, &mut ctx, "atcap", json!({})).unwrap();
    assert_eq!(out.output, "z".repeat(30_000));
}

#[test]
fn definitions_are_complete_and_ordered() {
    let reg = Registry::standard();
    let defs = reg.definitions();
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["read", "write", "edit", "bash", "glob", "grep", "todowrite", "todoread"]
    );
    for d in &defs {
        assert!(!d.description.is_empty(), "{} has empty prompt", d.name);
        assert_eq!(d.input_schema["type"], "object");
    }
}

// --------------------------------------------------------------- T6 (I3)

/// Esc reaches a running bash: token set at ~100 ms kills a 30 s sleep and
/// the result is an error carrying the interruption marker.
#[test]
fn bash_interrupted_by_cancel_token_returns_fast() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());

    let token = ctx.cancel.clone();
    let setter = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        token.set();
    });

    let start = std::time::Instant::now();
    let err = run(&reg, &mut ctx, "bash", json!({"command": "sleep 30"})).unwrap_err();
    setter.join().unwrap();

    assert!(
        start.elapsed() < std::time::Duration::from_secs(2),
        "interrupt must land within one poll slice (took {:?})",
        start.elapsed()
    );
    assert!(
        err.to_string().contains("(interrupted by user)"),
        "marker missing: {err}"
    );
}

/// A token already set when bash starts aborts before any real waiting.
#[test]
fn bash_with_preset_token_aborts_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    ctx.cancel.set();

    let start = std::time::Instant::now();
    let err = run(&reg, &mut ctx, "bash", json!({"command": "sleep 30"})).unwrap_err();
    assert!(
        start.elapsed() < std::time::Duration::from_secs(1),
        "pre-set token must abort at once (took {:?})",
        start.elapsed()
    );
    assert!(err.to_string().contains("(interrupted by user)"));
}

// ----------------------------------------------------------- T6 (E2): fuzzy

/// MUST-HOLD pin: when an exact match exists, the fuzzy pipeline is never
/// consulted and the output is byte-identical to v1 (no matcher marker).
#[test]
fn edit_exact_path_output_is_byte_identical_to_v1() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("e.txt");
    std::fs::write(&f, "a foo b").unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());

    let out = run(&reg, &mut ctx, "edit", json!({
        "filePath": f.to_str().unwrap(), "oldString": "foo", "newString": "bar"
    }))
    .unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "a bar b");
    assert_eq!(
        out.output,
        format!("Edited {} (1 replacement(s))", f.display()),
        "the exact path must not grow a marker"
    );
}

#[test]
fn edit_fuzzy_fallback_matrix() {
    struct Case {
        name: &'static str,
        initial: &'static str,
        old: &'static str,
        new: &'static str,
        replace_all: bool,
        // Ok: (final file content, output must contain). Err: message must
        // contain — and the file must be untouched.
        expect: Result<(&'static str, &'static str), &'static str>,
    }
    let cases = [
        Case {
            // F3: the file's indentation style (tab) survives the splice —
            // the model's spaces are swapped for the matched line's tab.
            name: "line_edge_whitespace_forgiven_file_indent_preserved",
            initial: "fn main() {\n\tlet x = 1;\n}\n",
            old: "    let x = 1;",
            new: "    let y = 2;",
            replace_all: false,
            expect: Ok(("fn main() {\n\tlet y = 2;\n}\n", "whitespace-tolerant match")),
        },
        Case {
            name: "interior_tab_vs_space_stays_not_found",
            initial: "x\nfoo\tbar\ny\n",
            old: "foo bar",
            new: "z",
            replace_all: false,
            expect: Err("not found in the file, even with whitespace-tolerant"),
        },
        Case {
            name: "crlf_file_lf_old_new_converted_rest_untouched",
            initial: "a\r\nfoo\r\nb\r\n",
            old: " foo",
            new: "bar",
            replace_all: false,
            expect: Ok(("a\r\nbar\r\nb\r\n", "whitespace-tolerant match")),
        },
        Case {
            name: "crlf_multiline_new_string_converted",
            initial: "a\r\nfoo\r\nb\r\n",
            old: "  foo",
            new: "x\ny",
            replace_all: false,
            expect: Ok(("a\r\nx\r\ny\r\nb\r\n", "whitespace-tolerant match")),
        },
        Case {
            // Uniform two-space delta across both lines (F3-compatible);
            // still exercises the no-doubled-newline splice.
            name: "trailing_newline_old_no_doubled_newline",
            initial: "x\na\nb\nc\n",
            old: "  a\n  b\n",
            new: "Q\n",
            replace_all: false,
            expect: Ok(("x\nQ\nc\n", "whitespace-tolerant match")),
        },
        Case {
            name: "eof_without_trailing_newline",
            initial: "a\nfoo",
            old: "  foo",
            new: "bar",
            replace_all: false,
            expect: Ok(("a\nbar", "whitespace-tolerant match")),
        },
        Case {
            name: "file_trailing_newline_preserved",
            initial: "a\nfoo\n",
            old: "  foo",
            new: "bar",
            replace_all: false,
            expect: Ok(("a\nbar\n", "whitespace-tolerant match")),
        },
        Case {
            name: "match_at_file_start",
            initial: "a\nb",
            old: "  a",
            new: "A",
            replace_all: false,
            expect: Ok(("A\nb", "whitespace-tolerant match")),
        },
        Case {
            name: "unicode_content_correct_splice",
            initial: "α\n\tβγ\nδ\n",
            old: " βγ",
            new: "χ",
            replace_all: false,
            expect: Ok(("α\nχ\nδ\n", "whitespace-tolerant match")),
        },
        Case {
            name: "exact_twice_keeps_v1_error_fuzzy_not_consulted",
            initial: "foo foo",
            old: "foo",
            new: "b",
            replace_all: false,
            expect: Err("appears 2 times"),
        },
        Case {
            name: "replace_all_with_fuzzy_only_match_errors",
            initial: "a\n\tfoo\n",
            old: "  foo",
            new: "b",
            replace_all: true,
            expect: Err("replaceAll requires an exact match"),
        },
        Case {
            name: "fuzzy_ambiguous_demands_more_context",
            initial: "a\nx\na\n",
            old: " a",
            new: "b",
            replace_all: false,
            expect: Err("matched 2 locations approximately"),
        },
        Case {
            name: "two_line_old_skips_block_anchor",
            initial: "start X\nend Y\n",
            old: "start X mangled\nend Y",
            new: "z",
            replace_all: false,
            expect: Err("not found in the file, even with whitespace-tolerant"),
        },
        Case {
            name: "block_anchor_mangled_middle_accepted_and_marked",
            initial: "fn f() {\n  actual_body();\n}\n",
            old: "fn f() {\n  imagined_body();\n}",
            new: "fn f() {\n  new_body();\n}",
            replace_all: false,
            expect: Ok((
                "fn f() {\n  new_body();\n}\n",
                "block-anchor match — oldString differed from the file; re-read",
            )),
        },
        Case {
            // F1: length tolerance now requires the middle-similarity
            // guard — here m1 appears in the candidate middle (1/1).
            name: "block_anchor_actual_block_longer_than_search",
            initial: "s\nm1\nm2\ne\n",
            old: "s\nm1\ne",
            new: "R",
            replace_all: false,
            expect: Ok(("R\n", "block-anchor match")),
        },
        Case {
            // F1: shorter actual block, half the search middle present.
            name: "block_anchor_actual_block_shorter_than_search",
            initial: "s\nm\ne\n",
            old: "s\nm\nx\ne",
            new: "R",
            replace_all: false,
            expect: Ok(("R\n", "block-anchor match")),
        },
        Case {
            // F1 regression (review scenario: nearest-anchor short splice).
            // A dissimilar middle with a length mismatch used to splice
            // away real code; it now refuses.
            name: "block_anchor_dissimilar_middle_refuses",
            initial: "s\nm1\nm2\ne\n",
            old: "s\nzz\ne",
            new: "R",
            replace_all: false,
            expect: Err("not found in the file, even with whitespace-tolerant"),
        },
        Case {
            // F1 regression (review scenario: inner-brace bind). The
            // nearest `}` is the if's; binding there deleted tail() and
            // reported success. Refusal, file untouched.
            name: "block_anchor_inner_brace_refuses",
            initial: "fn a() {\n    if x {\n        inner();\n    }\n    tail();\n}\n",
            old: "fn a() {\n    body();\n}",
            new: "fn a() {\n    new_body();\n}",
            replace_all: false,
            expect: Err("not found in the file, even with whitespace-tolerant"),
        },
        Case {
            // F3 regression (review scenario: nested Python, model wrote
            // the block one level shallower). The uniform +4 delta is
            // re-applied to newString: the file stays 8-space based.
            name: "indent_delta_nested_python_reindented",
            initial: "def f():\n        if cond:\n            do_a()\n        tail()\n",
            old: "    if cond:\n        do_a()",
            new: "    if cond:\n        do_b()\n        do_c()",
            replace_all: false,
            expect: Ok((
                "def f():\n        if cond:\n            do_b()\n            do_c()\n        tail()\n",
                "whitespace-tolerant match",
            )),
        },
        Case {
            // F3: tab-delta — the file's leading tab is re-applied.
            name: "indent_delta_tab_added",
            initial: "\tif x {\n\t\tgo();\n\t}\n",
            old: "if x {\n\tgo();\n}",
            new: "if x {\n\tstop();\n}",
            replace_all: false,
            expect: Ok(("\tif x {\n\t\tstop();\n\t}\n", "whitespace-tolerant match")),
        },
        Case {
            // F3: removal delta — the model over-indented; the extra two
            // spaces are stripped from newString.
            name: "indent_delta_spaces_removed",
            initial: "a()\nb()\nrest\n",
            old: "  a()\n  b()",
            new: "  c()\n  d()",
            replace_all: false,
            expect: Ok(("c()\nd()\nrest\n", "whitespace-tolerant match")),
        },
        Case {
            // F3: inconsistent per-line delta (one line +1 space, the
            // other -1) — no uniform rule exists, so the candidate is
            // rejected rather than spliced with guessed indentation.
            name: "indent_delta_inconsistent_refuses",
            initial: "  aa\nbb\n",
            old: " aa\n bb",
            new: "x",
            replace_all: false,
            expect: Err("not found in the file, even with whitespace-tolerant"),
        },
        Case {
            // F3 + CRLF: the delta is applied to the LF-shaped newString
            // first, then the whole replacement is CRLF-converted.
            name: "indent_delta_with_crlf_conversion",
            initial: "a\r\n    foo\r\nb\r\n",
            // Trailing space defeats the exact-substring path; the leading
            // delta is computed from the matched line ("" -> four spaces).
            old: "foo ",
            new: "bar\nbaz",
            replace_all: false,
            expect: Ok((
                "a\r\n    bar\r\n    baz\r\nb\r\n",
                "whitespace-tolerant match",
            )),
        },
        Case {
            name: "same_anchor_pair_twice_is_ambiguous",
            initial: "s\nm\ne\ns\nz\ne\n",
            old: "s\nq\ne",
            new: "R",
            replace_all: false,
            expect: Err("matched 2 locations approximately"),
        },
        Case {
            name: "old_with_more_lines_than_file_no_panic",
            initial: "a\nb",
            old: "a\nb\nc\nd\ne",
            new: "z",
            replace_all: false,
            expect: Err("not found in the file, even with whitespace-tolerant"),
        },
    ];

    let reg = Registry::standard();
    for c in &cases {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("t.txt");
        std::fs::write(&f, c.initial).unwrap();
        let mut ctx = ctx_in(dir.path());
        let res = run(&reg, &mut ctx, "edit", json!({
            "filePath": f.to_str().unwrap(),
            "oldString": c.old,
            "newString": c.new,
            "replaceAll": c.replace_all,
        }));
        match (&c.expect, res) {
            (Ok((want, marker)), Ok(out)) => {
                assert_eq!(
                    std::fs::read_to_string(&f).unwrap(),
                    *want,
                    "final content mismatch in {}",
                    c.name
                );
                assert!(
                    out.output.contains(marker),
                    "{}: output {:?} missing {marker:?}",
                    c.name,
                    out.output
                );
            }
            (Err(want), Err(e)) => {
                assert!(
                    e.to_string().contains(want),
                    "{}: error {:?} missing {want:?}",
                    c.name,
                    e.to_string()
                );
                assert_eq!(
                    std::fs::read_to_string(&f).unwrap(),
                    c.initial,
                    "{}: file must be untouched on error",
                    c.name
                );
            }
            (want, got) => panic!(
                "{}: expectation mismatch (want {:?}) got Ok={}",
                c.name,
                want.as_ref().map(|(w, m)| (w, m)),
                got.is_ok()
            ),
        }
    }
}

/// Invalid inputs stay invalid (unchanged from v1) and touch nothing.
#[test]
fn edit_invalid_inputs_unchanged_by_fuzzy() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("t.txt");
    std::fs::write(&f, "content\n").unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let fp = f.to_str().unwrap();

    let err = run(&reg, &mut ctx, "edit", json!({
        "filePath": fp, "oldString": "", "newString": "x"
    }))
    .unwrap_err();
    assert!(err.to_string().contains("must not be empty"));

    let err = run(&reg, &mut ctx, "edit", json!({
        "filePath": fp, "oldString": "same", "newString": "same"
    }))
    .unwrap_err();
    assert!(err.to_string().contains("must be different"));
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "content\n");
}

/// A fuzzy edit is not re-appliable: once applied, the same oldString no
/// longer matches anything — no silent double apply.
#[test]
fn edit_fuzzy_is_not_idempotently_reapplied() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("t.txt");
    std::fs::write(&f, "fn main() {\n\tlet x = 1;\n}\n").unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let input = json!({
        "filePath": f.to_str().unwrap(),
        "oldString": "    let x = 1;",
        "newString": "    let y = 2;"
    });

    run(&reg, &mut ctx, "edit", input.clone()).unwrap();
    let after_first = std::fs::read_to_string(&f).unwrap();
    let err = run(&reg, &mut ctx, "edit", input).unwrap_err();
    assert!(err.to_string().contains("not found"));
    assert_eq!(std::fs::read_to_string(&f).unwrap(), after_first);
}

// --- T18 P1: key-file guard (read/write/edit) --------------------------------
//
// HARD RULE: every test key is a placeholder string created by the test
// itself; no real key material is ever touched.

/// A tempdir with a secrets dir holding one placeholder key, a normal file
/// beside it, and a ToolCtx whose guard protects the key.
fn guarded_ctx() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf, ToolCtx) {
    let dir = tempfile::tempdir().unwrap();
    let secrets = dir.path().join("secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    let key = secrets.join("api.key");
    std::fs::write(&key, "placeholder-not-a-real-key\n").unwrap();
    let normal = dir.path().join("normal.txt");
    std::fs::write(&normal, "ordinary content\n").unwrap();
    let mut ctx = ctx_in(dir.path());
    ctx.guard = temur::tools::KeyGuard::from_paths(vec![key.clone()]);
    (dir, key, normal, ctx)
}

fn assert_denied(err: ToolError, what: &str) {
    let msg = err.to_string();
    assert!(msg.contains("key isolation"), "{what}: {msg}");
    assert!(
        !msg.contains("placeholder-not-a-real-key"),
        "{what}: denial must carry no key material: {msg}"
    );
}

#[test]
fn guard_read_denies_key_by_direct_path_symlink_and_hardlink() {
    let (dir, key, normal, mut ctx) = guarded_ctx();
    let reg = Registry::standard();

    let err = run(&reg, &mut ctx, "read", json!({"filePath": key.to_str().unwrap()})).unwrap_err();
    assert_denied(err, "direct read");

    let link = dir.path().join("innocent.txt");
    std::os::unix::fs::symlink(&key, &link).unwrap();
    let err = run(&reg, &mut ctx, "read", json!({"filePath": link.to_str().unwrap()})).unwrap_err();
    assert_denied(err, "symlink read");

    let hard = dir.path().join("hard.txt");
    std::fs::hard_link(&key, &hard).unwrap();
    let err = run(&reg, &mut ctx, "read", json!({"filePath": hard.to_str().unwrap()})).unwrap_err();
    assert_denied(err, "hardlink read");

    // The rest of the world still reads fine through the same ctx.
    let out = run(&reg, &mut ctx, "read", json!({"filePath": normal.to_str().unwrap()})).unwrap();
    assert!(out.output.contains("ordinary content"));
}

#[test]
fn guard_denies_everything_under_the_secrets_dir() {
    let (_dir, key, _normal, mut ctx) = guarded_ctx();
    let reg = Registry::standard();
    let sibling = key.parent().unwrap().join("sibling.key");
    std::fs::write(&sibling, "placeholder-not-a-real-key-2\n").unwrap();

    let err =
        run(&reg, &mut ctx, "read", json!({"filePath": sibling.to_str().unwrap()})).unwrap_err();
    assert_denied(err, "sibling read");
    // Reading the DIRECTORY (listing mode) is denied too.
    let err = run(
        &reg,
        &mut ctx,
        "read",
        json!({"filePath": key.parent().unwrap().to_str().unwrap()}),
    )
    .unwrap_err();
    assert_denied(err, "secrets dir listing");
}

#[test]
fn guard_write_denies_overwrite_and_create_under_secrets_dir() {
    let (_dir, key, _normal, mut ctx) = guarded_ctx();
    let reg = Registry::standard();

    let err = run(
        &reg,
        &mut ctx,
        "write",
        json!({"filePath": key.to_str().unwrap(), "content": "clobbered"}),
    )
    .unwrap_err();
    assert_denied(err, "key overwrite");
    assert_eq!(
        std::fs::read_to_string(&key).unwrap(),
        "placeholder-not-a-real-key\n",
        "the key file must be untouched"
    );

    // A CREATE under the secrets dir is denied before create_dir_all runs.
    let target = key.parent().unwrap().join("planted/evil.txt");
    let err = run(
        &reg,
        &mut ctx,
        "write",
        json!({"filePath": target.to_str().unwrap(), "content": "x"}),
    )
    .unwrap_err();
    assert_denied(err, "create under secrets dir");
    assert!(!target.parent().unwrap().exists(), "nothing may be created");
}

#[test]
fn guard_edit_denies_key_file() {
    let (_dir, key, _normal, mut ctx) = guarded_ctx();
    let reg = Registry::standard();
    let err = run(
        &reg,
        &mut ctx,
        "edit",
        json!({
            "filePath": key.to_str().unwrap(),
            "oldString": "placeholder",
            "newString": "poisoned"
        }),
    )
    .unwrap_err();
    assert_denied(err, "edit");
    assert_eq!(
        std::fs::read_to_string(&key).unwrap(),
        "placeholder-not-a-real-key\n"
    );
}

#[test]
fn guard_keyless_ctx_reads_the_same_files_freely() {
    // The SAME layout with the default (empty) guard: everything works,
    // proving keyless behavior is untouched by T18.
    let dir = tempfile::tempdir().unwrap();
    let secrets = dir.path().join("secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    let key = secrets.join("api.key");
    std::fs::write(&key, "placeholder-not-a-real-key\n").unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let out = run(&reg, &mut ctx, "read", json!({"filePath": key.to_str().unwrap()})).unwrap();
    assert!(out.output.contains("placeholder-not-a-real-key"));
    run(
        &reg,
        &mut ctx,
        "write",
        json!({"filePath": secrets.join("new.txt").to_str().unwrap(), "content": "ok"}),
    )
    .unwrap();
}

// --- T18 P2: key-file guard (grep/glob walks) --------------------------------

#[test]
fn guard_grep_never_reads_or_names_the_key_file() {
    let (dir, key, _normal, mut ctx) = guarded_ctx();
    let reg = Registry::standard();
    // The key content exists ONLY in the key file: a match would be a leak.
    let out = run(&reg, &mut ctx, "grep", json!({"pattern": "placeholder-not-a-real"})).unwrap();
    assert_eq!(out.output, "No matches found", "{}", out.output);

    // Ordinary content is still found, and the key file's PATH never
    // appears even when its lines would match a broad pattern.
    std::fs::write(dir.path().join("code.txt"), "ordinary needle here\n").unwrap();
    let out = run(&reg, &mut ctx, "grep", json!({"pattern": "needle"})).unwrap();
    assert!(out.output.contains("code.txt"), "{}", out.output);
    let out = run(&reg, &mut ctx, "grep", json!({"pattern": "."})).unwrap();
    assert!(
        !out.output.contains(key.to_str().unwrap()),
        "key path must never appear: {}",
        out.output
    );
    assert!(!out.output.contains("placeholder-not-a-real"), "{}", out.output);
}

#[test]
fn guard_glob_never_lists_key_or_secrets_dir_contents() {
    let (dir, key, _normal, mut ctx) = guarded_ctx();
    let reg = Registry::standard();
    let sibling = key.parent().unwrap().join("sibling.pem");
    std::fs::write(&sibling, "placeholder-not-a-real-key-2\n").unwrap();

    let out = run(&reg, &mut ctx, "glob", json!({"pattern": "**/*"})).unwrap();
    assert!(out.output.contains("normal.txt"), "{}", out.output);
    assert!(!out.output.contains("api.key"), "{}", out.output);
    assert!(!out.output.contains("sibling.pem"), "{}", out.output);

    // Aiming the walk INTO the secrets dir still lists nothing.
    let out = run(
        &reg,
        &mut ctx,
        "glob",
        json!({"pattern": "*", "path": key.parent().unwrap().to_str().unwrap()}),
    )
    .unwrap();
    assert_eq!(out.output, "No files found", "{}", out.output);

    let out = run(&reg, &mut ctx, "grep", json!({"pattern": "hello", "path": dir.path().to_str().unwrap()})).unwrap();
    assert_eq!(out.output, "No matches found");
}

#[test]
fn guard_grep_glob_walk_scale_sanity_and_keyless_unchanged() {
    // Walk-scale: a couple hundred files under a guarded ctx complete fine
    // (identities are stat'ed once per execute; see the guard unit test
    // snapshot_freezes_identities_once for the freeze proof).
    let (dir, key, _normal, mut ctx) = guarded_ctx();
    let reg = Registry::standard();
    for i in 0..200 {
        std::fs::write(dir.path().join(format!("f{i}.txt")), "bulk needle\n").unwrap();
    }
    let out = run(&reg, &mut ctx, "grep", json!({"pattern": "bulk needle"})).unwrap();
    assert!(out.output.contains("(Showing first 100 matches)"), "{}", out.output);
    assert!(!out.output.contains("api.key"), "{}", out.output);

    // Keyless ctx over the same tree: the key file IS found, as before T18.
    let mut plain = ctx_in(dir.path());
    let out = run(&reg, &mut plain, "grep", json!({"pattern": "placeholder-not-a-real"})).unwrap();
    assert!(out.output.contains("api.key"), "{}", out.output);
    let out = run(&reg, &mut plain, "glob", json!({"pattern": "**/*.key"})).unwrap();
    assert!(out.output.contains(key.to_str().unwrap()), "{}", out.output);
}

// --- T18 P3: bash key sandbox ------------------------------------------------
//
// Environment note: these tests assert whichever arm the environment
// makes real. On hosts with unprivileged user namespaces (WSL2, most
// desktop kernels) that is the sandboxed arm. In a container it depends on
// the runtime's seccomp policy: this project's rootless podman + crun
// PERMITS nested unshare(CLONE_NEWUSER), so the sandboxed arm runs
// in-container here too; a locked-down runtime would flip these to the
// refusal arm instead. The refusal decision itself is covered
// deterministically by the injected-probe unit tests in bash.rs, so no
// arm depends on luck to be exercised.

#[test]
fn guard_bash_sandboxed_masks_key_or_refuses() {
    let (dir, key, _normal, mut ctx) = guarded_ctx();
    let reg = Registry::standard();
    let res = run(
        &reg,
        &mut ctx,
        "bash",
        json!({"command": format!("cat {}", key.display())}),
    );
    if temur::tools::sandbox_available() {
        // Sandboxed: the key path reads as /dev/null, so cat sees nothing.
        let out = res.unwrap();
        assert!(
            !out.output.contains("placeholder-not-a-real-key"),
            "key content must never appear: {}",
            out.output
        );
        assert!(out.output.contains("(no output)"), "{}", out.output);

        // A write to the key path inside the sandbox is discarded: the
        // real file on the host is untouched.
        run(
            &reg,
            &mut ctx,
            "bash",
            json!({"command": format!("echo poisoned > {}", key.display())}),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&key).unwrap(),
            "placeholder-not-a-real-key\n",
            "host key file must be untouched by a sandboxed write"
        );

        // Everything else works inside the sandbox.
        let out = run(
            &reg,
            &mut ctx,
            "bash",
            json!({"command": format!("cat {}/normal.txt && echo sandbox-alive", dir.path().display())}),
        )
        .unwrap();
        assert!(out.output.contains("ordinary content"), "{}", out.output);
        assert!(out.output.contains("sandbox-alive"), "{}", out.output);
    } else {
        // No sandbox on this host: with keys configured and no override,
        // bash must refuse with the canonical message.
        let err = res.unwrap_err().to_string();
        assert_eq!(err, temur::tools::SANDBOX_REFUSAL);
    }
}

#[test]
fn guard_bash_override_never_refuses() {
    // allow_bash_without_key_sandbox: with a working sandbox it still
    // sandboxes (the override never disables it); without one it runs
    // plain, which by definition can read the placeholder. Either way the
    // command RUNS.
    let (_dir, key, _normal, mut ctx) = guarded_ctx();
    ctx.allow_unsandboxed_bash = true;
    let reg = Registry::standard();
    let out = run(
        &reg,
        &mut ctx,
        "bash",
        json!({"command": format!("cat {}; echo ran", key.display())}),
    )
    .unwrap();
    assert!(out.output.contains("ran"), "{}", out.output);
    if temur::tools::sandbox_available() {
        assert!(
            !out.output.contains("placeholder-not-a-real-key"),
            "a working sandbox still masks under the override: {}",
            out.output
        );
    }
}

#[test]
fn guard_bash_keyless_spawns_exactly_as_before() {
    // Keyless: no sandbox, no probe, no refusal; a key file on disk that
    // is NOT configured is readable, byte-identical to pre-T18 behavior.
    let dir = tempfile::tempdir().unwrap();
    let stray = dir.path().join("stray.key");
    std::fs::write(&stray, "placeholder-not-a-real-key\n").unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let out = run(
        &reg,
        &mut ctx,
        "bash",
        json!({"command": format!("cat {}", stray.display())}),
    )
    .unwrap();
    assert!(out.output.contains("placeholder-not-a-real-key"), "{}", out.output);
}

// --- T18 P4: active-key redaction at the registry chokepoint -----------------

#[test]
fn redaction_scrubs_registered_key_from_output_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("leaky.txt");
    std::fs::write(&f, "prefix placeholder-not-a-real-key-1234 suffix\n").unwrap();
    let mut reg = Registry::standard();
    reg.set_redaction_key(Some("placeholder-not-a-real-key-1234".into()));
    let mut ctx = ctx_in(dir.path());

    // Ok path: a read whose content contains the key comes back scrubbed.
    let out = run(&reg, &mut ctx, "read", json!({"filePath": f.to_str().unwrap()})).unwrap();
    assert!(!out.output.contains("placeholder-not-a-real-key-1234"), "{}", out.output);
    assert!(out.output.contains("prefix [redacted] suffix"), "{}", out.output);

    // Err path: a missing-file error naming a key-bearing path is scrubbed.
    let ghost = dir.path().join("placeholder-not-a-real-key-1234.txt");
    let err = run(&reg, &mut ctx, "read", json!({"filePath": ghost.to_str().unwrap()}))
        .unwrap_err()
        .to_string();
    assert!(!err.contains("placeholder-not-a-real-key-1234"), "{err}");
    assert!(err.contains("[redacted]"), "{err}");

    // bash output is scrubbed through the same chokepoint (sandbox or not,
    // the key STRING here is test data echoed by the command, not a file).
    let out = run(
        &reg,
        &mut ctx,
        "bash",
        json!({"command": "echo placeholder-not-a-real-key-1234"}),
    )
    .unwrap();
    assert!(!out.output.contains("placeholder-not-a-real-key-1234"), "{}", out.output);
    assert!(out.output.contains("[redacted]"), "{}", out.output);
}

#[test]
fn redaction_covers_the_truncation_boundary() {
    // Key placed to STRADDLE the 30,000-char central cut: if truncation ran
    // first, the key's head would survive in the kept slice. Redaction runs
    // first, so no key byte can ride the cut.
    let dir = tempfile::tempdir().unwrap();
    let key = "placeholder-not-a-real-key-1234"; // 31 chars
    let f = dir.path().join("big.txt");
    let body = format!("{}{}{}", "x".repeat(29_990), key, "y".repeat(2_000));
    std::fs::write(&f, &body).unwrap();
    let mut reg = Registry::standard();
    reg.set_redaction_key(Some(key.into()));
    let mut ctx = ctx_in(dir.path());

    let out = run(
        &reg,
        &mut ctx,
        "bash",
        json!({"command": format!("cat {}", f.display())}),
    )
    .unwrap();
    assert!(out.output.contains("(output truncated"), "the cap must fire: {}", out.output);
    assert!(!out.output.contains(key), "{}", &out.output[29_900..30_100.min(out.output.len())]);
    assert!(
        !out.output.contains("placeholder-not-a-real"),
        "not even a key prefix may survive the cut"
    );
}

#[test]
fn redaction_ignores_short_keys_and_cleared_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());

    // Shorter than 8 chars: stored but never matched.
    reg.set_redaction_key(Some("short".into()));
    let out = run(&reg, &mut ctx, "bash", json!({"command": "echo a short word"})).unwrap();
    assert!(out.output.contains("a short word"), "{}", out.output);
    assert!(!out.output.contains("[redacted]"), "{}", out.output);

    // Clearing (switch to keyless) stops redaction entirely.
    reg.set_redaction_key(Some("placeholder-not-a-real-key-1234".into()));
    reg.set_redaction_key(None);
    let out = run(
        &reg,
        &mut ctx,
        "bash",
        json!({"command": "echo placeholder-not-a-real-key-1234"}),
    )
    .unwrap();
    assert!(out.output.contains("placeholder-not-a-real-key-1234"), "{}", out.output);
}

// --------------------------------------------------------------- T28 (P1)

/// A tool that overrides the truncation advice, plus one that does not, so
/// the marker's variable half is provably per-tool while its fixed half
/// stays byte-identical (the pins above still hold unchanged).
struct HintedTool;
impl Tool for HintedTool {
    fn name(&self) -> &'static str { "hinted" }
    fn description(&self) -> &'static str { "emits too much" }
    fn truncation_hint(&self) -> &'static str { "ask for one piece at a time" }
    fn input_schema(&self) -> serde_json::Value { json!({"type":"object","properties":{}}) }
    fn execute(&self, _i: serde_json::Value, _c: &mut ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput { title: "hinted".into(), output: "z".repeat(40_000) })
    }
}

#[test]
fn truncation_marker_carries_the_tools_own_hint() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::with_tools(vec![Box::new(HintedTool)]);
    let mut ctx = ctx_in(dir.path());
    let out = run(&reg, &mut ctx, "hinted", json!({})).unwrap();
    assert!(
        out.output.contains(
            "(output truncated: showing the first 15000 and last 15000 of 40000 chars; \
             ask for one piece at a time)"
        ),
        "{}",
        out.output
    );
    assert!(
        !out.output.contains("grep or head/tail"),
        "the default advice must not survive an override: {}",
        out.output
    );
}

/// The dispatch-time cap a tool reads to decide its own shape (T28) is the
/// registry's context-scaled cap, not the ceiling.
struct CapEchoTool;
impl Tool for CapEchoTool {
    fn name(&self) -> &'static str { "capecho" }
    fn description(&self) -> &'static str { "reports the cap it was given" }
    fn input_schema(&self) -> serde_json::Value { json!({"type":"object","properties":{}}) }
    fn execute(&self, _i: serde_json::Value, c: &mut ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput { title: "cap".into(), output: c.output_cap.to_string() })
    }
}

#[test]
fn execute_hands_the_tool_the_context_scaled_cap() {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = Registry::with_tools(vec![Box::new(CapEchoTool)]);
    let mut ctx = ctx_in(dir.path());
    // Default: the ceiling, exactly what a bare ToolCtx already carries.
    assert_eq!(run(&reg, &mut ctx, "capecho", json!({})).unwrap().output, "30000");
    reg.set_context_window(Some(8_000));
    assert_eq!(run(&reg, &mut ctx, "capecho", json!({})).unwrap().output, "8000");
    // And the T19 floor still applies on the way down.
    reg.set_context_window(Some(100));
    assert_eq!(run(&reg, &mut ctx, "capecho", json!({})).unwrap().output, "4000");
}

// --- T33 tolerant scalar coercion -----------------------------------------

/// The three shapes taken VERBATIM from the T32 archive (2026-08-15,
/// Llama-3.2-3B): a boolean sent as `"false"`, a `u64` sent as `"600000"`,
/// and an optional `u64` sent as the string `"null"`. Each was rejected at
/// the parse boundary and resent until the repeat guard stopped the loop.
#[test]
fn t33_archived_stringified_scalars_are_coerced() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());

    // 1. edit replaceAll: "false" -> false. Two occurrences, so reading it
    //    as false is what produces the ambiguity error — a call that merely
    //    parsed would not prove the VALUE.
    let f = dir.path().join("e.txt");
    std::fs::write(&f, "foo bar foo").unwrap();
    let fp = f.to_str().unwrap();
    let err = run(&reg, &mut ctx, "edit",
        json!({"filePath": fp, "oldString": "foo", "newString": "baz", "replaceAll": "false"}))
        .unwrap_err();
    assert!(err.to_string().contains("2 times"), "replaceAll \"false\" must read as false: {err}");
    // ...and the other direction, so the coercion is not a constant.
    run(&reg, &mut ctx, "edit",
        json!({"filePath": fp, "oldString": "foo", "newString": "baz", "replaceAll": "true"}))
        .unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "baz bar baz");

    // 2. bash timeout: the archived "600000" parses, and a stringified
    //    small bound proves the NUMBER survives, not just the parse.
    let out = run(&reg, &mut ctx, "bash",
        json!({"command": "echo hi", "timeout": "600000"})).unwrap();
    assert!(out.output.contains("hi"));
    let out = run(&reg, &mut ctx, "bash",
        json!({"command": "sleep 5", "timeout": "200"})).unwrap();
    assert!(out.output.contains("timed out"), "stringified timeout must bind: {}", out.output);

    // 3. read offset/limit: the string "null" reads as absent (whole file),
    //    and a digit string binds as the number.
    let r = dir.path().join("r.txt");
    std::fs::write(&r, "alpha\nbeta\ngamma\n").unwrap();
    let rp = r.to_str().unwrap();
    let out = run(&reg, &mut ctx, "read",
        json!({"filePath": rp, "offset": "null", "limit": "null"})).unwrap();
    assert!(out.output.contains("alpha") && out.output.contains("gamma"));
    let out = run(&reg, &mut ctx, "read",
        json!({"filePath": rp, "offset": "2", "limit": "1"})).unwrap();
    assert!(out.output.contains("beta"), "{}", out.output);
    assert!(!out.output.contains("alpha") && !out.output.contains("gamma"), "{}", out.output);
    // The archived "0" shape parses too (and keeps its existing range
    // check, which coercion does not touch).
    let err = run(&reg, &mut ctx, "read", json!({"filePath": rp, "offset": "0"})).unwrap_err();
    assert!(err.to_string().contains("greater than or equal to 1"), "{err}");
}

/// Real scalars, real `null`, and absent fields keep their pre-T33 path.
#[test]
fn t33_real_scalars_and_absence_are_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let r = dir.path().join("r.txt");
    std::fs::write(&r, "alpha\nbeta\ngamma\n").unwrap();
    let rp = r.to_str().unwrap();

    // Absent -> whole file; real null -> whole file; real numbers -> bound.
    let whole = run(&reg, &mut ctx, "read", json!({"filePath": rp})).unwrap().output;
    let nulls = run(&reg, &mut ctx, "read",
        json!({"filePath": rp, "offset": null, "limit": null})).unwrap().output;
    assert_eq!(whole, nulls);
    let out = run(&reg, &mut ctx, "read",
        json!({"filePath": rp, "offset": 2, "limit": 1})).unwrap().output;
    assert!(out.contains("beta") && !out.contains("alpha"));

    // Real booleans on both sides.
    let f = dir.path().join("e.txt");
    std::fs::write(&f, "foo bar foo").unwrap();
    let fp = f.to_str().unwrap();
    let err = run(&reg, &mut ctx, "edit",
        json!({"filePath": fp, "oldString": "foo", "newString": "baz", "replaceAll": false}))
        .unwrap_err();
    assert!(err.to_string().contains("2 times"), "{err}");
    run(&reg, &mut ctx, "edit",
        json!({"filePath": fp, "oldString": "foo", "newString": "baz", "replaceAll": true}))
        .unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "baz bar baz");
}

/// Everything the coercion does NOT accept still fails LOUDLY, with a
/// message that names the accepted forms so the loop stays self-healing.
/// No trimming, no case tolerance, no floats, no signs.
#[test]
fn t33_unaccepted_strings_fail_loudly_with_the_accepted_forms() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let r = dir.path().join("r.txt");
    std::fs::write(&r, "alpha\n").unwrap();
    let rp = r.to_str().unwrap();
    let f = dir.path().join("e.txt");
    std::fs::write(&f, "foo").unwrap();
    let fp = f.to_str().unwrap();

    const BOOL_FORMS: &str = "expected a boolean, or the string \"true\" or \"false\"";
    const U64_FORMS: &str =
        "expected a number, or a string of digits like \"600000\", or the string \"null\"";

    // bool: garbage, and the near-misses the rule deliberately excludes.
    for bad in ["maybe", "", "True", "FALSE", " true", "true ", "1", "0", "yes"] {
        let err = run(&reg, &mut ctx, "edit", json!({
            "filePath": fp, "oldString": "foo", "newString": "baz", "replaceAll": bad
        })).unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)), "{bad:?} must be InvalidInput");
        let msg = err.to_string();
        assert!(msg.contains(BOOL_FORMS), "{bad:?} message must name the accepted forms: {msg}");
    }
    // The file was never touched by any of those.
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "foo");

    // u64: garbage, floats, signs, whitespace, separators, case.
    for bad in ["maybe", "", "12.5", "-3", "+3", " 12", "12 ", "1_000", "0x10", "1e3", "NULL"] {
        let err = run(&reg, &mut ctx, "read",
            json!({"filePath": rp, "limit": bad})).unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)), "{bad:?} must be InvalidInput");
        let msg = err.to_string();
        assert!(msg.contains(U64_FORMS), "{bad:?} message must name the accepted forms: {msg}");
    }
    // Digits that overflow u64 say so rather than claim digits are unusable.
    let err = run(&reg, &mut ctx, "read",
        json!({"filePath": rp, "limit": "99999999999999999999999"})).unwrap_err();
    assert!(err.to_string().contains("out of range for u64"), "{err}");

    // Real floats and negatives keep failing exactly as they did pre-T33.
    for bad in [json!(12.5), json!(-3)] {
        let err = run(&reg, &mut ctx, "read",
            json!({"filePath": rp, "limit": bad})).unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)), "{bad} must be InvalidInput");
        assert!(err.to_string().contains("expected u64"), "{err}");
    }
}

/// NO-CORRUPTION PIN. The reason coercion is field-level and not a value
/// walk: an edit whose oldString/newString is the literal string "false"
/// must be treated as text, byte-for-byte, with no coercion anywhere near
/// it.
#[test]
fn t33_string_fields_named_false_are_never_coerced() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let f = dir.path().join("cfg.txt");
    std::fs::write(&f, "enabled = false\n").unwrap();
    let fp = f.to_str().unwrap();

    run(&reg, &mut ctx, "edit", json!({
        "filePath": fp, "oldString": "false", "newString": "true", "replaceAll": false
    })).unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "enabled = true\n");

    // And back, with every scalar arg sent stringified at the same time:
    // the scalars coerce, the text fields do not.
    run(&reg, &mut ctx, "edit", json!({
        "filePath": fp, "oldString": "true", "newString": "false", "replaceAll": "false"
    })).unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "enabled = false\n");

    // Digit-only text is text too: writing "600000" over "false" is a
    // string replacement, not a number.
    run(&reg, &mut ctx, "edit", json!({
        "filePath": fp, "oldString": "false", "newString": "600000"
    })).unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "enabled = 600000\n");
}
