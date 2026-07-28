//! Black-box CLI tests (T14): spawn the REAL binary with isolated XDG dirs
//! and assert on exit codes and the stdout/stderr split, the things the
//! in-process suites cannot see. Every child gets its own tempdir config,
//! state, and HOME, and APP_SECRET_FILE is scrubbed, so no test can read the
//! operator's real config or leak state between cases.
//!
//! In the container gate these run against the same mounted binary the
//! smokes use (check.sh mounts the target bin dir at its build path, which
//! is what CARGO_BIN_EXE bakes in).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_temur");

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

struct Sandbox {
    /// Owns the tempdir for the sandbox's lifetime.
    _tmp: tempfile::TempDir,
    config_home: PathBuf,
    state_home: PathBuf,
    home: PathBuf,
}

fn sandbox() -> Sandbox {
    let tmp = tempfile::tempdir().unwrap();
    let config_home = tmp.path().join("config");
    let state_home = tmp.path().join("state");
    let home = tmp.path().join("home");
    for d in [&config_home, &state_home, &home] {
        std::fs::create_dir_all(d).unwrap();
    }
    Sandbox {
        _tmp: tmp,
        config_home,
        state_home,
        home,
    }
}

impl Sandbox {
    fn config_path(&self) -> PathBuf {
        self.config_home.join("temur").join("config.json")
    }

    fn write_config(&self, json: &str) {
        let dir = self.config_home.join("temur");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), json).unwrap();
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(BIN);
        c.env("XDG_CONFIG_HOME", &self.config_home)
            .env("XDG_STATE_HOME", &self.state_home)
            .env("HOME", &self.home)
            .env_remove("APP_SECRET_FILE")
            .env_remove("TEMUR_SKILLS_DIR")
            .env_remove("OPENCODE_SKILLS_DIR")
            .current_dir(&self.home);
        c
    }
}

/// Run to completion with `stdin` piped in; returns (exit code, stdout, stderr).
fn run(mut cmd: Command, stdin: &str) -> (i32, String, String) {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn temur");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().expect("no exit code (signal?)"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// ------------------------------------------------- P1: first-run quickstart

#[test]
fn no_config_live_run_prints_quickstart_and_fails() {
    let sb = sandbox();
    let (code, stdout, stderr) = run(sb.cmd(), "");
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
    // The three quickstart ingredients: the exact path looked for, the init
    // pointer, and the docs pointer.
    assert!(stderr.contains("no config file found"), "stderr: {stderr}");
    assert!(
        stderr.contains(&sb.config_path().display().to_string()),
        "stderr names the config path: {stderr}"
    );
    assert!(stderr.contains("temur init"), "stderr: {stderr}");
    assert!(stderr.contains("temur doctor"), "stderr: {stderr}");
    assert!(stderr.contains("README.md"), "stderr: {stderr}");
    // The raw credential error must be gone, and nothing may reach stdout.
    assert!(
        !stderr.contains("APP_SECRET_FILE"),
        "raw secret error replaced: {stderr}"
    );
    assert!(stdout.is_empty(), "stdout: {stdout}");
}

#[test]
fn no_config_mock_run_is_unchanged() {
    // --mock needs no credentials, so the quickstart must not fire.
    let sb = sandbox();
    let mut c = sb.cmd();
    c.args(["--mock", &fixture("text_simple.sse")]);
    let (code, stdout, stderr) = run(c, "hi\n");
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("[MOCK replay: 1 response(s)]"), "{stdout}");
    assert!(stdout.contains("Hello, world!"), "{stdout}");
    assert!(!stderr.contains("no config file found"), "{stderr}");
}

#[test]
fn no_config_with_app_secret_file_is_unchanged() {
    // The appsvc launcher path: no config file, credential via
    // APP_SECRET_FILE. Startup must proceed exactly as before (banner, then
    // EOF quits; no turn is run, so nothing touches the network).
    let sb = sandbox();
    let keyfile = sb.home.join("dummy-credential");
    std::fs::write(&keyfile, "dummy-value-for-startup-only\n").unwrap();
    let mut c = sb.cmd();
    c.env("APP_SECRET_FILE", &keyfile);
    let (code, stdout, stderr) = run(c, "");
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("temur "), "banner printed: {stdout}");
    assert!(stdout.contains("bye"), "{stdout}");
    assert!(!stderr.contains("no config file found"), "{stderr}");
}

#[test]
fn existing_config_startup_is_unchanged() {
    let sb = sandbox();
    sb.write_config(
        r#"{"provider":"openai-compat","openai_compat":{"model":"qwen3-1.7b"}}"#,
    );
    let (code, stdout, stderr) = run(sb.cmd(), "");
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("(model=qwen3-1.7b, thinking=false)"),
        "banner: {stdout}"
    );
    assert!(stdout.contains("bye"), "{stdout}");
    assert!(!stderr.contains("no config file found"), "{stderr}");
}

// ------------------------------------------------------ P2: one-shot (-p)

#[test]
fn oneshot_prose_turn_stdout_is_exactly_the_answer() {
    let sb = sandbox();
    let mut c = sb.cmd();
    c.args(["--mock", &fixture("text_simple.sse"), "-p", "hi"]);
    let (code, stdout, stderr) = run(c, "");
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    // No banner, no prompt, no "bye": stdout is the prose, newline-terminated.
    assert_eq!(stdout, "Hello, world!\n", "stdout: {stdout:?}");
    // Stats are chrome and land on stderr.
    assert!(stderr.contains("(turn:"), "stderr: {stderr}");
}

#[test]
fn oneshot_tool_turn_splits_streams_anthropic_wire() {
    let sb = sandbox();
    let mut c = sb.cmd();
    let fixtures = format!(
        "{},{}",
        fixture("tool_use_parallel.sse"),
        fixture("text_simple.sse")
    );
    c.args(["--mock", &fixtures, "--prompt", "do the smoke task"]);
    let (code, stdout, stderr) = run(c, "");
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    // Both prose segments on stdout, in order; zero chrome there.
    assert!(
        stdout.contains("read the file and list the directory"),
        "{stdout}"
    );
    assert!(stdout.contains("Hello, world!"), "{stdout}");
    assert!(
        !stdout.contains('→') && !stdout.contains("(turn:"),
        "chrome leaked to stdout: {stdout}"
    );
    // Tool chrome on stderr: starts and ends for both parallel calls.
    assert!(stderr.contains("→ read") && stderr.contains("→ bash"), "{stderr}");
    assert!(stderr.contains("(turn:"), "{stderr}");
}

#[test]
fn oneshot_tool_turn_splits_streams_openai_wire() {
    let sb = sandbox();
    sb.write_config(
        r#"{"provider":"openai-compat","openai_compat":{"model":"mock-local"}}"#,
    );
    let mut c = sb.cmd();
    let fixtures = format!(
        "{},{}",
        fixture("openai/tool_parallel.sse"),
        fixture("openai/text_simple.sse")
    );
    c.args(["--mock", &fixtures, "-p", "do the smoke task"]);
    let (code, stdout, stderr) = run(c, "");
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    // OpenAI chunk streams only assemble through the compat provider, so
    // prose on stdout proves the selection path worked in one-shot mode.
    assert!(stdout.contains("Hello, world!"), "{stdout}");
    assert!(!stdout.contains('→'), "chrome leaked to stdout: {stdout}");
    assert!(stderr.contains("→ bash"), "{stderr}");
}

#[test]
fn oneshot_provider_error_exits_failure() {
    // One fixture, but the tool round needs a second response: the replay
    // transport runs dry, which surfaces as a provider error. One-shot must
    // exit FAILURE with the error on stderr, prose so far on stdout.
    let sb = sandbox();
    let mut c = sb.cmd();
    c.args(["--mock", &fixture("tool_use_parallel.sse"), "-p", "go"]);
    let (code, stdout, stderr) = run(c, "");
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stderr.contains("provider error"), "{stderr}");
    assert!(!stdout.contains("provider error"), "{stdout}");
}

#[test]
fn oneshot_flag_conflicts_are_usage_errors() {
    let sb = sandbox();
    let mut c = sb.cmd();
    c.args(["--tui", "-p", "hi"]);
    let (code, _stdout, stderr) = run(c, "");
    assert_eq!(code, 1);
    assert!(
        stderr.contains("usage:") && stderr.contains("mutually exclusive"),
        "{stderr}"
    );

    let mut c = sb.cmd();
    c.args(["-p", "hi", "tls-probe"]);
    let (code, _stdout, stderr) = run(c, "");
    assert_eq!(code, 1);
    assert!(
        stderr.contains("usage:") && stderr.contains("subcommand"),
        "{stderr}"
    );
}

#[test]
fn oneshot_without_config_gets_the_quickstart() {
    // -p is a live path like the REPL: with no config and no credential it
    // must fail with the quickstart, not a raw secret error.
    let sb = sandbox();
    let mut c = sb.cmd();
    c.args(["-p", "hi"]);
    let (code, stdout, stderr) = run(c, "");
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stderr.contains("temur init"), "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
}

#[test]
fn existing_broken_config_error_is_unchanged() {
    let sb = sandbox();
    sb.write_config(r#"{"provider":"bedrock"}"#);
    let (code, stdout, stderr) = run(sb.cmd(), "");
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stderr.contains("unknown provider \"bedrock\""),
        "old error kept: {stderr}"
    );
    assert!(!stderr.contains("no config file found"), "{stderr}");
}
