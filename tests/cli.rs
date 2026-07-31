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
    // Best-effort: a child that exits before reading stdin (usage errors,
    // overwrite refusal) closes the pipe, and that EPIPE is not a failure.
    let _ = child.stdin.take().unwrap().write_all(stdin.as_bytes());
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
    // T15: the model-shortlist pointer rides the quickstart too.
    assert!(
        stderr.contains("docs/OFFLINE.md") && stderr.contains("Recommended small models"),
        "stderr: {stderr}"
    );
    // T16: sessions discoverability rides it too.
    assert!(
        stderr.contains("saved automatically per working directory")
            && stderr.contains("temur --continue resumes the last one"),
        "stderr: {stderr}"
    );
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

// ------------------------------------------------- T19 P2: read-first e2e

#[test]
fn write_unread_existing_file_denied_through_the_binary() {
    let sb = sandbox();
    // The fixture writes to the RELATIVE path existing.txt, which resolves
    // against the binary's cwd (the sandbox home).
    std::fs::write(sb.home.join("existing.txt"), "original").unwrap();
    let mut c = sb.cmd();
    let fixtures = format!(
        "{},{}",
        fixture("write_unread.sse"),
        fixture("text_simple.sse")
    );
    c.args(["--mock", &fixtures, "-p", "overwrite the file"]);
    let (code, stdout, stderr) = run(c, "");
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        std::fs::read_to_string(sb.home.join("existing.txt")).unwrap(),
        "original",
        "a blind overwrite must be refused"
    );
}

#[test]
fn write_after_read_succeeds_through_the_binary() {
    let sb = sandbox();
    std::fs::write(sb.home.join("existing.txt"), "original").unwrap();
    let mut c = sb.cmd();
    let fixtures = format!(
        "{},{}",
        fixture("read_then_write.sse"),
        fixture("text_simple.sse")
    );
    c.args(["--mock", &fixtures, "-p", "update the file"]);
    let (code, stdout, stderr) = run(c, "");
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        std::fs::read_to_string(sb.home.join("existing.txt")).unwrap(),
        "updated",
        "read-then-write in the same batch must pass"
    );
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
fn oneshot_interrupted_by_sigint_exits_130() {
    // Event-driven, deliberately sleep-free: the fixture's turn runs a
    // `bash sleep 987` tool call, so we block on stderr until the ToolStart
    // line proves the turn is mid-flight (SIGINT handler long installed),
    // send SIGINT, and let the product's own cooperative cancel land the
    // turn. Nothing here depends on scheduling speed.
    use std::io::{BufRead, BufReader, Read};
    let sb = sandbox();
    let mut c = sb.cmd();
    c.args(["--mock", &fixture("interrupt_sleep.sse"), "-p", "go"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().expect("spawn temur");
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        if err.read_line(&mut line).unwrap() == 0 {
            panic!("stderr closed before the bash tool started");
        }
        if line.contains("→ bash") {
            break;
        }
    }
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let mut rest = String::new();
    err.read_to_string(&mut rest).unwrap();
    let status = child.wait().unwrap();
    let mut out = String::new();
    child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    assert_eq!(
        status.code(),
        Some(130),
        "stdout: {out}\nstderr tail: {rest}"
    );
    assert!(rest.contains("turn interrupted"), "{rest}");
    // The pre-interrupt prose still belongs to stdout.
    assert!(out.contains("Sleeping now."), "{out}");
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

// -------------------------------------------------------- P3: temur init

use std::os::unix::fs::PermissionsExt;

fn mode_of(p: &std::path::Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
}

/// A 127.0.0.1 base URL whose port was just bound and released, so a
/// connect there fails fast — the hermetic "server down" answer for the
/// local template's base URL question (T15). Never the default base URL:
/// a real server may be listening on the operator's 8080.
fn refused_base_url() -> String {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    format!("http://127.0.0.1:{port}/v1")
}

#[test]
fn init_local_template_writes_exact_config_and_no_key_file() {
    let sb = sandbox();
    let mut c = sb.cmd();
    c.arg("init");
    // Answers: template 1 (default via empty), base URL (dead port, so the
    // listing fails and the wizard falls back), model default via empty.
    let base = refused_base_url();
    let (code, stdout, stderr) = run(c, &format!("\n{base}\n\n"));
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let written = std::fs::read_to_string(sb.config_path()).unwrap();
    assert_eq!(
        written,
        format!(
            "{{\n  \"provider\": \"openai-compat\",\n  \"max_tokens\": 4096,\n  \"openai_compat\": {{ \"base_url\": \"{base}\",\n                     \"model\": \"qwen3-1.7b\", \"context_window\": 8192 }}\n}}\n"
        )
    );
    assert!(stdout.contains("could not list models from"), "{stdout}");
    assert!(stdout.contains("Wrote "), "{stdout}");
    assert!(!sb.home.join(".secrets").exists(), "keyless template made a key dir");
}

#[test]
fn init_local_picker_lists_server_models_and_number_selects() {
    // Hermetic one-shot HTTP server: one canned listing, then closed. The
    // request head is captured so the no-auth rule is asserted end-to-end
    // through the real binary.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}/v1", listener.local_addr().unwrap().port());
    let body = r#"{"object":"list","data":[{"id":"served-a"},{"id":"served-b"}]}"#;
    let server = std::thread::spawn(move || {
        use std::io::Read;
        let (mut stream, _) = listener.accept().unwrap();
        let mut req = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            let n = stream.read(&mut buf).unwrap();
            req.extend_from_slice(&buf[..n]);
            if n == 0 || req.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
        String::from_utf8_lossy(&req).into_owned()
    });
    let sb = sandbox();
    let mut c = sb.cmd();
    c.arg("init");
    // Template default, the server's base URL, model number 2.
    let (code, stdout, stderr) = run(c, &format!("\n{base}\n2\n"));
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("1) served-a") && stdout.contains("2) served-b"), "{stdout}");
    let written = std::fs::read_to_string(sb.config_path()).unwrap();
    assert!(written.contains("\"model\": \"served-b\""), "{written}");
    let request = server.join().unwrap().to_ascii_lowercase();
    assert!(request.starts_with("get /v1/models "), "{request}");
    assert!(
        !request.contains("authorization") && !request.contains("x-api-key"),
        "init sent a credential header: {request}"
    );
}

#[test]
fn init_anthropic_template_exact_config_and_empty_600_key_file() {
    let sb = sandbox();
    let mut c = sb.cmd();
    c.arg("init");
    // Template 2, default startup profile (sonnet), default key path.
    let (code, stdout, stderr) = run(c, "2\n\n\n");
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let key = sb.home.join(".secrets").join("temur-anthropic-key");
    let written = std::fs::read_to_string(sb.config_path()).unwrap();
    // T16: the template writes the curated 4-profile set, every profile
    // sharing the one key file; startup profile defaults to sonnet.
    let k = key.display();
    assert_eq!(
        written,
        format!(
            "{{\n  \"profiles\": {{\n    \"fable\":  {{ \"provider\": \"anthropic\", \"model\": \"claude-fable-5\",\n                \"api_key_file\": \"{k}\" }},\n    \"haiku\":  {{ \"provider\": \"anthropic\", \"model\": \"claude-haiku-4-5\",\n                \"api_key_file\": \"{k}\" }},\n    \"opus\":   {{ \"provider\": \"anthropic\", \"model\": \"claude-opus-5\",\n                \"api_key_file\": \"{k}\" }},\n    \"sonnet\": {{ \"provider\": \"anthropic\", \"model\": \"claude-sonnet-5\",\n                \"api_key_file\": \"{k}\" }}\n  }},\n  \"profile\": \"sonnet\"\n}}\n"
        )
    );
    assert!(stdout.contains("Startup profile (number or name) [sonnet]"), "{stdout}");
    // Key file: EMPTY (metadata only, contents never read), mode 600, in a
    // 700 dir the wizard created.
    assert_eq!(std::fs::metadata(&key).unwrap().len(), 0);
    assert_eq!(mode_of(&key), 0o600);
    assert_eq!(mode_of(&sb.home.join(".secrets")), 0o700);
    assert!(stdout.contains("Paste your key into"), "{stdout}");
    assert!(stdout.contains("with your editor"), "{stdout}");
    // T16: the closing sessions-discoverability line.
    assert!(
        stdout.contains("saved automatically per working directory")
            && stdout.contains("temur --continue"),
        "{stdout}"
    );
}

#[test]
fn init_hosted_compat_templates_exact_configs() {
    for (answer, name, base, model) in [
        ("3", "openai", "https://api.openai.com/v1", "gpt-4o-mini"),
        (
            "4",
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta/openai",
            "gemini-2.5-flash",
        ),
        ("5", "xai", "https://api.x.ai/v1", "grok-4"),
    ] {
        let sb = sandbox();
        let mut c = sb.cmd();
        c.arg("init");
        let (code, stdout, stderr) = run(c, &format!("{answer}\n\n\n"));
        assert_eq!(code, 0, "{name}: stdout: {stdout}\nstderr: {stderr}");
        let key = sb.home.join(".secrets").join(format!("temur-{name}-key"));
        let written = std::fs::read_to_string(sb.config_path()).unwrap();
        assert_eq!(
            written,
            format!(
                "{{\n  \"provider\": \"openai-compat\",\n  \"openai_compat\": {{ \"base_url\": \"{base}\",\n                     \"model\": \"{model}\",\n                     \"api_key_file\": \"{}\" }}\n}}\n",
                key.display()
            ),
            "template {name}"
        );
        assert_eq!(std::fs::metadata(&key).unwrap().len(), 0, "{name}");
        assert_eq!(mode_of(&key), 0o600, "{name}");
    }
}

#[test]
fn init_custom_model_and_key_path_survive() {
    let sb = sandbox();
    let keydir = sb.home.join("alt-keys");
    let keypath = keydir.join("k");
    let mut c = sb.cmd();
    c.arg("init");
    let (code, stdout, stderr) = run(c, &format!("openai\nmy-model\n{}\n", keypath.display()));
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let written = std::fs::read_to_string(sb.config_path()).unwrap();
    assert!(written.contains("\"my-model\""), "{written}");
    assert!(written.contains(&keypath.display().to_string()), "{written}");
    assert_eq!(std::fs::metadata(&keypath).unwrap().len(), 0);
    assert_eq!(mode_of(&keypath), 0o600);
    assert_eq!(mode_of(&keydir), 0o700, "created parent gets 700");
}

#[test]
fn init_refuses_to_overwrite_without_force() {
    let sb = sandbox();
    sb.write_config(r#"{"model":"keep-me"}"#);
    let mut c = sb.cmd();
    c.arg("init");
    let (code, _stdout, stderr) = run(c, "\n\n");
    assert_eq!(code, 1);
    assert!(
        stderr.contains(&sb.config_path().display().to_string())
            && stderr.contains("already exists")
            && stderr.contains("--force"),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(sb.config_path()).unwrap(),
        r#"{"model":"keep-me"}"#,
        "refusal must not touch the file"
    );
}

#[test]
fn init_force_overwrites_and_force_is_init_only() {
    let sb = sandbox();
    sb.write_config(r#"{"model":"old"}"#);
    let mut c = sb.cmd();
    c.args(["init", "--force"]);
    let (code, stdout, stderr) = run(c, &format!("\n{}\n\n", refused_base_url()));
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        std::fs::read_to_string(sb.config_path()).unwrap().contains("openai-compat")
    );

    // --force anywhere else is a usage error, not a silent no-op.
    let mut c = sb.cmd();
    c.arg("--force");
    let (code, _stdout, stderr) = run(c, "");
    assert_eq!(code, 1);
    assert!(stderr.contains("--force is only valid"), "{stderr}");
}

#[test]
fn init_leaves_an_existing_key_file_untouched() {
    let sb = sandbox();
    let keypath = sb.home.join("existing-key");
    std::fs::write(&keypath, "REAL-KEY-MATERIAL\n").unwrap();
    let mut c = sb.cmd();
    c.arg("init");
    let (code, stdout, stderr) = run(c, &format!("2\n\n{}\n", keypath.display()));
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        std::fs::read_to_string(&keypath).unwrap(),
        "REAL-KEY-MATERIAL\n",
        "an existing key file must never be truncated or rewritten"
    );
    assert!(stdout.contains("left untouched"), "{stdout}");
    // And the key material must never appear in any output stream.
    assert!(!stdout.contains("REAL-KEY-MATERIAL"), "{stdout}");
    assert!(!stderr.contains("REAL-KEY-MATERIAL"), "{stderr}");
}

#[test]
fn init_rejects_unknown_template() {
    let sb = sandbox();
    let mut c = sb.cmd();
    c.arg("init");
    let (code, _stdout, stderr) = run(c, "7\n");
    assert_eq!(code, 1);
    assert!(stderr.contains("unknown template"), "{stderr}");
    assert!(!sb.config_path().exists(), "no config on a failed wizard");
}

// ------------------------------------------------- T17: temur init --add

/// The local starter config in the exact pretty form `init --add` re-emits,
/// so the merge assertions below are byte-predictable.
const LOCAL_PRETTY: &str = "{\n  \"provider\": \"openai-compat\",\n  \"max_tokens\": 4096,\n  \"openai_compat\": {\n    \"model\": \"qwen3-1.7b\",\n    \"context_window\": 8192\n  }\n}\n";

#[test]
fn init_add_anthropic_merges_profiles_and_leaves_the_rest_alone() {
    let sb = sandbox();
    sb.write_config(LOCAL_PRETTY);
    let mut c = sb.cmd();
    c.args(["init", "--add", "anthropic"]);
    // One answer: the key path (default under HOME).
    let (code, stdout, stderr) = run(c, "\n");
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let key = sb.home.join(".secrets").join("temur-anthropic-key");
    let written = std::fs::read_to_string(sb.config_path()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&written).unwrap();
    let profiles = parsed["profiles"].as_object().unwrap();
    assert_eq!(
        profiles.keys().collect::<Vec<_>>(),
        vec!["fable", "haiku", "opus", "sonnet"],
        "{written}"
    );
    for (_, p) in profiles {
        assert_eq!(p["api_key_file"], key.display().to_string(), "{written}");
    }
    // The base selection survives byte-relevant: same fields, and NO
    // startup "profile" key was invented.
    assert_eq!(parsed["openai_compat"]["model"], "qwen3-1.7b", "{written}");
    assert_eq!(parsed["max_tokens"], 4096, "{written}");
    assert!(parsed.get("profile").is_none(), "{written}");
    // Key file exactly as the fresh wizard makes it: empty, 600.
    assert_eq!(std::fs::metadata(&key).unwrap().len(), 0);
    assert_eq!(mode_of(&key), 0o600);
    assert!(stdout.contains("Added profiles \"fable\", \"haiku\", \"opus\", \"sonnet\""), "{stdout}");
    assert!(stdout.contains("/model <name> switches to one"), "{stdout}");
}

#[test]
fn init_add_each_single_profile_template_through_the_binary() {
    // Hosted templates: model default + key default. local: dead-port base
    // URL (listing fails, free-text fallback) + model default, keyless.
    for template in ["openai", "gemini", "xai"] {
        let sb = sandbox();
        sb.write_config(LOCAL_PRETTY);
        let mut c = sb.cmd();
        c.args(["init", "--add", template]);
        let (code, stdout, stderr) = run(c, "\n\n");
        assert_eq!(code, 0, "{template}: stdout: {stdout}\nstderr: {stderr}");
        let written = std::fs::read_to_string(sb.config_path()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert!(parsed["profiles"][template].is_object(), "{template}: {written}");
        let key = sb.home.join(".secrets").join(format!("temur-{template}-key"));
        assert_eq!(std::fs::metadata(&key).unwrap().len(), 0, "{template}");
        assert_eq!(mode_of(&key), 0o600, "{template}");
    }
    let sb = sandbox();
    sb.write_config(LOCAL_PRETTY);
    let mut c = sb.cmd();
    c.args(["init", "--add", "local"]);
    let (code, stdout, stderr) = run(c, &format!("{}\n\n", refused_base_url()));
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let written = std::fs::read_to_string(sb.config_path()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&written).unwrap();
    assert_eq!(parsed["profiles"]["local"]["model"], "qwen3-1.7b", "{written}");
    assert!(!sb.home.join(".secrets").exists(), "keyless template made a key dir");
}

#[test]
fn init_add_collision_and_missing_config_fail_closed() {
    // Collision: a profile named "openai" already exists.
    let sb = sandbox();
    let before = "{\n  \"profiles\": {\n    \"openai\": {\n      \"provider\": \"openai-compat\",\n      \"model\": \"mine\"\n    }\n  }\n}\n";
    sb.write_config(before);
    let mut c = sb.cmd();
    c.args(["init", "--add", "openai"]);
    let (code, _stdout, stderr) = run(c, "\n\n");
    assert_eq!(code, 1);
    assert!(stderr.contains("\"openai\" already in"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(sb.config_path()).unwrap(),
        before,
        "collision must not touch the file"
    );

    // No config: --add points at the plain wizard instead of inventing one.
    let sb = sandbox();
    let mut c = sb.cmd();
    c.args(["init", "--add", "anthropic"]);
    let (code, _stdout, stderr) = run(c, "\n");
    assert_eq!(code, 1);
    assert!(stderr.contains("no config at"), "{stderr}");
    assert!(stderr.contains("temur init"), "{stderr}");
    assert!(!sb.config_path().exists());
}

#[test]
fn init_key_entry_piped_placeholder_lands_in_the_key_file_only() {
    // T17 P3, piped path: the placeholder reaches the key file and nothing
    // else; the pty-level "nothing echoed" check is the P5 live smoke.
    let sb = sandbox();
    sb.write_config(LOCAL_PRETTY);
    let mut c = sb.cmd();
    c.args(["init", "--add", "openai"]);
    // Answers: model default, key path default, then the placeholder.
    let (code, stdout, stderr) = run(c, "\n\nplaceholder-not-a-real-key\n");
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let key = sb.home.join(".secrets").join("temur-openai-key");
    assert_eq!(
        std::fs::read_to_string(&key).unwrap(),
        "placeholder-not-a-real-key\n"
    );
    assert_eq!(mode_of(&key), 0o600);
    assert!(stdout.contains("key saved (hidden)"), "{stdout}");
    assert!(!stdout.contains("placeholder-not-a-real-key"), "{stdout}");
    assert!(!stderr.contains("placeholder-not-a-real-key"), "{stderr}");
}

#[test]
fn init_add_flag_rules() {
    // --add without init is a usage error.
    let sb = sandbox();
    let mut c = sb.cmd();
    c.args(["--add", "openai"]);
    let (code, _stdout, stderr) = run(c, "");
    assert_eq!(code, 1);
    assert!(stderr.contains("--add is only valid with the init subcommand"), "{stderr}");

    // --add plus --force is contradictory and rejected.
    let mut c = sb.cmd();
    c.args(["init", "--add", "openai", "--force"]);
    let (code, _stdout, stderr) = run(c, "");
    assert_eq!(code, 1);
    assert!(stderr.contains("--force does not combine with --add"), "{stderr}");
}

// ----------------------------------------------- T15: /model ... --save

#[test]
fn model_save_persists_across_restart() {
    // Live keyless run: provider construction is lazy, no turn runs, so
    // nothing touches the network. First run switches and saves; the
    // second proves the file now selects the saved model at startup.
    let sb = sandbox();
    sb.write_config(
        r#"{"provider":"openai-compat","openai_compat":{"model":"first-model"}}"#,
    );
    let (code, stdout, stderr) = run(sb.cmd(), "/model second-model --save\n");
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("switched model to second-model"), "{stdout}");
    assert!(
        stdout.contains(&format!(
            "saved model second-model to {}",
            sb.config_path().display()
        )),
        "{stdout}"
    );

    let (code, stdout, stderr) = run(sb.cmd(), "/status\n");
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("(model=second-model"),
        "restart banner shows the saved model: {stdout}"
    );
    assert!(
        stdout.contains("provider: openai-compat · model: second-model"),
        "{stdout}"
    );
}

#[test]
fn model_save_without_config_file_reports_and_keeps_the_switch() {
    // A config-less start only works with a credential path; openai-compat
    // needs a config, so drive the anthropic default via APP_SECRET_FILE
    // with a dummy value that is never sent anywhere (no turn runs).
    let sb = sandbox();
    let keyfile = sb.home.join("dummy-credential");
    std::fs::write(&keyfile, "dummy-value-for-startup-only\n").unwrap();
    let mut c = sb.cmd();
    c.env("APP_SECRET_FILE", &keyfile);
    let (code, stdout, stderr) = run(c, "/model other-model --save\n");
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("switched model to other-model"), "{stdout}");
    assert!(
        stdout.contains("NOT saved") && stdout.contains("no config file"),
        "{stdout}"
    );
    assert!(!sb.config_path().exists(), "no file invented");
}

// ------------------------------------------------------ P4: temur doctor

fn doctor(sb: &Sandbox) -> (i32, String, String) {
    let mut c = sb.cmd();
    c.args(["doctor", "--no-network"]);
    run(c, "")
}

fn write_key(sb: &Sandbox, name: &str, contents: &str, mode: u32) -> std::path::PathBuf {
    let p = sb.home.join(name);
    std::fs::write(&p, contents).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    p
}

fn keyed_config(sb: &Sandbox, key_path: &std::path::Path) {
    sb.write_config(&format!(
        r#"{{"provider":"openai-compat","openai_compat":{{"model":"m","api_key_file":"{}"}}}}"#,
        key_path.display()
    ));
}

#[test]
fn doctor_without_config_fails_with_the_quickstart_pointer() {
    let sb = sandbox();
    let (code, stdout, stderr) = doctor(&sb);
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("FAIL: no config file at"), "{stdout}");
    assert!(stdout.contains("temur init"), "{stdout}");
    assert!(stdout.contains("doctor: "), "summary line present: {stdout}");
}

#[test]
fn doctor_healthy_keyless_config_passes() {
    let sb = sandbox();
    sb.write_config(
        r#"{"provider":"openai-compat","openai_compat":{"model":"qwen3-1.7b"}}"#,
    );
    let (code, stdout, stderr) = doctor(&sb);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("PASS: config parsed"), "{stdout}");
    assert!(stdout.contains("PASS: active selection"), "{stdout}");
    assert!(stdout.contains("keyless"), "{stdout}");
    assert!(stdout.contains("sessions dir"), "{stdout}");
    assert!(stdout.contains("SKIP: reachability probes (--no-network)"), "{stdout}");
    assert!(stdout.contains("0 fail"), "{stdout}");
    assert!(!stdout.contains("FAIL"), "{stdout}");
}

#[test]
fn doctor_healthy_keyed_config_never_reads_the_key() {
    let sb = sandbox();
    let key = write_key(&sb, "k", "SUPER-SECRET-VALUE\n", 0o600);
    keyed_config(&sb, &key);
    let (code, stdout, stderr) = doctor(&sb);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("non-empty (by size), mode 600"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("SUPER-SECRET-VALUE") && !stderr.contains("SUPER-SECRET-VALUE"),
        "key material must never surface"
    );
}

#[test]
fn doctor_bad_prompt_profile_fails_naming_it() {
    let sb = sandbox();
    sb.write_config(r#"{"prompt_profile":"tiny"}"#);
    let (code, stdout, _stderr) = doctor(&sb);
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("FAIL") && stdout.contains("tiny"), "{stdout}");
}

#[test]
fn doctor_missing_key_file_fails() {
    let sb = sandbox();
    keyed_config(&sb, &sb.home.join("nope-key"));
    let (code, stdout, _stderr) = doctor(&sb);
    assert_eq!(code, 1, "{stdout}");
    assert!(
        stdout.contains("FAIL") && stdout.contains("missing"),
        "{stdout}"
    );
}

#[test]
fn doctor_world_readable_key_warns_but_passes() {
    let sb = sandbox();
    let key = write_key(&sb, "k", "value\n", 0o644);
    keyed_config(&sb, &key);
    let (code, stdout, _stderr) = doctor(&sb);
    assert_eq!(code, 0, "a loose mode is advisory: {stdout}");
    assert!(
        stdout.contains("WARN") && stdout.contains("mode 644") && stdout.contains("chmod 600"),
        "{stdout}"
    );
}

#[test]
fn doctor_empty_key_file_fails() {
    let sb = sandbox();
    let key = write_key(&sb, "k", "", 0o600);
    keyed_config(&sb, &key);
    let (code, stdout, _stderr) = doctor(&sb);
    assert_eq!(code, 1, "{stdout}");
    assert!(
        stdout.contains("FAIL") && stdout.contains("empty (by size)"),
        "{stdout}"
    );
}

#[test]
fn doctor_anthropic_without_any_key_path_fails() {
    let sb = sandbox();
    sb.write_config(r#"{"provider":"anthropic"}"#);
    let (code, stdout, _stderr) = doctor(&sb);
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("APP_SECRET_FILE is not set"), "{stdout}");
}

#[test]
fn doctor_inactive_profile_key_problem_is_warn_only() {
    let sb = sandbox();
    sb.write_config(&format!(
        r#"{{"provider":"openai-compat",
            "openai_compat":{{"model":"m"}},
            "profiles":{{"hosted":{{"provider":"anthropic","model":"x",
                          "api_key_file":"{}"}}}}}}"#,
        sb.home.join("absent-key").display()
    ));
    let (code, stdout, _stderr) = doctor(&sb);
    assert_eq!(code, 0, "inactive profile problems must not FAIL: {stdout}");
    assert!(
        stdout.contains("WARN") && stdout.contains("profile \"hosted\""),
        "{stdout}"
    );
}

#[test]
fn doctor_sessions_dir_that_is_a_file_fails() {
    let sb = sandbox();
    let bogus = sb.home.join("sessions-blocker");
    std::fs::write(&bogus, "x").unwrap();
    sb.write_config(&format!(
        r#"{{"provider":"openai-compat","openai_compat":{{"model":"m"}},"sessions_dir":"{}"}}"#,
        bogus.display()
    ));
    let (code, stdout, _stderr) = doctor(&sb);
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("not a directory"), "{stdout}");
}

#[test]
fn no_network_outside_doctor_is_a_usage_error() {
    let sb = sandbox();
    let mut c = sb.cmd();
    c.arg("--no-network");
    let (code, _stdout, stderr) = run(c, "");
    assert_eq!(code, 1);
    assert!(stderr.contains("--no-network is only valid"), "{stderr}");
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

// ------------------------------------------------- T21: bash approval (e2e)
//
// The Ask arm needs a probe-FAIL, forced deterministically via the one-way
// TEMUR_TEST_SANDBOX_UNAVAILABLE seam (set on the CHILD only, so nothing
// in this process is touched), and an interactive terminal, provided by a
// pty from script(1). Placeholder key material only, as everywhere.

/// Config with one keyed anthropic profile whose key file is a placeholder
/// inside the sandbox home; returns the key path.
fn guarded_mock_config(sb: &Sandbox) -> PathBuf {
    let key = sb.home.join("api.key");
    std::fs::write(&key, "placeholder-not-a-real-key\n").unwrap();
    sb.write_config(&format!(
        r#"{{"profiles":{{"a":{{"provider":"anthropic","model":"claude-sonnet-5","api_key_file":"{}"}}}},"profile":"a"}}"#,
        key.display()
    ));
    key
}

fn approval_fixtures() -> String {
    format!("{},{}", fixture("bash_approval.sse"), fixture("text_simple.sse"))
}

/// Run the binary under a pty (script(1)) so stdin/stdout ARE a terminal,
/// with `stdin` relayed in. Returns (exit code of script, pty output).
fn run_pty(sb: &Sandbox, args: &str, stdin: &str, force_probe_fail: bool) -> (i32, String) {
    let mut c = Command::new("script");
    c.args(["-qec", &format!("{BIN} {args}"), "/dev/null"])
        .env("XDG_CONFIG_HOME", &sb.config_home)
        .env("XDG_STATE_HOME", &sb.state_home)
        .env("HOME", &sb.home)
        .env_remove("APP_SECRET_FILE")
        .env_remove("TEMUR_SKILLS_DIR")
        .env_remove("OPENCODE_SKILLS_DIR")
        .current_dir(&sb.home);
    if force_probe_fail {
        c.env("TEMUR_TEST_SANDBOX_UNAVAILABLE", "1");
    } else {
        c.env_remove("TEMUR_TEST_SANDBOX_UNAVAILABLE");
    }
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().expect("spawn script(1) pty wrapper");
    let _ = child.stdin.take().unwrap().write_all(stdin.as_bytes());
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().expect("no exit code (signal?)"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn approval_pty_yes_prompts_with_the_exact_command_and_runs_it() {
    let sb = sandbox();
    guarded_mock_config(&sb);
    let (code, out) = run_pty(
        &sb,
        &format!("--plain --mock {}", approval_fixtures()),
        "do it\ny\nexit\n",
        true,
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("bash approval needed"), "{out}");
    assert!(out.contains("echo approval-ran > approval-marker.txt"), "exact command shown: {out}");
    assert!(out.contains("run it? [y/N]"), "{out}");
    assert!(out.contains("✓ bash"), "approved bash succeeds: {out}");
    assert!(out.contains("Hello, world!"), "second round: {out}");
    assert_eq!(
        std::fs::read_to_string(sb.home.join("approval-marker.txt")).unwrap(),
        "approval-ran\n",
        "the approved command must actually run"
    );
}

#[test]
fn approval_pty_no_denies_and_the_session_continues() {
    let sb = sandbox();
    guarded_mock_config(&sb);
    let (code, out) = run_pty(
        &sb,
        &format!("--plain --mock {}", approval_fixtures()),
        "do it\nn\nexit\n",
        true,
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("run it? [y/N]"), "{out}");
    assert!(out.contains("✗ bash"), "denied bash is an error result: {out}");
    assert!(out.contains("Hello, world!"), "the session continues: {out}");
    assert!(
        !sb.home.join("approval-marker.txt").exists(),
        "a denied command must not run: {out}"
    );
}

#[test]
fn approval_piped_noninteractive_still_refuses() {
    let sb = sandbox();
    guarded_mock_config(&sb);
    let mut c = sb.cmd();
    c.env("TEMUR_TEST_SANDBOX_UNAVAILABLE", "1")
        .args(["--plain", "--mock", &approval_fixtures()]);
    let (code, stdout, stderr) = run(c, "do it\nexit\n");
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(!stdout.contains("[y/N]"), "piped stdin must never prompt: {stdout}");
    assert!(stdout.contains("✗ bash"), "refusal is an error result: {stdout}");
    assert!(stdout.contains("Hello, world!"), "the session continues: {stdout}");
    assert!(
        !sb.home.join("approval-marker.txt").exists(),
        "a refused command must not run"
    );
}

#[test]
fn approval_keyless_pty_never_prompts_and_runs_plain() {
    let sb = sandbox();
    sb.write_config("{}");
    let (code, out) = run_pty(
        &sb,
        &format!("--plain --mock {}", approval_fixtures()),
        "do it\nexit\n",
        true,
    );
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("[y/N]"), "keyless must never prompt: {out}");
    assert!(out.contains("✓ bash"), "{out}");
    assert_eq!(
        std::fs::read_to_string(sb.home.join("approval-marker.txt")).unwrap(),
        "approval-ran\n"
    );
}

#[test]
fn approval_working_sandbox_pty_never_prompts() {
    if !temur::tools::sandbox_available() {
        eprintln!("skip: no unprivileged user namespaces in this environment");
        return;
    }
    let sb = sandbox();
    guarded_mock_config(&sb);
    let (code, out) = run_pty(
        &sb,
        &format!("--plain --mock {}", approval_fixtures()),
        "do it\nexit\n",
        false,
    );
    assert_eq!(code, 0, "{out}");
    assert!(
        !out.contains("[y/N]"),
        "a working sandbox is never preempted by approval: {out}"
    );
    assert!(out.contains("✓ bash"), "{out}");
    assert_eq!(
        std::fs::read_to_string(sb.home.join("approval-marker.txt")).unwrap(),
        "approval-ran\n",
        "sandboxed bash still writes ordinary files"
    );
}
