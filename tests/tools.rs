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
        !out.output.contains("(output truncated: showing first"),
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
    assert!(out.output.contains("(output truncated: showing first 30000 of 40000 chars)"));
    assert!(out.output.len() < 40_000);
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
