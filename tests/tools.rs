//! M3 tool tests — temp dirs on native tmpfs/ext4, run as i686 binaries.

use temur::tools::{PromptProfile, Registry, Tool, ToolCtx, ToolError, ToolOutput, WalkLimits};
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

/// The sibling pin, from the other end of the same failure: an array
/// schema that does not say what its elements are. JSON Schema permits a
/// bare `{"type": "array"}`, but shipped chat templates walk the schema to
/// print it, and several of them dereference the element type
/// unconditionally: gpt-oss-20b's bundled template renders
/// `{%- if param_spec['items'] -%}` for every array-typed parameter, so a
/// missing "items" is not a vaguer prompt but HTTP 500 on every turn that
/// sends tools, which is 0/9 on the nine-task eval (archive:
/// e1-2026-09-09/isolate2-gptoss20b.log, and T57 for the fix).
///
/// Walks BOTH prompt profiles and every nested schema level, so an array
/// without "items", in any tool, at any depth, fails here rather than in
/// the field. The element type is a product decision; having one is not.
#[test]
fn every_array_schema_declares_its_items() {
    fn is_array_type(t: &serde_json::Value) -> bool {
        match t {
            serde_json::Value::String(s) => s == "array",
            // A union type is already banned by the pin above; accept the
            // spelling here anyway so the two pins cannot disagree.
            serde_json::Value::Array(list) => {
                list.iter().any(|e| e.as_str() == Some("array"))
            }
            _ => false,
        }
    }
    fn walk(v: &serde_json::Value, tool: &str, path: &str) {
        match v {
            serde_json::Value::Object(map) => {
                if map.get("type").map(is_array_type).unwrap_or(false) {
                    assert!(
                        map.contains_key("items"),
                        "{tool}: array schema at {path} declares no \"items\"; \
                         templates that print the element type render it as a \
                         hard error, not as a looser parameter"
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
        assert!(defs.iter().any(|d| d.name == "spreadsheet"), "spreadsheet tool missing");
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

    // T31 (D3): known types get a remedy they can actually run. The PDF
    // branch is GONE as of T54, deliberately: a PDF is no longer refused,
    // it is read, so there is no hint to give. Garbage with a .pdf name
    // now fails as an unreadable PDF rather than as a binary file, which
    // is the T54 P1 sentence and is asserted with the others there.
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

/// T59: a comma outside braces is read as a list of alternatives. The
/// first pattern is the exact shape 7 of 10 parent task 5 runs sent.
#[test]
fn glob_comma_joined_pattern_finds_the_files_it_names() {
    let dir = tempfile::tempdir().unwrap();
    for f in ["alpha.txt", "beta.txt", "gamma.txt", "notes.md", "a.txt", "a,b.txt"] {
        std::fs::write(dir.path().join(f), "x").unwrap();
    }
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let closing = "(comma-separated pattern read as 3 alternatives)";

    let out = run(&reg, &mut ctx, "glob", json!({"pattern": "alpha.txt,beta.txt,gamma.txt"}))
        .unwrap();
    for f in ["alpha.txt", "beta.txt", "gamma.txt"] {
        assert!(out.output.contains(f), "{f} missing from:\n{}", out.output);
    }
    assert!(!out.output.contains("notes.md"));
    assert!(!out.output.contains("No files found"));
    assert!(out.output.ends_with(closing), "{}", out.output);
    // The title stays the raw pattern: the transcript shows what was sent.
    assert_eq!(out.title, "alpha.txt,beta.txt,gamma.txt");

    // Braces are globset's own alternation; the comma inside them is not
    // ours to read and no closing line is added.
    let out = run(&reg, &mut ctx, "glob", json!({"pattern": "*.{txt,md}"})).unwrap();
    assert!(out.output.contains("alpha.txt"));
    assert!(out.output.contains("notes.md"));
    assert!(!out.output.contains("comma-separated"), "{}", out.output);

    // The literal pattern is still in the set: a file really named a,b.txt
    // is found by a,b.txt, and the line counts the two pieces it also tried.
    let out = run(&reg, &mut ctx, "glob", json!({"pattern": "a,b.txt"})).unwrap();
    let lines: Vec<&str> = out.output.lines().collect();
    assert_eq!(lines.len(), 2, "one hit and the closing line:\n{}", out.output);
    assert!(lines[0].ends_with("/a,b.txt"), "{}", out.output);
    assert_eq!(lines[1], "(comma-separated pattern read as 2 alternatives)");

    // Spaces after the commas are trimmed away.
    let out = run(&reg, &mut ctx, "glob", json!({"pattern": "alpha.txt, gamma.txt"})).unwrap();
    assert!(out.output.contains("alpha.txt"));
    assert!(out.output.contains("gamma.txt"));
    assert!(!out.output.contains("beta.txt"));

    // An empty piece is dropped, not an error.
    let out = run(&reg, &mut ctx, "glob", json!({"pattern": "a.txt,"})).unwrap();
    assert!(out.output.contains("a.txt"));
    assert!(out.output.ends_with("(comma-separated pattern read as 1 alternative)"), "{}", out.output);

    // Nothing named: the search still finished, and the line still says
    // how the pattern was read.
    let out = run(&reg, &mut ctx, "glob", json!({"pattern": "x.nope,y.nope"})).unwrap();
    assert_eq!(
        out.output,
        "No files found\n(comma-separated pattern read as 2 alternatives)"
    );
}

/// T59 Layer B: temur has no Task tool, so glob and grep no longer send
/// an open-ended search to one. bash.txt's "TodoWrite or Task tools"
/// lines are half true (todowrite exists) and are left as they are.
#[test]
fn glob_and_grep_descriptions_name_no_task_tool() {
    let defs = Registry::standard().definitions();
    for name in ["glob", "grep"] {
        let d = defs.iter().find(|d| d.name == name).unwrap();
        assert!(!d.description.contains("Task tool"), "{name}: {}", d.description);
    }
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

// --- T62 P1(a): grep searches a document as text ---------------------------

#[test]
fn a_compressed_pdf_is_searched_as_text_not_as_bytes() {
    // sample-resume.pdf is flate-compressed and carries NULs in its first
    // 4 KB, so the byte path skipped it entirely and answered "No matches
    // found" for text that is plainly in the document.
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(office_fixture("sample-resume.pdf"), dir.path().join("resume.pdf")).unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());

    let out = run(&reg, &mut ctx, "grep", json!({"pattern": "Jordan"})).unwrap();
    assert!(out.output.contains("resume.pdf:"), "{}", out.output);
    assert!(!out.output.contains("No matches found"), "{}", out.output);
}

#[test]
fn the_line_number_grep_reports_is_one_read_offset_accepts() {
    // The whole point of (a): a model can act on the number. grep's line N
    // and read's offset=N must name the same line of the same extracted
    // text, so the number is taken from grep and handed to read.
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(office_fixture("sample-resume.pdf"), dir.path().join("resume.pdf")).unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());

    let out = run(&reg, &mut ctx, "grep", json!({"pattern": "Experience"})).unwrap();
    let line = out
        .output
        .lines()
        .find(|l| l.contains("resume.pdf:"))
        .expect("a match line");
    // "<path>:<lineno>: <text>"
    let lineno: u64 = line
        .split(':')
        .nth(1)
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or_else(|| panic!("no line number in {line}"));

    let back = run(
        &reg,
        &mut ctx,
        "read",
        json!({"filePath": dir.path().join("resume.pdf").to_str().unwrap(),
               "offset": lineno, "limit": 1}),
    )
    .unwrap();
    assert!(
        back.output.contains("Experience"),
        "grep said line {lineno} but read at that offset shows:\n{}",
        back.output
    );
}

#[test]
fn a_document_grep_cannot_extract_is_skipped_not_an_error() {
    // Extraction failure must not fail the grep: the rest of the walk is still
    // searched and the result is not an error. What it must NOT be is silent,
    // which is the whole point of the disclosure below.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("broken.pdf"), b"%PDF-1.4\nnot really a pdf\n").unwrap();
    std::fs::write(dir.path().join("notes.txt"), "findme here\n").unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());

    let out = run(&reg, &mut ctx, "grep", json!({"pattern": "findme"})).unwrap();
    assert!(out.output.contains("notes.txt:1:"), "{}", out.output);
    assert!(out.output.contains("Found 1 matches"), "{}", out.output);
    // Matches and a skip both appear, and the skip never reads as a match.
    assert!(
        out.output.contains(
            "1 document file could not be read as text and was skipped; read it to see the error."
        ),
        "{}",
        out.output
    );
}

#[test]
fn a_document_that_cannot_be_read_as_text_is_disclosed() {
    // The residue (a) leaves: a document grep could not turn into text is the
    // only document it still skips, and answering "No matches found" for it is
    // the same false answer the raw-byte skip used to give.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("broken.pdf"), b"%PDF-1.4\nnot really a pdf\n").unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());

    let out = run(&reg, &mut ctx, "grep", json!({"pattern": "backlog"})).unwrap();
    assert!(out.output.contains("No matches found"), "{}", out.output);
    assert!(
        out.output.contains(
            "1 document file could not be read as text and was skipped; read it to see the error."
        ),
        "{}",
        out.output
    );
}

#[test]
fn the_unreadable_document_disclosure_agrees_in_number() {
    // Both forms pinned either side of the boundary.
    for (count, want) in [
        (1usize, "1 document file could not be read as text and was skipped; read it to see the error."),
        (2usize, "2 document files could not be read as text and were skipped; read one to see the error."),
    ] {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..count {
            std::fs::write(dir.path().join(format!("b{i}.pdf")), b"%PDF-1.4\nnope\n").unwrap();
        }
        let reg = Registry::standard();
        let mut ctx = ctx_in(dir.path());
        let out = run(&reg, &mut ctx, "grep", json!({"pattern": "anything"})).unwrap();
        assert!(out.output.contains(want), "count {count}: {}", out.output);
    }
}

#[test]
fn a_readable_document_is_never_counted_as_unreadable() {
    // sample-resume.pdf is now SEARCHED, not skipped, so it must not appear in
    // the unreadable count. The inverse of the candidate-(b) test it replaces.
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(office_fixture("sample-resume.pdf"), dir.path().join("resume.pdf")).unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());

    let out = run(&reg, &mut ctx, "grep", json!({"pattern": "Jordan"})).unwrap();
    assert!(out.output.contains("resume.pdf:"), "{}", out.output);
    assert!(!out.output.contains("could not be read as text"), "{}", out.output);
}

#[test]
fn a_non_document_binary_is_still_skipped_by_bytes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.o"), [0u8, 1, 2, 3]).unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());

    let out = run(&reg, &mut ctx, "grep", json!({"pattern": "anything"})).unwrap();
    // Exactly "No matches found": a non-document binary is skipped by bytes and
    // is NOT counted as an unreadable document.
    assert_eq!(out.output, "No matches found", "{}", out.output);
}

// --- T53 P1: a walk can be interrupted, and cannot run forever (D21) -------
//
// The shipped constants are deliberately large, so every limit test injects
// its own through ToolCtx::walk_limits rather than lowering them.

/// Files spread over a few directories, so the walk visits directory
/// entries as well as file entries.
fn generated_tree(root: &std::path::Path, files: usize) {
    let per_dir = 100;
    for i in 0..files {
        let d = root.join(format!("d{}", i / per_dir));
        if i % per_dir == 0 {
            std::fs::create_dir_all(&d).unwrap();
        }
        std::fs::write(d.join(format!("f{i}.txt")), "needle here\n").unwrap();
    }
}

#[test]
fn glob_cancel_before_walk_is_bash_shaped_interruption() {
    let dir = tempfile::tempdir().unwrap();
    generated_tree(dir.path(), 20);
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    ctx.cancel.set();

    let err = run(&reg, &mut ctx, "glob", json!({"pattern": "**/*.txt"})).unwrap_err();
    // bash's shape for an Esc mid-command: an error result carrying the
    // partial output and the same marker. Nothing was walked, so the
    // marker stands alone; "No files found" would claim a finished search.
    match err {
        ToolError::Failed(msg) => assert_eq!(msg, "(interrupted by user)"),
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn grep_cancel_before_walk_is_bash_shaped_interruption() {
    let dir = tempfile::tempdir().unwrap();
    generated_tree(dir.path(), 20);
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    ctx.cancel.set();

    let err = run(&reg, &mut ctx, "grep", json!({"pattern": "needle"})).unwrap_err();
    match err {
        ToolError::Failed(msg) => assert_eq!(msg, "(interrupted by user)"),
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn glob_cancel_midwalk_returns_promptly() {
    // 10,000 files take far longer to walk than the 15 ms the setter
    // sleeps (a warm 93,035-entry walk on this box is 0.7 s, so this tree
    // is tens of ms), which keeps the cancel landing mid-walk with a wide
    // margin. The assertion itself is only the generous wall-clock bound.
    let dir = tempfile::tempdir().unwrap();
    generated_tree(dir.path(), 10_000);
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());

    let token = ctx.cancel.clone();
    let setter = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(15));
        token.set();
    });

    let started = std::time::Instant::now();
    let res = run(&reg, &mut ctx, "glob", json!({"pattern": "**/*.txt"}));
    let elapsed = started.elapsed();
    setter.join().unwrap();

    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "a cancelled walk must return promptly, took {elapsed:?}"
    );
    if let Err(ToolError::Failed(msg)) = &res {
        assert!(msg.ends_with("(interrupted by user)"), "{msg}");
    } else {
        // The walk beat the setter: acceptable, and still bounded above.
        assert!(res.is_ok(), "unexpected error shape: {res:?}");
    }
}

#[test]
fn glob_entries_cap_returns_partial_hits_and_closing_line() {
    let dir = tempfile::tempdir().unwrap();
    generated_tree(dir.path(), 50);
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    ctx.walk_limits = Some(WalkLimits {
        max_entries: 6,
        deadline: std::time::Duration::from_secs(30),
    });

    let out = run(&reg, &mut ctx, "glob", json!({"pattern": "**/*.txt"})).unwrap();
    assert!(out.output.contains("search stopped after"), "{}", out.output);
    assert!(out.output.contains("entries under"), "{}", out.output);
    assert!(
        out.output.contains("narrow the path or pattern"),
        "{}",
        out.output
    );
    let listed = out.output.lines().filter(|l| l.ends_with(".txt")).count();
    assert!(
        listed > 0 && listed < 50,
        "expected a partial listing, got {listed} of 50"
    );
}

#[test]
fn glob_deadline_returns_closing_line() {
    let dir = tempfile::tempdir().unwrap();
    generated_tree(dir.path(), 20);
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    // A zero deadline trips on the first entry, deterministically.
    ctx.walk_limits = Some(WalkLimits {
        max_entries: u64::MAX,
        deadline: std::time::Duration::ZERO,
    });

    let out = run(&reg, &mut ctx, "glob", json!({"pattern": "**/*.txt"})).unwrap();
    assert!(out.output.contains("search stopped after 0 s"), "{}", out.output);
    // A search cut short never claims it found nothing.
    assert!(!out.output.contains("No files found"), "{}", out.output);
}

#[test]
fn grep_entries_cap_returns_partial_matches_and_closing_line() {
    let dir = tempfile::tempdir().unwrap();
    generated_tree(dir.path(), 50);
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    ctx.walk_limits = Some(WalkLimits {
        max_entries: 6,
        deadline: std::time::Duration::from_secs(30),
    });

    let out = run(&reg, &mut ctx, "grep", json!({"pattern": "needle"})).unwrap();
    assert!(out.output.contains("search stopped after"), "{}", out.output);
    assert!(
        out.output.contains("narrow the path or pattern"),
        "{}",
        out.output
    );
    let hits = out.output.lines().filter(|l| l.contains("needle here")).count();
    assert!(hits > 0 && hits < 50, "expected partial matches, got {hits}");
}

#[test]
fn grep_deadline_returns_closing_line() {
    let dir = tempfile::tempdir().unwrap();
    generated_tree(dir.path(), 20);
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    ctx.walk_limits = Some(WalkLimits {
        max_entries: u64::MAX,
        deadline: std::time::Duration::ZERO,
    });

    let out = run(&reg, &mut ctx, "grep", json!({"pattern": "needle"})).unwrap();
    assert!(out.output.contains("search stopped after 0 s"), "{}", out.output);
    assert!(!out.output.contains("No matches found"), "{}", out.output);
}

#[test]
fn guard_denied_paths_stay_absent_from_a_truncated_walk() {
    // T18 order is unchanged by T53: the guard is consulted for every
    // visited entry, so stopping early can never leak one. Proven both
    // ways: guarded plus a tripped limit hides the key, and a keyless ctx
    // over the same tree still finds it (so the file really is reachable).
    let (dir, key, _normal, mut ctx) = guarded_ctx();
    let reg = Registry::standard();
    for i in 0..40 {
        std::fs::write(dir.path().join(format!("f{i}.txt")), "bulk needle\n").unwrap();
    }
    ctx.walk_limits = Some(WalkLimits {
        max_entries: 8,
        deadline: std::time::Duration::from_secs(30),
    });

    let out = run(&reg, &mut ctx, "glob", json!({"pattern": "**/*"})).unwrap();
    assert!(out.output.contains("search stopped after"), "{}", out.output);
    assert!(!out.output.contains("api.key"), "{}", out.output);

    let out = run(&reg, &mut ctx, "grep", json!({"pattern": "placeholder-not-a-real"})).unwrap();
    assert!(!out.output.contains("api.key"), "{}", out.output);

    let mut plain = ctx_in(dir.path());
    let out = run(&reg, &mut plain, "glob", json!({"pattern": "**/*.key"})).unwrap();
    assert!(out.output.contains(key.to_str().unwrap()), "{}", out.output);
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
        vec!["read", "write", "edit", "bash", "glob", "grep", "spreadsheet", "todowrite", "todoread"]
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
    assert!(err.to_string().contains("newString equals oldString: there is nothing to change"));
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
    // T63 P2b: the null read runs on a fresh context. On the same context it
    // is the same window over the same unchanged file, so it would carry the
    // repeat marker; what T33 pins is that null and absent parse alike.
    let mut fresh = ctx_in(dir.path());
    let nulls = run(&reg, &mut fresh, "read",
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

// --- T54 P1: the read tool reads documents (D20) ---------------------------

fn office_fixture(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/office")
        .join(name)
}

/// Read a fixture through the REAL tool, from its own directory.
fn read_doc(name: &str, extra: serde_json::Value) -> Result<ToolOutput, ToolError> {
    let reg = Registry::standard();
    let dir = office_fixture(name);
    let mut ctx = ctx_in(dir.parent().unwrap());
    let mut input = json!({"filePath": dir.to_str().unwrap()});
    for (k, v) in extra.as_object().unwrap() {
        input[k] = v.clone();
    }
    run(&reg, &mut ctx, "read", input)
}

#[test]
fn a_pdf_with_a_flate_stream_reads_as_text() {
    let out = read_doc("synth-flate.pdf", json!({})).unwrap();
    assert!(out.output.contains("Quarterly Revenue Report"), "{}", out.output);
    assert!(out.output.contains("Total units shipped: 1842"), "{}", out.output);
}

#[test]
fn an_uncompressed_pdf_reads_the_same_way() {
    let out = read_doc("synth-plain.pdf", json!({})).unwrap();
    assert!(out.output.contains("Quarterly Revenue Report"), "{}", out.output);
}

#[test]
fn a_real_shaped_resume_pdf_reads_as_recognisable_text() {
    let out = read_doc("sample-resume.pdf", json!({})).unwrap();
    assert!(out.output.contains("Jordan Q. Sample"), "{}", out.output);
    assert!(out.output.contains("Experience"), "{}", out.output);
}

#[test]
fn a_workbook_reads_one_block_per_sheet_with_cached_values() {
    let out = read_doc("sample-timesheet.xlsx", json!({})).unwrap();
    assert!(out.output.contains("== Sheet: Timesheet =="), "{}", out.output);
    assert!(out.output.contains("== Sheet: Summary =="), "{}", out.output);
    // SUM/SUMIF cells arrive as the CACHED value, never as a formula.
    assert!(out.output.contains("34.5"), "{}", out.output);
    assert!(!out.output.contains("=SUM"), "formulas must never appear: {}", out.output);
    assert!(!out.output.contains("SUMIF"), "{}", out.output);
}

#[test]
fn an_ods_reads_through_the_same_path() {
    let out = read_doc("synth.ods", json!({})).unwrap();
    assert!(out.output.contains("== Sheet: Sheet1 =="), "{}", out.output);
    assert!(out.output.contains("alpha,1"), "{}", out.output);
}

#[test]
fn a_docx_reads_paragraphs_as_lines_and_tables_as_tabs() {
    let out = read_doc("synth.docx", json!({})).unwrap();
    assert!(out.output.contains("Project Kickoff Notes"), "{}", out.output);
    // The entity reference must survive: quick-xml emits it as its own
    // event and dropping it silently loses the character (T54 P0 finding).
    assert!(
        out.output.contains("schedule & budget"),
        "entity refs must resolve: {}",
        out.output
    );
    assert!(out.output.contains("Task\tOwner"), "{}", out.output);
    assert!(out.output.contains("Draft spec\tDana"), "{}", out.output);
}

#[test]
fn a_real_shaped_docx_reads_its_table_too() {
    let out = read_doc("sample-notes.docx", json!({})).unwrap();
    assert!(out.output.contains("Meeting notes"), "{}", out.output);
    assert!(out.output.contains("Widget\t4\t12.50"), "{}", out.output);
}

#[test]
fn a_document_pages_like_any_long_file() {
    let whole = read_doc("sample-resume.pdf", json!({})).unwrap();
    let first = read_doc("sample-resume.pdf", json!({"limit": 3})).unwrap();
    // Same pipeline: 1-indexed line numbers, a limit, and a continuation hint.
    assert!(first.output.contains("1: "), "{}", first.output);
    assert!(first.output.contains("offset=4"), "{}", first.output);
    assert!(first.output.len() < whole.output.len());

    let second = read_doc("sample-resume.pdf", json!({"offset": 4, "limit": 3})).unwrap();
    assert!(second.output.contains("4: "), "{}", second.output);
    assert!(!second.output.contains("\n1: "), "{}", second.output);
}

#[test]
fn the_input_cap_refuses_and_names_itself() {
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("huge.pdf");
    // Sparse-ish write just over the 32 MiB cap; content is irrelevant
    // because the cap is checked from metadata before any parse.
    let f = std::fs::File::create(&big).unwrap();
    f.set_len(33 * 1024 * 1024).unwrap();
    drop(f);
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let err = run(&reg, &mut ctx, "read", json!({"filePath": big.to_str().unwrap()})).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("32 MiB"), "the cap names itself: {msg}");
}

#[test]
fn a_pdf_with_no_text_layer_says_so_in_one_sentence() {
    // A structurally valid PDF whose single page carries no text operators,
    // which is what a scan looks like to a text extractor.
    let err = read_doc("synth-notext.pdf", json!({})).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("no text layer"), "{msg}");
    assert!(msg.contains("Ask the user for the text"), "{msg}");
    assert_eq!(msg.lines().count(), 1, "one sentence, one line: {msg}");
}

#[test]
fn a_corrupt_workbook_says_so_without_leaking_the_crate_error() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("broken.xlsx");
    std::fs::write(&p, b"PK\x03\x04 this is not a workbook").unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let err = run(&reg, &mut ctx, "read", json!({"filePath": p.to_str().unwrap()})).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("could not read this workbook"), "{msg}");
    assert_eq!(msg.lines().count(), 1, "{msg}");
}

// --- T62 P0a: a workbook is read within a bound ----------------------------

#[test]
fn a_workbook_with_a_far_corner_cell_is_read_instead_of_killing_temur() {
    // Before T62 this test did not FAIL, it ABORTED the test binary. calamine
    // lays a sheet out densely between the extreme cells that are really in
    // it, and this file's extremes are A1 and XFD1048576, so the layout asks
    // for 1,048,576 x 16,384 x 32 bytes = 549,755,813,888. An allocation that
    // large fails, and a failed allocation aborts rather than unwinding, so
    // the catch_unwind around extraction never sees it.
    let out = read_doc("far-corner.xlsx", json!({})).unwrap();
    assert!(out.output.contains("== Sheet: Regions =="), "{}", out.output);
    assert!(out.output.contains("Region,Units"), "{}", out.output);
    assert!(out.output.contains("North,1200"), "{}", out.output);
    assert!(out.output.contains("South,940"), "{}", out.output);
    // The stray cell is a million rows past the window, so the read stops at
    // the window and says so instead of quoting a total it never counted.
    assert!(
        out.output.contains("More of this document was not extracted"),
        "{}",
        out.output
    );
}

#[test]
fn a_workbook_that_declares_a_huge_range_but_holds_three_rows_still_reads() {
    // The guard against fixing the wrong thing. A DECLARED used range is not
    // a bound and must never be treated as one: calamine derives the range
    // from the cells that are really present, so this file (dimension
    // A1:XFD1048576, three rows in it) reads in microseconds. A pre-scan of
    // the declared range would refuse this and still abort on the file above.
    let out = read_doc("declared-huge.xlsx", json!({})).unwrap();
    assert!(out.output.contains("North,1200"), "{}", out.output);
    assert!(out.output.contains("South,940"), "{}", out.output);
    assert!(out.output.contains("End of file"), "{}", out.output);
}

#[test]
fn a_workbook_whose_parts_declare_more_than_the_cap_is_refused() {
    // MAX_UNZIPPED_BYTES has been enforced on the docx path since T54 and
    // never on this one. Only the parts a read inflates are counted.
    let err = read_doc("oversized-part.xlsx", json!({})).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("expands to more than 64 MiB"), "{msg}");
    assert_eq!(msg.lines().count(), 1, "one sentence, one line: {msg}");
}

/// A 50,000 x 10 workbook, written by temur's own writer rather than
/// committed: a 1.5 MB fixture to demonstrate a bound is a bad trade.
fn big_workbook(dir: &std::path::Path, reg: &Registry, ctx: &mut ToolCtx) -> std::path::PathBuf {
    let p = dir.join("big.xlsx");
    let mut csv = String::new();
    for r in 1..=50_000u64 {
        for c in 0..10u64 {
            if c > 0 {
                csv.push(',');
            }
            csv.push_str(&(r * 10 + c).to_string());
        }
        csv.push('\n');
    }
    run(reg, ctx, "write", json!({"filePath": p.to_str().unwrap(), "content": csv})).unwrap();
    p
}

#[test]
fn a_large_honest_workbook_costs_only_its_window() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = big_workbook(dir.path(), &reg, &mut ctx);

    // A small explicit window, because the DEFAULT one is masked: 2000 rows
    // of ten columns exceed read's 28 KB byte cap, and the byte-cap footer
    // then hides which of the two stops fired. Five lines keeps the window
    // itself the binding constraint, so the footer below is the observable
    // proof that extraction touched six rows of fifty thousand: a path that
    // rendered the whole sheet would know the total and quote it.
    let out = run(&reg, &mut ctx, "read", json!({
        "filePath": p.to_str().unwrap(), "limit": 5
    })).unwrap();
    assert!(out.output.contains("10,11,12,13,14,15,16,17,18,19"), "{}", out.output);
    assert!(out.output.contains("40,41,42,43,44,45,46,47,48,49"), "{}", out.output);
    assert!(!out.output.contains("50,51,52"), "past the window: {}", out.output);
    assert!(
        out.output.contains("More of this document was not extracted"),
        "{}",
        out.output
    );
    assert!(!out.output.contains("of 50001"), "no total was counted: {}", out.output);
}

#[test]
fn paging_a_large_workbook_reaches_its_last_rows() {
    // The guard on the retained-cell budget. Cells ahead of the window are
    // counted for line numbering and then dropped, never held, so the
    // 1,000,000-cell backstop cannot stand between the caller and a late
    // page: with the budget on everything collected instead, a 10-column
    // sheet would stop paging at row 100,000 and this read would be empty.
    // Line 1 is the sheet header, so row 49,990 is line 49,991.
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = big_workbook(dir.path(), &reg, &mut ctx);

    let out = run(&reg, &mut ctx, "read", json!({
        "filePath": p.to_str().unwrap(), "offset": 49_991, "limit": 20
    })).unwrap();
    assert!(out.output.contains("499900,499901,499902"), "{}", out.output);
    assert!(out.output.contains("500000,500001,500002"), "{}", out.output);
    // Reaching the real end, not a truncation, is the whole point.
    assert!(out.output.contains("End of file"), "{}", out.output);
}

// --- T62 P0b: an ODS is measured before it is opened -----------------------

/// The <content> body of a read, without the <path> line, so two files can be
/// compared byte for byte.
fn content_of(out: &str) -> String {
    let start = out.find("<content>").expect("content") + "<content>".len();
    let end = out.find("</content>").expect("/content");
    out[start..end].to_string()
}

#[test]
fn an_ods_that_spans_too_much_is_refused_in_one_sentence() {
    // 1,048,576 rows by 95 columns of content span, which calamine lays out
    // as 99,614,720 cells and 3.19 GB. Measured on this box before P0b: the
    // allocation failed and the process aborted, from an 820-byte file.
    let err = read_doc("ods-far-corner.ods", json!({})).unwrap_err();
    let msg = format!("{err}");
    assert_eq!(msg.lines().count(), 1, "one sentence, one line: {msg}");
    assert!(msg.contains("ask the user for the sheet or the range"), "{msg}");
    // No crate error may reach the model.
    assert!(!msg.contains("OdsError"), "{msg}");
    assert!(!msg.contains("CellLimit"), "{msg}");
}

#[test]
fn an_ods_is_measured_before_calamine_opens_it() {
    // Placement is the point, and it is not testable by asserting "an error"
    // arrived: an ODS is laid out inside calamine's open, so a check placed
    // after the open would abort instead of returning. What proves the
    // placement is WHICH sentence comes back. This one can only come from the
    // pre-scan, because it names the span the pre-scan computed.
    let err = read_doc("ods-far-corner.ods", json!({})).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("1048576 rows by 95 columns"), "{msg}");
    assert!(msg.contains("more than temur will lay out"), "{msg}");
}

#[test]
fn a_libreoffice_trailing_block_reads_the_same_as_the_sheet_without_it() {
    // Every sheet LibreOffice saves ends with a trailing empty block
    // declaring the rest of the sheet, so a pre-scan that summed DECLARED
    // repeats would read this three-row spreadsheet as 1,048,576 by 16,384
    // and refuse it. calamine drops the block, and so does the pre-scan.
    let with_block = read_doc("trailing-block.ods", json!({})).unwrap();
    let without = read_doc("plain-3x2.ods", json!({})).unwrap();
    assert!(with_block.output.contains("North,1200"), "{}", with_block.output);
    assert_eq!(
        content_of(&with_block.output),
        content_of(&without.output),
        "the trailing block must change nothing"
    );
}

#[test]
fn a_document_under_a_secrets_dir_is_still_guarded() {
    // T54 changes nothing about T18: the guard runs before any open, so a
    // document is refused exactly as a text file under the same path is.
    let (dir, key, _normal, mut ctx) = guarded_ctx();
    let doc = dir.path().join("secrets").join("private.pdf");
    std::fs::copy(office_fixture("sample-resume.pdf"), &doc).unwrap();
    ctx.guard = temur::tools::KeyGuard::from_paths(vec![key, doc.clone()]);
    let reg = Registry::standard();
    let err = run(&reg, &mut ctx, "read", json!({"filePath": doc.to_str().unwrap()})).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("key isolation"), "{msg}");
    assert!(!msg.contains("Jordan"), "no content may leak: {msg}");
}

#[test]
fn hostile_input_never_panics_and_always_answers() {
    // The T54 P0 spike found pdf-extract panicking on 3 of 25 seeded
    // corruptions (its own expect()s). Extraction runs under catch_unwind
    // now; this replays the same seed across all four parsers and asserts
    // the process survives every one with an ordinary Ok or Err.
    let reg = Registry::standard();
    let dir = tempfile::tempdir().unwrap();
    let mut state: u64 = 20260908; // the spike's seed
    let mut next = || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 33) as usize
    };
    for src in [
        "sample-resume.pdf",
        "sample-timesheet.xlsx",
        "sample-notes.docx",
        "synth.ods",
        // T62: the bound fixtures are structurally valid, so the seeded
        // corruptions are the only thing that reaches their parsers sideways.
        // ods-far-corner.ods is deliberately NOT here: a corruption that
        // happened to defeat the pre-scan would allocate gigabytes inside
        // calamine, and two direct tests already cover that file.
        "far-corner.xlsx",
        "declared-huge.xlsx",
        "trailing-block.ods",
    ] {
        let bytes = std::fs::read(office_fixture(src)).unwrap();
        let ext = std::path::Path::new(src).extension().unwrap().to_str().unwrap();
        for i in 0..50 {
            let mut b = bytes.clone();
            if i % 2 == 0 {
                let keep = 1 + next() % b.len();
                b.truncate(keep);
            } else {
                for _ in 0..(1 + next() % 8) {
                    let at = next() % b.len();
                    b[at] ^= 1u8 << (next() % 8);
                }
            }
            let p = dir.path().join(format!("case.{ext}"));
            std::fs::write(&p, &b).unwrap();
            let mut ctx = ctx_in(dir.path());
            // The assertion IS that this returns rather than aborting.
            let _ = run(&reg, &mut ctx, "read", json!({"filePath": p.to_str().unwrap()}));
        }
    }
}

#[test]
fn the_binary_refusal_still_covers_what_temur_cannot_read() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("deck.pptx");
    std::fs::write(&p, b"PK\x03\x04 not readable here").unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let err = run(&reg, &mut ctx, "read", json!({"filePath": p.to_str().unwrap()})).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("Cannot read binary file"), "{msg}");
    // The image line stays true: temur still cannot see images.
    let img = dir.path().join("shot.png");
    std::fs::write(&img, [0x89, 0x50, 0x4e, 0x47, 0, 1, 2, 3]).unwrap();
    let err = run(&reg, &mut ctx, "read", json!({"filePath": img.to_str().unwrap()})).unwrap_err();
    assert!(format!("{err}").contains("Cannot read binary file"));
}

// --- T54 P2: the write tool writes a spreadsheet (D23) ---------------------

#[test]
fn writing_an_xlsx_round_trips_through_the_read_tool() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("out.xlsx");
    let csv = "x,f(x)\n0,3\n1,2\n2,3\nnote,\"a, comma\"\n";
    run(&reg, &mut ctx, "write", json!({"filePath": p.to_str().unwrap(), "content": csv})).unwrap();

    // Read it back through P1 and compare cell by cell.
    let out = run(&reg, &mut ctx, "read", json!({"filePath": p.to_str().unwrap()})).unwrap();
    assert!(out.output.contains("== Sheet: Sheet1 =="), "{}", out.output);
    assert!(out.output.contains("x,f(x)"), "{}", out.output);
    assert!(out.output.contains("0,3"), "{}", out.output);
    assert!(out.output.contains("1,2"), "{}", out.output);
    assert!(out.output.contains("2,3"), "{}", out.output);
    // The quoted field with an embedded comma survives the round trip.
    assert!(out.output.contains("note,\"a, comma\""), "{}", out.output);
}

#[test]
fn numbers_are_numbers_and_everything_else_is_text() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("t.xlsx");
    let csv = "42,-5,3.5,1e3,007,hello,2026-09-08\n";
    run(&reg, &mut ctx, "write", json!({"filePath": p.to_str().unwrap(), "content": csv})).unwrap();
    let out = run(&reg, &mut ctx, "read", json!({"filePath": p.to_str().unwrap()})).unwrap();
    // Whole numbers read back as integers, not 42.0; a negative number is a
    // number, not a formula lead-in; a date string stays the string it was.
    assert!(out.output.contains("42,-5,3.5,1000,7,hello,2026-09-08"), "{}", out.output);
}

#[test]
fn a_cell_that_looks_like_a_formula_is_written_as_text() {
    // CSV content cannot inject a formula: nothing here calls a
    // formula-writing API, and the read side proves the cell came back as
    // its literal text rather than as a computed value.
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("inject.xlsx");
    let csv = "=SUM(A1:A9),+1+1,-cmd,@ref,plain\n";
    run(&reg, &mut ctx, "write", json!({"filePath": p.to_str().unwrap(), "content": csv})).unwrap();
    let out = run(&reg, &mut ctx, "read", json!({"filePath": p.to_str().unwrap()})).unwrap();
    assert!(out.output.contains("=SUM(A1:A9)"), "{}", out.output);
    assert!(out.output.contains("+1+1"), "{}", out.output);
    assert!(out.output.contains("-cmd"), "{}", out.output);
    assert!(out.output.contains("@ref"), "{}", out.output);
    // Read back as CACHED VALUES; a real formula would have produced a
    // number (or 0), never the source text.
    assert!(!out.output.contains(",2,"), "no formula was evaluated: {}", out.output);
}

#[test]
fn xlsx_writing_keeps_the_write_tools_own_rules() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("rules.xlsx");
    let fp = p.to_str().unwrap();
    run(&reg, &mut ctx, "write", json!({"filePath": fp, "content": "a,1\n"})).unwrap();

    // Read-first still governs an existing file, in a fresh session.
    let mut fresh = ctx_in(dir.path());
    let err = run(&reg, &mut fresh, "write", json!({"filePath": fp, "content": "b,2\n"}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("has not been read in this session"), "{err}");

    // And an overwrite keeps the previous workbook beside it (T64 P0a),
    // which since T65 P1 is the rule for a document this session did not
    // write itself: the fresh session, once it has read the file, is that
    // shape. The session that wrote the workbook replaces its own output.
    run(&reg, &mut fresh, "read", json!({"filePath": fp})).unwrap();
    let out = run(&reg, &mut fresh, "write", json!({"filePath": fp, "content": "b,2\n"})).unwrap();
    assert!(out.output.starts_with("Replaced "), "{}", out.output);
    assert!(out.output.contains("the previous document is at"), "{}", out.output);
}

#[test]
fn a_guarded_xlsx_path_is_refused_before_anything_is_written() {
    let (dir, key, _normal, mut ctx) = guarded_ctx();
    let target = dir.path().join("secrets").join("book.xlsx");
    ctx.guard = temur::tools::KeyGuard::from_paths(vec![key, target.clone()]);
    let reg = Registry::standard();
    let err = run(
        &reg,
        &mut ctx,
        "write",
        json!({"filePath": target.to_str().unwrap(), "content": "a,1\n"}),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("key isolation"), "{err}");
    assert!(!target.exists(), "nothing may be created under a guarded path");
}

#[test]
fn a_non_office_write_is_byte_for_byte_what_it_always_was() {
    // T60: .md stays text too; Markdown only becomes a document under a
    // document extension.
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    for name in ["plain.csv", "notes.md", "memo.txt", "data.json", "page.html"] {
        let p = dir.path().join(name);
        let body = "# x,f(x)\n0,3\n";
        run(&reg, &mut ctx, "write", json!({"filePath": p.to_str().unwrap(), "content": body})).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), body, "{name}");
    }
}

// --- T60 P1: the write tool writes a Word document -------------------------

/// The Markdown sample every supported element appears in, shared by the
/// docx and PDF round trips.
const DOC_SAMPLE: &str = "# Title\n\nFirst paragraph with **bold**, *italic* and `code`.\n\n## Section\n\n- alpha item\n- beta item\n\n### Steps\n\n1. one\n2. two\n\n```\nfn main() {}\nlet x = 1;\n```\n\nLast paragraph.\n";

/// The lines the read tool prints for a document, in order, without the
/// `N: ` numbering, the wrapper lines, or the empty last line the docx
/// reader's trailing newline produces.
fn doc_lines(out: &str) -> Vec<String> {
    let mut lines: Vec<String> = out
        .lines()
        .filter(|l| l.starts_with(|c: char| c.is_ascii_digit()))
        .filter_map(|l| l.split_once(": ").map(|(_, t)| t.to_string()))
        .collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

#[test]
fn writing_a_docx_round_trips_through_the_read_tool() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("out.docx");
    run(&reg, &mut ctx, "write", json!({"filePath": p.to_str().unwrap(), "content": DOC_SAMPLE})).unwrap();

    let out = run(&reg, &mut ctx, "read", json!({"filePath": p.to_str().unwrap()})).unwrap();
    let lines = doc_lines(&out.output);
    let expected = [
        "Title",
        "First paragraph with bold, italic and code.",
        "Section",
        "- alpha item",
        "- beta item",
        "Steps",
        "1. one",
        "2. two",
        "fn main() {}",
        "let x = 1;",
        "Last paragraph.",
    ];
    assert_eq!(lines, expected, "{}", out.output);
}

#[test]
fn a_docx_has_the_parts_a_word_document_needs() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("parts.docx");
    run(&reg, &mut ctx, "write", json!({"filePath": p.to_str().unwrap(), "content": "# Hi\n"})).unwrap();

    let mut archive = zip::ZipArchive::new(std::fs::File::open(&p).unwrap()).unwrap();
    let mut names: Vec<String> = (0..archive.len()).map(|i| archive.by_index(i).unwrap().name().to_string()).collect();
    names.sort();
    assert_eq!(
        names,
        ["[Content_Types].xml", "_rels/.rels", "word/_rels/document.xml.rels", "word/document.xml", "word/styles.xml"]
    );
    let mut types = String::new();
    std::io::Read::read_to_string(&mut archive.by_name("[Content_Types].xml").unwrap(), &mut types).unwrap();
    assert!(types.contains("/word/document.xml"), "{types}");
    // Headings carry their style, and the style part defines it.
    let mut body = String::new();
    std::io::Read::read_to_string(&mut archive.by_name("word/document.xml").unwrap(), &mut body).unwrap();
    assert!(body.contains(r#"<w:pStyle w:val="Heading1"/>"#), "{body}");
    let mut styles = String::new();
    std::io::Read::read_to_string(&mut archive.by_name("word/styles.xml").unwrap(), &mut styles).unwrap();
    assert!(styles.contains(r#"w:styleId="Heading1""#), "{styles}");
    assert!(styles.contains("Courier New"), "{styles}");
}

#[test]
fn a_guarded_docx_path_is_refused_before_anything_is_written() {
    let (dir, key, _normal, mut ctx) = guarded_ctx();
    let target = dir.path().join("secrets").join("memo.docx");
    ctx.guard = temur::tools::KeyGuard::from_paths(vec![key, target.clone()]);
    let reg = Registry::standard();
    let err = run(
        &reg,
        &mut ctx,
        "write",
        json!({"filePath": target.to_str().unwrap(), "content": "# memo\n"}),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("key isolation"), "{err}");
    assert!(!target.exists(), "nothing may be created under a guarded path");
}

#[test]
fn markup_characters_survive_the_docx_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("chars.docx");
    let body = "a < b & c > d, it\u{2019}s \"quoted\"\n";
    run(&reg, &mut ctx, "write", json!({"filePath": p.to_str().unwrap(), "content": body})).unwrap();
    let out = run(&reg, &mut ctx, "read", json!({"filePath": p.to_str().unwrap()})).unwrap();
    assert_eq!(doc_lines(&out.output), ["a < b & c > d, it\u{2019}s \"quoted\""], "{}", out.output);
}

// --- T60 P2: the write tool writes a PDF ------------------------------------

#[test]
fn writing_a_pdf_round_trips_through_pdf_extract() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("out.pdf");
    let out = run(&reg, &mut ctx, "write", json!({"filePath": p.to_str().unwrap(), "content": DOC_SAMPLE})).unwrap();
    // Pure ASCII: nothing was replaced, and the result line says nothing.
    assert!(!out.output.contains("WinAnsi"), "{}", out.output);

    // The read tool's PDF path IS pdf-extract, the same parser the eval
    // scores task 12 with.
    let text = run(&reg, &mut ctx, "read", json!({"filePath": p.to_str().unwrap()})).unwrap().output;
    let mut at = 0usize;
    for needle in [
        "Title",
        "First paragraph with bold, italic and code.",
        "Section",
        "- alpha item",
        "- beta item",
        "Steps",
        "1. one",
        "2. two",
        "fn main() {}",
        "let x = 1;",
        "Last paragraph.",
    ] {
        let pos = text[at..].find(needle).unwrap_or_else(|| panic!("{needle:?} missing or out of order in:\n{text}"));
        at += pos + needle.len();
    }
}

#[test]
fn a_long_pdf_has_more_than_one_page() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("long.pdf");
    let mut md = String::new();
    for i in 1..=300 {
        md.push_str(&format!("Line {i} of the long document.\n\n"));
    }
    run(&reg, &mut ctx, "write", json!({"filePath": p.to_str().unwrap(), "content": md})).unwrap();
    let doc = lopdf::Document::load(&p).unwrap();
    let pages = doc.get_pages().len();
    assert!(pages > 1, "300 paragraphs on {pages} page(s)");
    // Every page draws text: the last paragraph is on the last page, not
    // lost off the bottom of the first.
    let text = run(&reg, &mut ctx, "read", json!({"filePath": p.to_str().unwrap(), "limit": 2000})).unwrap().output;
    assert!(text.contains("Line 300 of the long document."), "{text}");
}

#[test]
fn characters_outside_winansi_become_question_marks_and_are_counted() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("chars.pdf");
    // The em dash and the curly apostrophe ARE WinAnsi (0x97, 0x92) and
    // must survive; the CJK character and the check mark are not.
    let body = "Rollout \u{2014} it\u{2019}s done \u{6f22} \u{2713}\n";
    let out = run(&reg, &mut ctx, "write", json!({"filePath": p.to_str().unwrap(), "content": body})).unwrap();
    assert!(out.output.ends_with(", 2 characters outside WinAnsi replaced)"), "{}", out.output);
    let text = run(&reg, &mut ctx, "read", json!({"filePath": p.to_str().unwrap()})).unwrap().output;
    assert!(text.contains("Rollout \u{2014} it\u{2019}s done ? ?"), "{text}");

    // Pure ASCII reports nothing.
    let q = dir.path().join("ascii.pdf");
    let out = run(&reg, &mut ctx, "write", json!({"filePath": q.to_str().unwrap(), "content": "plain\n"})).unwrap();
    assert!(!out.output.contains("replaced"), "{}", out.output);
}

#[test]
fn a_guarded_pdf_path_is_refused_before_anything_is_written() {
    let (dir, key, _normal, mut ctx) = guarded_ctx();
    let target = dir.path().join("secrets").join("summary.pdf");
    ctx.guard = temur::tools::KeyGuard::from_paths(vec![key, target.clone()]);
    let reg = Registry::standard();
    let err = run(
        &reg,
        &mut ctx,
        "write",
        json!({"filePath": target.to_str().unwrap(), "content": "# summary\n"}),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("key isolation"), "{err}");
    assert!(!target.exists(), "nothing may be created under a guarded path");
}

#[test]
fn the_write_description_says_what_it_can_write() {
    for profile in [PromptProfile::Full, PromptProfile::Compact] {
        let defs = Registry::standard().with_profile(profile).definitions();
        let d = defs.iter().find(|d| d.name == "write").unwrap();
        assert!(d.description.contains(".docx or .pdf path takes Markdown"), "{}", d.description);
        assert!(!d.description.contains("Never create binary formats"), "{}", d.description);
    }
}

// --- T60 P2b: a failed converter and a misnamed workbook point at write -----

const NUDGE: &str = "No converter is installed here. The write tool writes a .pdf or .docx from Markdown";

#[test]
fn a_missing_converter_names_the_write_tool() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    // Case 1: a document named and the shell's 127.
    let out = run(&reg, &mut ctx, "bash", json!({"command": "pdftk x output y.pdf"})).unwrap();
    assert!(out.output.contains("(exit code 127)"), "{}", out.output);
    assert_eq!(out.output.matches(NUDGE).count(), 1, "{}", out.output);
    // Case 1, the install shape: the install fails and the command
    // names the document, so it fires whatever the exit code.
    let out = run(&reg, &mut ctx, "bash", json!({"command": "pip3 install fpdf2 && python3 -c 'open(\"out.pdf\")'"})).unwrap();
    assert!(!out.output.contains("exit code 0"), "{}", out.output);
    assert_eq!(out.output.matches(NUDGE).count(), 1, "{}", out.output);
    // A failed install that names no document is any failed install.
    let out = run(&reg, &mut ctx, "bash", json!({"command": "pip3 install fpdf2"})).unwrap();
    assert!(!out.output.contains("exit code 0"), "{}", out.output);
    assert!(!out.output.contains(NUDGE), "{}", out.output);
    // 127 without a document name is any typo, not a converter hunt.
    let out = run(&reg, &mut ctx, "bash", json!({"command": "nosuchcmd"})).unwrap();
    assert!(!out.output.contains(NUDGE), "{}", out.output);
    // A document named but a different failure, and no install word.
    let out = run(&reg, &mut ctx, "bash", json!({"command": "ls missing.pdf"})).unwrap();
    assert!(!out.output.contains("exit code 0"), "{}", out.output);
    assert!(!out.output.contains(NUDGE), "{}", out.output);
}

#[test]
fn text_redirected_into_a_document_name_names_the_write_tool() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    // Case 2: exit 0, plain text under the .pdf name.
    let out = run(&reg, &mut ctx, "bash", json!({"command": "echo hi > out.pdf"})).unwrap();
    let expected = format!("{} is not a PDF: it starts with plain text. {}", dir.path().join("out.pdf").display(), NUDGE);
    assert!(out.output.contains(&expected), "{}", out.output);
    assert_eq!(out.output.matches(NUDGE).count(), 1, "{}", out.output);
    // And under a .docx name, through a quoted heredoc-style redirect.
    let out = run(&reg, &mut ctx, "bash", json!({"command": "printf 'memo' >\"memo.docx\""})).unwrap();
    assert!(out.output.contains("memo.docx is not a Word document"), "{}", out.output);
    // A real PDF from the write tool, copied: the header is genuine.
    let a = dir.path().join("a.pdf");
    run(&reg, &mut ctx, "write", json!({"filePath": a.to_str().unwrap(), "content": "# real\n"})).unwrap();
    let out = run(&reg, &mut ctx, "bash", json!({"command": "cp a.pdf b.pdf"})).unwrap();
    assert!(!out.output.contains(NUDGE), "{}", out.output);
    // A file that existed before the command is not blamed on it. The
    // mtime check allows 100 ms for the kernel's coarse timestamps, so
    // the file has to be older than that when the command starts.
    std::thread::sleep(std::time::Duration::from_millis(250));
    let out = run(&reg, &mut ctx, "bash", json!({"command": "ls out.pdf"})).unwrap();
    assert!(!out.output.contains(NUDGE), "{}", out.output);
    // Once, even when a redirect and a 127 could both match.
    let out = run(&reg, &mut ctx, "bash", json!({"command": "echo hi > again.pdf; nosuchconverter again.pdf"})).unwrap();
    assert!(out.output.contains("(exit code 127)"), "{}", out.output);
    assert_eq!(out.output.matches(NUDGE).count(), 1, "{}", out.output);
}

#[test]
fn the_spreadsheet_tool_refuses_a_path_that_is_not_a_workbook() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("summary.pdf");
    let err = run(&reg, &mut ctx, "spreadsheet", json!({
        "filePath": p.to_str().unwrap(), "sheets": [{"name": "S", "rows": [[1]]}]
    })).unwrap_err().to_string();
    assert!(err.contains("writes .xlsx workbooks only"), "{err}");
    assert!(err.contains("call write with the document path and Markdown content"), "{err}");
    assert!(!p.exists(), "nothing may be created under a refused path");
    // Before the guard: a guarded non-workbook path gets this sentence,
    // not the key-isolation one, and still nothing exists.
    let (gdir, key, _normal, mut gctx) = guarded_ctx();
    let target = gdir.path().join("secrets").join("book.docx");
    gctx.guard = temur::tools::KeyGuard::from_paths(vec![key, target.clone()]);
    let err = run(&reg, &mut gctx, "spreadsheet", json!({
        "filePath": target.to_str().unwrap(), "sheets": [{"name": "S", "rows": [[1]]}]
    })).unwrap_err().to_string();
    assert!(err.contains("writes .xlsx workbooks only"), "{err}");
    assert!(!target.exists());
}

// --- T54 P3: charts, the one new surface (D23) -----------------------------

#[test]
fn the_d23_request_end_to_end() {
    // "make an excel file, then create a chart mapping every x,y integer
    // on f(x) = x^2 - 2x + 3 from 0 to 10" -- the dogfood request that
    // dead-ended on a missing libreoffice.
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("fx.xlsx");

    let mut rows = vec![json!(["x", "f(x)"])];
    for x in 0..=10i64 {
        rows.push(json!([x, x * x - 2 * x + 3]));
    }
    let out = run(
        &reg,
        &mut ctx,
        "spreadsheet",
        json!({
            "filePath": p.to_str().unwrap(),
            "sheets": [{"name": "Data", "rows": rows}],
            "charts": [{
                "sheet": "Data", "chartType": "line", "title": "f(x) = x^2 - 2x + 3",
                "categories": "A2:A12",
                "series": [{"name": "f(x)", "values": "B2:B12"}],
                "anchor": "D2"
            }]
        }),
    )
    .unwrap();
    assert!(out.output.contains("1 sheet, 1 chart"), "{}", out.output);

    // Read back through P1: the values are right and no formula appears.
    let back = run(&reg, &mut ctx, "read", json!({"filePath": p.to_str().unwrap()})).unwrap();
    assert!(back.output.contains("== Sheet: Data =="), "{}", back.output);
    for (x, y) in (0..=10i64).map(|x| (x, x * x - 2 * x + 3)) {
        assert!(back.output.contains(&format!("{x},{y}")), "missing {x},{y}: {}", back.output);
    }

    // The chart part is really in the workbook.
    let f = std::fs::File::open(&p).unwrap();
    let mut zip = zip::ZipArchive::new(f).unwrap();
    let names: Vec<String> = (0..zip.len()).map(|i| zip.by_index(i).unwrap().name().to_string()).collect();
    assert!(names.iter().any(|n| n == "xl/charts/chart1.xml"), "{names:?}");
    let mut xml = String::new();
    {
        use std::io::Read;
        zip.by_name("xl/charts/chart1.xml").unwrap().read_to_string(&mut xml).unwrap();
    }
    assert!(xml.contains("<c:lineChart>"), "chart type in xml");
    assert!(xml.contains("f(x)"), "series name in xml");
}

#[test]
fn multiple_sheets_and_charts_in_one_call() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("multi.xlsx");
    let out = run(
        &reg,
        &mut ctx,
        "spreadsheet",
        json!({
            "filePath": p.to_str().unwrap(),
            "sheets": [
                {"name": "A", "rows": [["n", "v"], [1, 10], [2, 20]]},
                {"name": "B", "rows": [["n", "v"], [1, 5], [2, 6]]}
            ],
            "charts": [
                {"sheet": "A", "chartType": "column", "series": [{"values": "B2:B3"}]},
                {"sheet": "B", "chartType": "pie", "series": [{"values": "B2:B3"}]}
            ]
        }),
    )
    .unwrap();
    assert!(out.output.contains("2 sheets, 2 charts"), "{}", out.output);
    let back = run(&reg, &mut ctx, "read", json!({"filePath": p.to_str().unwrap()})).unwrap();
    assert!(back.output.contains("== Sheet: A =="), "{}", back.output);
    assert!(back.output.contains("== Sheet: B =="), "{}", back.output);
}

#[test]
fn an_out_of_range_series_is_an_error_naming_the_range() {
    // Never a silently empty chart: an empty frame looks like a temur bug
    // to the user and tells the model nothing.
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("bad.xlsx");
    let err = run(
        &reg,
        &mut ctx,
        "spreadsheet",
        json!({
            "filePath": p.to_str().unwrap(),
            "sheets": [{"name": "Data", "rows": [["n"], [1], [2]]}],
            "charts": [{"sheet": "Data", "chartType": "line", "series": [{"values": "B2:B99"}]}]
        }),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("B2:B99"), "names the range: {err}");
    assert!(err.contains("Data"), "names the sheet: {err}");
    assert!(!p.exists(), "nothing is written when a range is wrong");
}

#[test]
fn bad_chart_input_is_rejected_with_the_accepted_forms() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("x.xlsx");
    let base = |charts: serde_json::Value| {
        json!({
            "filePath": p.to_str().unwrap(),
            "sheets": [{"name": "Data", "rows": [["n"], [1]]}],
            "charts": charts
        })
    };
    let err = run(&reg, &mut ctx, "spreadsheet",
        base(json!([{"sheet": "Data", "chartType": "donut", "series": [{"values": "A1:A1"}]}])))
        .unwrap_err().to_string();
    assert!(err.contains("line, column, bar, scatter, pie"), "{err}");

    let err = run(&reg, &mut ctx, "spreadsheet",
        base(json!([{"sheet": "Data", "chartType": "line", "series": [{"values": "not-a-range"}]}])))
        .unwrap_err().to_string();
    assert!(err.contains("A1 range"), "{err}");

    let err = run(&reg, &mut ctx, "spreadsheet",
        base(json!([{"sheet": "Nope", "chartType": "line", "series": [{"values": "A1:A1"}]}])))
        .unwrap_err().to_string();
    assert!(err.contains("Nope"), "{err}");
}

#[test]
fn the_spreadsheet_tool_keeps_the_same_file_rules() {
    let (dir, key, _normal, mut ctx) = guarded_ctx();
    let target = dir.path().join("secrets").join("book.xlsx");
    ctx.guard = temur::tools::KeyGuard::from_paths(vec![key, target.clone()]);
    let reg = Registry::standard();
    let err = run(&reg, &mut ctx, "spreadsheet", json!({
        "filePath": target.to_str().unwrap(),
        "sheets": [{"name": "S", "rows": [[1]]}]
    })).unwrap_err().to_string();
    assert!(err.contains("key isolation"), "{err}");
    assert!(!target.exists());

    // Read-first governs an existing workbook in a fresh session.
    let plain = tempfile::tempdir().unwrap();
    let p = plain.path().join("r.xlsx");
    let mut c1 = ctx_in(plain.path());
    run(&reg, &mut c1, "spreadsheet", json!({
        "filePath": p.to_str().unwrap(), "sheets": [{"name": "S", "rows": [[1]]}]
    })).unwrap();
    let mut c2 = ctx_in(plain.path());
    let err = run(&reg, &mut c2, "spreadsheet", json!({
        "filePath": p.to_str().unwrap(), "sheets": [{"name": "S", "rows": [[2]]}]
    })).unwrap_err().to_string();
    assert!(err.contains("has not been read in this session"), "{err}");

    // T60 P2b: an .xlsx write is what it always was, whatever the case of
    // the extension, and the workbook reads back.
    let upper = plain.path().join("R2.XLSX");
    run(&reg, &mut c1, "spreadsheet", json!({
        "filePath": upper.to_str().unwrap(), "sheets": [{"name": "S", "rows": [["k", 7]]}]
    })).unwrap();
    let out = run(&reg, &mut c1, "read", json!({"filePath": upper.to_str().unwrap()})).unwrap();
    assert!(out.output.contains("k,7"), "{}", out.output);
}

#[test]
fn a1_parsing_covers_the_shapes_a_model_writes() {
    use temur::tools::office_a1 as a1;
    assert_eq!(a1::cell("A1"), Some((0, 0)));
    assert_eq!(a1::cell("D2"), Some((1, 3)));
    assert_eq!(a1::cell("Z10"), Some((9, 25)));
    assert_eq!(a1::cell("AA1"), Some((0, 26)));
    assert_eq!(a1::cell("$B$7"), Some((6, 1)));
    assert_eq!(a1::cell("1A"), None);
    assert_eq!(a1::cell(""), None);
    assert_eq!(a1::range("A2:A12"), Some((1, 0, 11, 0)));
    // Reversed corners normalize rather than erroring.
    assert_eq!(a1::range("A12:A2"), Some((1, 0, 11, 0)));
    assert_eq!(a1::range("B7"), Some((6, 1, 6, 1)));
    assert_eq!(a1::range("nope"), None);
}

#[test]
fn the_new_tool_is_served_in_both_profiles() {
    // Registration follows the existing rule: the compact profile trims
    // DESCRIPTIONS, never the tool set, so every profile can do everything.
    for profile in [PromptProfile::Full, PromptProfile::Compact] {
        let defs = Registry::standard().with_profile(profile).definitions();
        let d = defs.iter().find(|d| d.name == "spreadsheet").expect("registered");
        assert!(!d.description.is_empty());
        // It must say WHEN to prefer it over write, or it will be reached
        // for whenever a spreadsheet is mentioned.
        assert!(d.description.contains("write tool"), "{}", d.description);
        assert!(d.description.to_lowercase().contains("chart"), "{}", d.description);
    }
}

#[test]
fn the_obvious_spelling_of_chart_type_is_still_accepted() {
    // T33: tolerance at the argument boundary, not in the declared schema.
    // The schema says chartType (T34: no property named "type"), but a
    // model that writes "type" is understood rather than refused.
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("alias.xlsx");
    run(&reg, &mut ctx, "spreadsheet", json!({
        "filePath": p.to_str().unwrap(),
        "sheets": [{"name": "D", "rows": [["n"], [1], [2]]}],
        "charts": [{"sheet": "D", "type": "line", "series": [{"values": "A2:A3"}]}]
    })).unwrap();
    assert!(p.exists());
}

// --- T60 P0: the eval's read-back hook ------------------------------------

/// Not an assertion: scripts/weak_model_eval.sh's host-side scorer for
/// tasks 11 and 12 (memo-docx, summary-pdf). With TEMUR_EVAL_READBACK set
/// to a path, this runs the read tool on it and prints the tool's output,
/// or its error, between two marker lines, so the eval can judge a
/// document the model wrote through temur's own docx and PDF parsers (the
/// ones compiled into the binary under test) rather than through a host
/// tool the box may not have. Unset, which is every ordinary test run, it
/// does nothing and passes.
#[test]
fn eval_read_back() {
    let path = match std::env::var_os("TEMUR_EVAL_READBACK") {
        Some(p) => std::path::PathBuf::from(p),
        None => return,
    };
    let reg = Registry::standard();
    let root = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let mut ctx = ctx_in(&root);
    // An optional window, so a caller can page the whole document instead of
    // only the default first screen. The read tool's byte cap fires at any
    // limit, so proving two files extract to the same TEXT takes more than one
    // call; T62 Ruling T62-4 preflight (ii) needs the pages past the first.
    let mut input = json!({"filePath": path.to_string_lossy()});
    if let Some(v) = std::env::var_os("TEMUR_EVAL_READBACK_OFFSET") {
        if let Ok(n) = v.to_string_lossy().parse::<u64>() {
            input["offset"] = json!(n);
        }
    }
    let res = run(&reg, &mut ctx, "read", input);
    println!("READBACK-BEGIN");
    match res {
        Ok(out) => println!("{}", out.output),
        Err(e) => println!("ERROR: {e}"),
    }
    println!("READBACK-END");
}

/// T62 P1: the same hook in the other direction, so the eval can build task
/// 13's fixture through temur's OWN write path.
///
/// Why a test hook rather than the binary: `write` is a tool, reachable only
/// from a model turn, and a model cannot produce an exact fixture. This is
/// the mechanism `eval_read_back` already established and that tasks 11 and
/// 12 are already scored by, so the eval already trusts it: the same code,
/// compiled for the same target, run in the same container image.
///
/// Does nothing at all unless both env vars are set, so it is inert in every
/// ordinary run of the suite.
#[test]
fn eval_write_pdf() {
    let (src, dst) = match (
        std::env::var_os("TEMUR_EVAL_WRITE_SRC"),
        std::env::var_os("TEMUR_EVAL_WRITE_PDF"),
    ) {
        (Some(a), Some(b)) => (std::path::PathBuf::from(a), std::path::PathBuf::from(b)),
        _ => return,
    };
    let body = std::fs::read_to_string(&src).expect("markdown source");
    let reg = Registry::standard();
    let root = dst.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let mut ctx = ctx_in(&root);
    let res = run(
        &reg,
        &mut ctx,
        "write",
        json!({"filePath": dst.to_string_lossy(), "content": body}),
    );
    println!("WRITEPDF-BEGIN");
    match res {
        Ok(out) => println!("{}", out.output),
        Err(e) => println!("ERROR: {e}"),
    }
    println!("WRITEPDF-END");
}

/// T63 P4 (c): extracted PDF text wraps a long sentence at the writer's line
/// width, and a hit on the first half used to show only that half (task 13
/// quoted "which the board has approved." from a line ending "which the
/// board"). A prose document hit now carries the next extracted line, once,
/// at the hit's own line number. A raw text file is unchanged.
#[test]
fn a_document_hit_carries_the_rest_of_a_wrapped_sentence() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let sentence = "The Kestrel-class hull survey is deferred to the 2027 dry-dock window, which the board accepted on the understanding that the interim checks continue.";
    let pdf = dir.path().join("wrapped.pdf");
    run(&reg, &mut ctx, "write", json!({"filePath": pdf.to_str().unwrap(), "content": sentence})).unwrap();
    std::fs::write(
        dir.path().join("wrapped.txt"),
        "The Kestrel note, which the board\naccepted later.\n",
    )
    .unwrap();

    let out = run(&reg, &mut ctx, "grep", json!({"pattern": "Kestrel"})).unwrap();
    let pdf_hit = out.output.lines().find(|l| l.contains("wrapped.pdf:")).expect("a pdf hit");
    assert!(
        pdf_hit.contains("Kestrel-class") && pdf_hit.contains("accepted on the understanding"),
        "the hit must carry the continuation: {}",
        out.output
    );
    let txt_hit = out.output.lines().find(|l| l.contains("wrapped.txt:")).expect("a txt hit");
    assert!(txt_hit.ends_with("which the board"), "a raw file is unchanged: {}", out.output);

    // The number is still the hit's own line: read at that offset shows it.
    let n: u64 = pdf_hit
        .split(':')
        .nth(1)
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or_else(|| panic!("no line number in {pdf_hit}"));
    let back = run(
        &reg,
        &mut ctx,
        "read",
        json!({"filePath": pdf.to_str().unwrap(), "offset": n, "limit": 1}),
    )
    .unwrap();
    assert!(back.output.contains("Kestrel-class"), "{}", back.output);
}

/// T63 P2a, the EDIT IDEMPOTENCY GUARD (named so; the edit tool's own "F3"
/// is the indentation-delta feature). The repro from the spec: an edit whose
/// newString extends oldString in place, sent twice, appended twice, because
/// oldString still matches inside the already-edited line.
#[test]
fn an_already_applied_edit_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("cart.py");
    std::fs::write(
        &f,
        "def total(items):\n    total = 0\n    for item in items:\n        total += item[\"qty\"]\n    return total\n",
    )
    .unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let old = "        total += item[\"qty\"]";
    let new = format!("{old} * item[\"unit_price\"]");
    let args = json!({"filePath": f.to_str().unwrap(), "oldString": old, "newString": new});

    run(&reg, &mut ctx, "edit", args.clone()).unwrap();
    let once = std::fs::read_to_string(&f).unwrap();
    assert_eq!(once.matches("unit_price").count(), 1, "{once}");

    let second = run(&reg, &mut ctx, "edit", args);
    let after = std::fs::read_to_string(&f).unwrap();
    assert_eq!(after, once, "the second identical edit must not change the file:\n{after}");
    let err = second.expect_err("the second identical edit must be refused");
    assert!(err.to_string().contains("this edit looks already applied"), "{err}");
}

/// T63 P2a: a wrap edit, where newString contains oldString but is not yet
/// in the file, still applies. Resending it is refused, and replaceAll
/// refuses when any site is already applied rather than applying it twice.
#[test]
fn a_wrap_edit_applies_once_and_replace_all_never_double_applies() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("w.js");
    std::fs::write(&f, "foo();\n").unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let wrap = json!({"filePath": f.to_str().unwrap(), "oldString": "foo()", "newString": "try { foo() }"});
    run(&reg, &mut ctx, "edit", wrap.clone()).unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "try { foo() };\n");
    let again = run(&reg, &mut ctx, "edit", wrap).expect_err("the resent wrap must be refused");
    assert!(again.to_string().contains("this edit looks already applied"), "{again}");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "try { foo() };\n");

    // replaceAll with one site already applied and one fresh: refused, file untouched.
    let g = dir.path().join("r.txt");
    std::fs::write(&g, "a = 1 + tax\nb = 1\n").unwrap();
    let all = json!({"filePath": g.to_str().unwrap(), "oldString": "1", "newString": "1 + tax", "replaceAll": true});
    let refused = run(&reg, &mut ctx, "edit", all).expect_err("an applied site must refuse replaceAll");
    assert!(refused.to_string().contains("this edit looks already applied"), "{refused}");
    assert!(
        refused.to_string().contains("With replaceAll, 1 of 2 match sites already carry newString, so nothing was changed."),
        "a mixed replaceAll says how many sites are done: {refused}"
    );
    assert_eq!(std::fs::read_to_string(&g).unwrap(), "a = 1 + tax\nb = 1\n");
}

/// T63 P2b (b): an identical read of an unchanged file says so on its first
/// line and still returns the content. A changed file, a different window,
/// and a directory read normally.
#[test]
fn a_repeated_identical_read_is_marked_unchanged() {
    const MARK: &str = "[unchanged: identical to your previous read of this file]";
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("notes.txt");
    std::fs::write(&f, "one\ntwo\nthree\n").unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let args = json!({"filePath": f.to_str().unwrap()});

    let first = run(&reg, &mut ctx, "read", args.clone()).unwrap().output;
    assert!(!first.contains(MARK), "{first}");
    let second = run(&reg, &mut ctx, "read", args.clone()).unwrap().output;
    assert!(second.starts_with(MARK), "{second}");
    assert!(second.contains("2: two"), "the content is still returned: {second}");

    // Same window, changed file: not identical.
    std::fs::write(&f, "one\ntwo\nthree\nfour\n").unwrap();
    let changed = run(&reg, &mut ctx, "read", args).unwrap().output;
    assert!(!changed.contains(MARK), "{changed}");
    assert!(changed.contains("4: four"), "{changed}");

    // Same file, different window: not identical.
    let windowed =
        run(&reg, &mut ctx, "read", json!({"filePath": f.to_str().unwrap(), "offset": 2})).unwrap().output;
    assert!(!windowed.contains(MARK), "{windowed}");

    // A directory listing is never marked.
    let dir_args = json!({"filePath": dir.path().to_str().unwrap()});
    run(&reg, &mut ctx, "read", dir_args.clone()).unwrap();
    let dir_again = run(&reg, &mut ctx, "read", dir_args).unwrap().output;
    assert!(!dir_again.contains(MARK), "{dir_again}");
}

/// T63 P2b (a): an edit whose newString equals oldString says there is
/// nothing to change and to move on.
#[test]
fn an_edit_with_nothing_to_change_says_to_continue() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("same.txt");
    std::fs::write(&f, "x = 1\n").unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let err = run(
        &reg,
        &mut ctx,
        "edit",
        json!({"filePath": f.to_str().unwrap(), "oldString": "x = 1", "newString": "x = 1"}),
    )
    .expect_err("an edit with nothing to change must be refused");
    assert!(
        err.to_string().contains(
            "newString equals oldString: there is nothing to change. If the file already reads the way you want, do not edit it again; continue with the task."
        ),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "x = 1\n");
}

// --------------------------------------------------------- T64 P0a
// F9 and F10: edit says why it cannot change a file, and a write over an
// existing document keeps the previous one beside it.

#[test]
fn edit_names_the_real_reason_it_cannot_read_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let edit = |ctx: &mut ToolCtx, p: &std::path::Path| {
        run(&reg, ctx, "edit", json!({"filePath": p.to_str().unwrap(), "oldString": "a", "newString": "b"}))
            .unwrap_err()
            .to_string()
    };

    // Missing is the only case that says File not found.
    let err = edit(&mut ctx, &dir.path().join("nope.txt"));
    assert!(err.contains("File not found"), "{err}");

    // Latin-1 text: it exists, and decoded it would even match oldString.
    let latin1 = dir.path().join("latin1.txt");
    std::fs::write(&latin1, b"caf\xe9 a\n").unwrap();
    let err = edit(&mut ctx, &latin1);
    assert!(err.contains("is not UTF-8 text"), "{err}");
    assert!(!err.contains("File not found"), "{err}");
    assert_eq!(std::fs::read(&latin1).unwrap(), b"caf\xe9 a\n");

    // Any other failure reports the io error itself.
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let err = edit(&mut ctx, &sub);
    assert!(err.contains("Cannot read"), "{err}");
    assert!(!err.contains("File not found"), "{err}");

    // An edit that read nothing does not arm write's read-first rule.
    let err = run(&reg, &mut ctx, "write", json!({"filePath": latin1.to_str().unwrap(), "content": "x"}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("has not been read in this session"), "{err}");
}

#[test]
fn edit_on_a_document_says_what_temur_can_do_instead() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let book = dir.path().join("book.xlsx");
    run(&reg, &mut ctx, "write", json!({"filePath": book.to_str().unwrap(), "content": "a,1\n"})).unwrap();
    let notes = dir.path().join("notes.docx");
    std::fs::copy(office_fixture("synth.docx"), &notes).unwrap();
    // Upper case: write decides by the lowercased extension, so edit does too.
    let report = dir.path().join("report.PDF");
    std::fs::copy(office_fixture("synth-plain.pdf"), &report).unwrap();

    for p in [&book, &notes, &report] {
        let before = std::fs::read(p).unwrap();
        let err = run(&reg, &mut ctx, "edit", json!({"filePath": p.to_str().unwrap(), "oldString": "a", "newString": "b"}))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("temur cannot edit documents in place; read shows their text; write replaces the whole document with one built from your content"),
            "{err}"
        );
        assert_eq!(std::fs::read(p).unwrap(), before, "{}", p.display());
    }

    // The refusal read nothing, so it does not arm write's read-first rule.
    let err = run(&reg, &mut ctx, "write", json!({"filePath": notes.to_str().unwrap(), "content": "# x\n"}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("has not been read in this session"), "{err}");
}

#[test]
fn a_write_over_a_document_keeps_the_previous_one_beside_it() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    for (name, previous, v1, v2, v3) in [
        ("book.xlsx", "book.previous.xlsx", "a,1\n", "b,2\n", "c,3\n"),
        ("notes.docx", "notes.previous.docx", "# One\n", "# Two\n", "# Three\n"),
        ("report.pdf", "report.previous.pdf", "one\n", "two\n", "three\n"),
    ] {
        let p = dir.path().join(name);
        let fp = p.to_str().unwrap();
        let prev = dir.path().join(previous);

        // A new document moves nothing aside.
        let out = run(&reg, &mut ctx, "write", json!({"filePath": fp, "content": v1})).unwrap();
        assert!(out.output.starts_with("Created "), "{}", out.output);
        assert!(!prev.exists(), "{name}");
        let first = std::fs::read(&p).unwrap();

        // Read-first still governs a document in a fresh session, and a
        // refused write moves nothing.
        let mut fresh = ctx_in(dir.path());
        let err = run(&reg, &mut fresh, "write", json!({"filePath": fp, "content": v2}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("has not been read in this session"), "{err}");
        assert!(!prev.exists(), "{name}");
        assert_eq!(std::fs::read(&p).unwrap(), first, "{name}");

        // A read through office::extract arms the write, and the write keeps
        // the old bytes under the .previous name, byte-identical.
        run(&reg, &mut fresh, "read", json!({"filePath": fp})).unwrap();
        let out = run(&reg, &mut fresh, "write", json!({"filePath": fp, "content": v2})).unwrap();
        assert_eq!(std::fs::read(&prev).unwrap(), first, "{name}");
        assert!(out.output.starts_with(&format!("Replaced {} (", p.display())), "{}", out.output);
        assert!(
            out.output.ends_with(&format!("; the previous document is at {}", prev.display())),
            "{}",
            out.output
        );

        // T65 P1: the third write is over temur's own output, so nothing
        // rotates: the copy still holds the version temur did not write,
        // and the result line says the original is still there.
        let out = run(&reg, &mut fresh, "write", json!({"filePath": fp, "content": v3})).unwrap();
        assert_eq!(std::fs::read(&prev).unwrap(), first, "{name}");
        assert!(
            out.output.ends_with(&format!("; the original is still at {}", prev.display())),
            "{}",
            out.output
        );
    }
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["book.previous.xlsx", "book.xlsx", "notes.docx", "notes.previous.docx", "report.pdf", "report.previous.pdf"]
    );
}

#[test]
fn a_document_write_that_fails_puts_the_previous_document_back() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("wide.xlsx");
    let fp = p.to_str().unwrap();
    run(&reg, &mut ctx, "write", json!({"filePath": fp, "content": "a,1\n"})).unwrap();
    let before = std::fs::read(&p).unwrap();
    // A stale copy from an earlier write. The move aside replaces it, so
    // its absence afterwards proves the move happened before the writer
    // failed; this is also the rollback's stated limit.
    let prev = dir.path().join("wide.previous.xlsx");
    std::fs::write(&prev, "stale").unwrap();

    // 16,385 fields: one past Excel's column limit, which the workbook
    // writer refuses after the existing file has been moved aside.
    let too_wide = format!("{}\n", ",".repeat(16_384));
    // T65 P1: move aside, and so put_back, is the path for a document that
    // is NOT temur's own last write. A fresh session that has read the file
    // is that shape, and is what a user meets after `--continue`.
    let mut fresh = ctx_in(dir.path());
    run(&reg, &mut fresh, "read", json!({"filePath": fp})).unwrap();
    let err = run(&reg, &mut fresh, "write", json!({"filePath": fp, "content": too_wide}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("more rows or columns than a worksheet can hold"), "{err}");
    assert_eq!(std::fs::read(&p).unwrap(), before);
    assert!(!prev.exists());
}

/// T65 P1 (2), CONTROL: temur's own last write is the only thing it
/// writes over directly. Anything else changed the file, so the file is
/// the user's newest version and moves aside as it always did.
#[test]
fn a_document_changed_outside_temur_moves_aside_again() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("notes.docx");
    let fp = p.to_str().unwrap();
    let prev = dir.path().join("notes.previous.docx");

    run(&reg, &mut ctx, "write", json!({"filePath": fp, "content": "# One\n"})).unwrap();
    let original = std::fs::read(&p).unwrap();

    let mut fresh = ctx_in(dir.path());
    run(&reg, &mut fresh, "read", json!({"filePath": fp})).unwrap();
    run(&reg, &mut fresh, "write", json!({"filePath": fp, "content": "# Two\n"})).unwrap();
    assert_eq!(std::fs::read(&prev).unwrap(), original);

    // Somebody else saves over the document. A DIFFERENT length, so the
    // stamp differs whatever the filesystem's mtime granularity is.
    let outside = b"saved from Word, and a different length entirely".to_vec();
    std::fs::write(&p, &outside).unwrap();

    let out = run(&reg, &mut fresh, "write", json!({"filePath": fp, "content": "# Three\n"})).unwrap();
    assert_eq!(std::fs::read(&prev).unwrap(), outside);
    assert!(
        out.output.ends_with(&format!("; the previous document is at {}", prev.display())),
        "{}",
        out.output
    );
}

/// T65 P1 (3): the copy is the user's to delete. temur's own writes never
/// put it back, and the result line then has no tail at all.
#[test]
fn a_deleted_previous_copy_is_not_recreated() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("report.pdf");
    let fp = p.to_str().unwrap();
    let prev = dir.path().join("report.previous.pdf");

    run(&reg, &mut ctx, "write", json!({"filePath": fp, "content": "one\n"})).unwrap();
    let mut fresh = ctx_in(dir.path());
    run(&reg, &mut fresh, "read", json!({"filePath": fp})).unwrap();
    run(&reg, &mut fresh, "write", json!({"filePath": fp, "content": "two\n"})).unwrap();
    assert!(prev.exists());
    std::fs::remove_file(&prev).unwrap();

    let out = run(&reg, &mut fresh, "write", json!({"filePath": fp, "content": "three\n"})).unwrap();
    assert!(!prev.exists());
    let n = std::fs::metadata(&p).unwrap().len();
    assert_eq!(out.output, format!("Replaced {} ({n} bytes)", p.display()));
}

/// T65 P1 (5): a failed write over temur's own output rotates nothing, so
/// the user's document is still beside it and the retry lands directly on
/// whatever the failed writer left.
#[test]
fn a_failed_write_over_temurs_own_document_keeps_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("wide.xlsx");
    let fp = p.to_str().unwrap();
    let prev = dir.path().join("wide.previous.xlsx");

    run(&reg, &mut ctx, "write", json!({"filePath": fp, "content": "a,1\n"})).unwrap();
    let original = std::fs::read(&p).unwrap();

    let mut fresh = ctx_in(dir.path());
    run(&reg, &mut fresh, "read", json!({"filePath": fp})).unwrap();
    run(&reg, &mut fresh, "write", json!({"filePath": fp, "content": "b,2\n"})).unwrap();
    assert_eq!(std::fs::read(&prev).unwrap(), original);

    // 16,385 fields: one past Excel's column limit, the same failure the
    // put_back test uses.
    let too_wide = format!("{}\n", ",".repeat(16_384));
    let err = run(&reg, &mut fresh, "write", json!({"filePath": fp, "content": too_wide}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("more rows or columns than a worksheet can hold"), "{err}");
    // Before T65 P1 the failed write had already rotated the original away
    // and put_back left nothing here.
    assert_eq!(std::fs::read(&prev).unwrap(), original);

    let out = run(&reg, &mut fresh, "write", json!({"filePath": fp, "content": "c,3\n"})).unwrap();
    assert_eq!(std::fs::read(&prev).unwrap(), original);
    assert!(
        out.output.ends_with(&format!("; the original is still at {}", prev.display())),
        "{}",
        out.output
    );
}

#[test]
fn the_spreadsheet_tool_keeps_the_previous_workbook_beside_it() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("plot.xlsx");
    let prev = dir.path().join("plot.previous.xlsx");
    let book = |label: &str| {
        json!({"filePath": p.to_str().unwrap(), "sheets": [{"name": "Data", "rows": [["label", label]]}], "charts": []})
    };

    let out = run(&reg, &mut ctx, "spreadsheet", book("one")).unwrap();
    assert!(out.output.starts_with("Created "), "{}", out.output);
    assert!(!prev.exists());
    let first = std::fs::read(&p).unwrap();

    // T65 P1: the workbook a later session finds is not that session's own
    // write, so it moves aside exactly as it did under T64 P0a.
    let mut fresh = ctx_in(dir.path());
    run(&reg, &mut fresh, "read", json!({"filePath": p.to_str().unwrap()})).unwrap();
    let out = run(&reg, &mut fresh, "spreadsheet", book("two")).unwrap();
    assert_eq!(std::fs::read(&prev).unwrap(), first);
    assert!(out.output.starts_with(&format!("Replaced {} (1 sheet, 0 charts, ", p.display())), "{}", out.output);
    assert!(
        out.output.ends_with(&format!("; the previous document is at {}", prev.display())),
        "{}",
        out.output
    );

    // T65 P1: a third call is over temur's own workbook, so the copy still
    // holds the first one and the result line says so.
    let out = run(&reg, &mut fresh, "spreadsheet", book("three")).unwrap();
    assert_eq!(std::fs::read(&prev).unwrap(), first);
    assert!(
        out.output.ends_with(&format!("; the original is still at {}", prev.display())),
        "{}",
        out.output
    );
}

#[test]
fn plain_text_over_an_ods_keeps_the_workbook_beside_it() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    // Upper case: the rule ignores case and the copy keeps the spelling.
    let p = dir.path().join("sheet.ODS");
    let prev = dir.path().join("sheet.previous.ODS");
    std::fs::copy(office_fixture("plain-3x2.ods"), &p).unwrap();
    let original = std::fs::read(&p).unwrap();

    run(&reg, &mut ctx, "read", json!({"filePath": p.to_str().unwrap()})).unwrap();
    let out = run(&reg, &mut ctx, "write", json!({"filePath": p.to_str().unwrap(), "content": "x,y\n"})).unwrap();
    assert_eq!(std::fs::read(&prev).unwrap(), original);
    // write has no .ods writer: the new file is the plain text it was given.
    assert_eq!(std::fs::read(&p).unwrap(), b"x,y\n");
    assert_eq!(
        out.output,
        format!("Replaced {} (4 bytes); the previous document is at {}", p.display(), prev.display())
    );
}

// --------------------------------------------------------- T64 P0 (R4)

#[test]
fn a_docx_code_block_keeps_its_indentation() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("code.docx");
    let content = "Before.\n\n```\nfn main() {\n  two\n    four\n        eight\n\ttab\n}\n```\n\nAfter.\n";
    run(&reg, &mut ctx, "write", json!({"filePath": p.to_str().unwrap(), "content": content})).unwrap();
    let out = run(&reg, &mut ctx, "read", json!({"filePath": p.to_str().unwrap()})).unwrap();
    assert_eq!(
        doc_lines(&out.output),
        ["Before.", "fn main() {", "  two", "    four", "        eight", "\ttab", "}", "After."],
        "{}",
        out.output
    );
}

// --------------------------------------------------------- T64 P4 (F-5)

/// A .docx holding nothing but the given `word/document.xml`. The reader
/// opens that one entry, so no other part is needed; the point is to feed it
/// XML that temur's own writer would never emit.
fn docx_with_document_xml(path: &std::path::Path, xml: &str) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    zip.start_file("word/document.xml", zip::write::SimpleFileOptions::default())
        .unwrap();
    std::io::Write::write_all(&mut zip, xml.as_bytes()).unwrap();
    zip.finish().unwrap();
}

#[test]
fn a_pretty_printed_docx_does_not_read_back_its_own_xml_indentation() {
    // T64 P4 (Amendment 3, F-5). quick-xml runs with trim_text(false), so
    // the newlines and four-space indentation BETWEEN these elements arrive
    // as Text events. Collecting them made a foreign document's XML layout
    // look like content: R4's trim_end() kept it in front of every Code
    // line, and an ordinary paragraph got it between its runs.
    //
    // The stray `&amp;amp;` between the two runs is the entity half of the same
    // claim: entity references arrive as their own event, so the guard has
    // to cover GeneralRef as well as Text or an `&amp;` outside any w:t lands
    // in the paragraph. Without the GeneralRef guard this reads "one& two".
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::standard();
    let mut ctx = ctx_in(dir.path());
    let p = dir.path().join("pretty.docx");
    docx_with_document_xml(
        &p,
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
            "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\n",
            "    <w:body>\n",
            "        <w:p>\n",
            "            <w:pPr>\n",
            "                <w:pStyle w:val=\"Code\"/>\n",
            "            </w:pPr>\n",
            "            <w:r>\n",
            "                <w:t xml:space=\"preserve\">    four spaces of code</w:t>\n",
            "            </w:r>\n",
            "        </w:p>\n",
            "        <w:p>\n",
            "            <w:r>\n",
            "                <w:t>one</w:t>\n",
            "            </w:r>\n",
            "            &amp;amp;\n",
            "            <w:r>\n",
            "                <w:t xml:space=\"preserve\"> two</w:t>\n",
            "            </w:r>\n",
            "        </w:p>\n",
            "    </w:body>\n",
            "</w:document>\n",
        ),
    );
    let out = run(&reg, &mut ctx, "read", json!({"filePath": p.to_str().unwrap()})).unwrap();
    // The Code paragraph keeps the four spaces that are inside its w:t and
    // gains nothing; the two runs join with nothing added between them.
    assert_eq!(
        doc_lines(&out.output),
        ["    four spaces of code", "one two"],
        "{}",
        out.output
    );
}

// --------------------------------------------------- T64 P1b follow-up (A2)

#[test]
fn the_registry_names_the_tools_whose_results_only_acknowledge_a_change() {
    let reg = Registry::standard();
    for name in ["edit", "write", "spreadsheet", "todowrite"] {
        assert!(reg.acknowledges(name), "{name}");
    }
    // bash's output is information; so is every read-only tool's.
    for name in ["bash", "read", "grep", "glob", "todoread", "skill", "no-such-tool"] {
        assert!(!reg.acknowledges(name), "{name}");
    }
}

