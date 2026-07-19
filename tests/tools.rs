//! M3 tool tests — temp dirs on native tmpfs/ext4, run as i686 binaries.

use temur::tools::{Registry, Tool, ToolCtx, ToolError, ToolOutput};
use serde_json::json;

fn ctx_in(dir: &std::path::Path) -> ToolCtx {
    ToolCtx::new(dir.to_path_buf())
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
