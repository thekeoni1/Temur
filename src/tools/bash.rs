use super::{parse_input, Tool, ToolCtx, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::{json, Value};
use std::ffi::CString;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 600_000;

// --------------------------------------------------------- T18 layer 2
// The bash key sandbox: an unprivileged user namespace + mount namespace
// in which every existing protected key file is bind-masked with
// /dev/null, so inside the child the key path reads as empty and writes
// are discarded. Sequence verified against user_namespaces(7) and
// mount_namespaces(7)/mount(2):
//   1. unshare(CLONE_NEWUSER | CLONE_NEWNS);
//   2. write "deny" to /proc/self/setgroups (required before gid_map for
//      a process without CAP_SETGID in the parent namespace), then the
//      single-line self-maps "uid uid 1" / "gid gid 1" (the one mapping
//      an unprivileged process may write);
//   3. mount(NULL, "/", NULL, MS_REC | MS_PRIVATE, NULL): most systems
//      default to shared propagation, and the masks must never propagate
//      back to the host;
//   4. mount("/dev/null", <key>, NULL, MS_BIND, NULL) per existing file.
// Everything the pre_exec closure touches is pre-computed BYTES: it runs
// between fork and exec in a threaded process, so it must not allocate.

/// The refusal wording when keys are configured, no sandbox is possible,
/// and no approver is installed (T21: the NON-interactive arm; interactive
/// sessions ask per command instead of refusing). One constant so the tool
/// error and the tests cannot drift.
pub const SANDBOX_REFUSAL: &str = "bash is disabled: key files are configured, and this kernel does not allow the unprivileged user namespace sandbox that isolates them from shell commands. In an interactive session temur asks for per-command approval instead; this non-interactive session cannot ask. The other tools stay guarded. For non-interactive use, set \"allow_bash_without_key_sandbox\": true in config.json to accept running bash WITHOUT the key sandbox.";

/// The denial wording when the user answers no at the approval prompt
/// (T21). Returned as a normal is_error tool_result, so the model can
/// adapt and the turn continues.
pub const APPROVAL_DENIED: &str = "the user declined to run this command";

/// What a spawn should do, given the guard and host facts. Pure, so the
/// whole decision table is unit-testable with an injected probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SandboxDecision {
    /// No keys configured (or override accepted, or the user approved this
    /// one command): spawn byte-identically to pre-T18 behavior, no
    /// unshare at all.
    Plain,
    Sandboxed,
    /// T21: sandbox unavailable, override off, but an interactive approver
    /// is installed: ask the user about THIS command.
    Ask,
    Refuse,
}

/// Keyless configs never even probe: the invariant is that they spawn
/// exactly as before T18. With keys, a working sandbox always wins
/// (neither the override nor an approver ever preempts it); the override
/// silences the ask entirely; the T21 Ask arm rescues interactive sessions
/// on hosts where the probe fails; non-interactive sessions still refuse.
fn decide_sandbox(
    keys_guarded: bool,
    allow_unsandboxed: bool,
    approver_available: bool,
    probe: impl FnOnce() -> bool,
) -> SandboxDecision {
    if !keys_guarded {
        return SandboxDecision::Plain;
    }
    if probe() {
        return SandboxDecision::Sandboxed;
    }
    if allow_unsandboxed {
        return SandboxDecision::Plain;
    }
    if approver_available {
        SandboxDecision::Ask
    } else {
        SandboxDecision::Refuse
    }
}

/// Async-signal-safe file write for the pre_exec closure: open/write/
/// close only, errors via errno with no allocation.
///
/// # Safety
/// `path` must be a valid NUL-terminated C string.
unsafe fn write_bytes_raw(path: *const libc::c_char, data: &[u8]) -> std::io::Result<()> {
    let fd = libc::open(path, libc::O_WRONLY);
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let n = libc::write(fd, data.as_ptr() as *const libc::c_void, data.len());
    let write_err = if n < 0 {
        Some(std::io::Error::last_os_error())
    } else if n as usize != data.len() {
        Some(std::io::Error::from_raw_os_error(libc::EIO))
    } else {
        None
    };
    libc::close(fd);
    match write_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Install the sandbox pre_exec on `cmd`. `masks` are the (already
/// canonicalized, existing) key files to bind-mask; the probe passes an
/// empty list. All bytes are pre-computed here, before the fork.
fn install_sandbox(cmd: &mut Command, masks: &[std::path::PathBuf]) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;
    let c_masks: Vec<CString> = masks
        .iter()
        .map(|p| CString::new(p.as_os_str().as_bytes()))
        .collect::<Result<_, _>>()
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let devnull = CString::new("/dev/null").unwrap();
    let root = CString::new("/").unwrap();
    let setgroups = CString::new("/proc/self/setgroups").unwrap();
    let uid_map_path = CString::new("/proc/self/uid_map").unwrap();
    let gid_map_path = CString::new("/proc/self/gid_map").unwrap();
    // getuid/getgid in the parent: fork inherits them, and the map line
    // must name the parent-namespace ids anyway.
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let uid_map: Vec<u8> = format!("{uid} {uid} 1\n").into_bytes();
    let gid_map: Vec<u8> = format!("{gid} {gid} 1\n").into_bytes();
    unsafe {
        cmd.pre_exec(move || {
            if libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            write_bytes_raw(setgroups.as_ptr(), b"deny")?;
            write_bytes_raw(uid_map_path.as_ptr(), &uid_map)?;
            write_bytes_raw(gid_map_path.as_ptr(), &gid_map)?;
            if libc::mount(
                std::ptr::null(),
                root.as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            for mask in &c_masks {
                if libc::mount(
                    devnull.as_ptr(),
                    mask.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                ) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    Ok(())
}

/// Can this host run the sandbox at all? Answered EMPIRICALLY: spawn
/// `true` through the exact pre_exec sequence (no masks needed) and see
/// whether it exits cleanly. Cached per process; also used by doctor.
pub fn sandbox_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        // T21 test seam: force the probe to FAIL, so the Ask/Refuse arms
        // are reachable on hosts whose kernel would let the real probe
        // succeed (the e2e suites and the live-smoke fallback). One-way by
        // design: the seam can only make behavior MORE restrictive; there
        // is no way to fake a WORKING sandbox.
        if std::env::var_os("TEMUR_TEST_SANDBOX_UNAVAILABLE").is_some() {
            return false;
        }
        let mut cmd = Command::new("true");
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if install_sandbox(&mut cmd, &[]).is_err() {
            return false;
        }
        matches!(cmd.status(), Ok(s) if s.success())
    })
}

/// Kill the child's whole process group (see `process_group(0)` at spawn),
/// then reap the sh itself. The group kill goes through sh's BUILTIN kill —
/// a kill *binary* does not exist in minimal images (debian base ships none),
/// but the builtin is everywhere sh is, and sh is what spawned the child.
/// Failures are ignored — the direct kill below still ends the sh either way.
fn kill_group(child: &mut std::process::Child) {
    let _ = Command::new("sh")
        .arg("-c")
        .arg(format!("kill -9 -{}", child.id()))
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

#[derive(Deserialize)]
struct Params {
    command: String,
    /// Milliseconds.
    timeout: Option<u64>,
    workdir: Option<String>,
}

pub struct BashTool;

impl Tool for BashTool {
    fn name(&self) -> &'static str {
        "bash"
    }
    fn description(&self) -> &'static str {
        include_str!("prompts/bash.txt")
    }
    fn description_compact(&self) -> &'static str {
        include_str!("prompts/compact/bash.txt")
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "The command to execute"},
                "timeout": {"type": "number", "description": "Optional timeout in milliseconds"},
                "workdir": {"type": "string", "description": "The working directory to run the command in. Use this instead of 'cd' commands."}
            },
            "required": ["command"]
        })
    }

    fn execute(&self, input: Value, ctx: &mut ToolCtx) -> Result<ToolOutput, ToolError> {
        let p: Params = parse_input(input)?;
        let timeout = Duration::from_millis(p.timeout.unwrap_or(DEFAULT_TIMEOUT_MS).min(MAX_TIMEOUT_MS));
        let workdir = p
            .workdir
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| ctx.cwd.clone());

        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(&p.command)
            .current_dir(&workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Own process group (leader = the sh itself): sh usually FORKS the
        // command, so killing sh alone orphans it — and the orphan holds the
        // output pipes open, blocking the drain threads below until it exits
        // on its own. Kill/timeout paths kill the whole group instead.
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        // T18 layer 2: with key files guarded, bash MUST run sandboxed (or
        // refuse; or run plain when the config explicitly accepts the
        // risk; or, T21, ask the user per command in an interactive
        // session). A keyless guard takes the Plain arm without probing:
        // byte-identical spawn to pre-T18.
        match decide_sandbox(
            !ctx.guard.is_empty(),
            ctx.allow_unsandboxed_bash,
            ctx.bash_approver.is_some(),
            sandbox_available,
        ) {
            SandboxDecision::Plain => {}
            SandboxDecision::Refuse => return Err(ToolError::failed(SANDBOX_REFUSAL)),
            SandboxDecision::Ask => {
                // Default is DENY: an interrupt already requested (cancel
                // token set) denies without even prompting, and the
                // approver itself answers false for anything but an
                // explicit yes. An approved command runs PLAIN, this one
                // time only; the decision is never cached.
                let approved = !ctx.cancel.is_set()
                    && ctx
                        .bash_approver
                        .as_mut()
                        .map(|approve| approve(&p.command))
                        .unwrap_or(false);
                if !approved {
                    return Err(ToolError::failed(APPROVAL_DENIED));
                }
            }
            SandboxDecision::Sandboxed => {
                // Mask what exists; a configured-but-missing key file has
                // nothing to mask (and layer 1 still guards its path).
                let masks: Vec<std::path::PathBuf> = ctx
                    .guard
                    .protected_files()
                    .iter()
                    .filter(|p| p.exists())
                    .cloned()
                    .collect();
                install_sandbox(&mut cmd, &masks)
                    .map_err(|e| ToolError::failed(format!("key sandbox setup failed: {e}")))?;
            }
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| ToolError::failed(format!("failed to spawn shell: {e}")))?;

        // Drain pipes on threads so a chatty child can't deadlock the wait.
        let mut stdout_pipe = child.stdout.take().expect("stdout piped");
        let mut stderr_pipe = child.stderr.take().expect("stderr piped");
        let out_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stdout_pipe.read_to_end(&mut buf);
            buf
        });
        let err_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr_pipe.read_to_end(&mut buf);
            buf
        });

        // Sliced wait (T6): poll the cancel token every ≤200 ms so an Esc
        // reaches a long-running command promptly. Deadline math in u64
        // millis, never usize (32-bit target). The timeout semantics are
        // unchanged: slices sum to exactly the configured deadline.
        let deadline_ms: u64 = timeout.as_millis() as u64;
        let mut waited_ms: u64 = 0;
        let mut interrupted = false;
        let (timed_out, exit_code) = loop {
            if ctx.cancel.is_set() {
                kill_group(&mut child);
                interrupted = true;
                break (false, None);
            }
            let slice = Duration::from_millis((deadline_ms - waited_ms).min(200));
            match child
                .wait_timeout(slice)
                .map_err(|e| ToolError::failed(e.to_string()))?
            {
                Some(status) => break (false, status.code()),
                None => {
                    waited_ms += slice.as_millis() as u64;
                    if waited_ms >= deadline_ms {
                        kill_group(&mut child);
                        break (true, None);
                    }
                }
            }
        };

        let stdout = String::from_utf8_lossy(&out_thread.join().unwrap_or_default()).into_owned();
        let stderr = String::from_utf8_lossy(&err_thread.join().unwrap_or_default()).into_owned();

        let mut output = String::new();
        if !stdout.is_empty() {
            output.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(&stderr);
        }
        if interrupted {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str("(interrupted by user)");
            // An interrupted command is an error result: the model must not
            // treat whatever partial output exists as the command's outcome.
            return Err(ToolError::Failed(output));
        }
        if timed_out {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(&format!("(command timed out after {} ms)", timeout.as_millis()));
        } else if let Some(code) = exit_code {
            if code != 0 {
                if !output.is_empty() && !output.ends_with('\n') {
                    output.push('\n');
                }
                output.push_str(&format!("(exit code {code})"));
            }
        }
        if output.is_empty() {
            output.push_str("(no output)");
        }

        Ok(ToolOutput {
            title: p.command,
            output,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full T18+T21 decision table with an injected probe. The probe
    /// must not even RUN for keyless configs (the invariant is "no unshare
    /// at all"), asserted via a panicking probe, with and without an
    /// approver.
    #[test]
    fn sandbox_decision_table() {
        let no_probe = || -> bool { panic!("keyless configs must never probe") };
        assert_eq!(decide_sandbox(false, false, false, no_probe), SandboxDecision::Plain);
        assert_eq!(decide_sandbox(false, true, false, no_probe), SandboxDecision::Plain);
        assert_eq!(decide_sandbox(false, false, true, no_probe), SandboxDecision::Plain);
        assert_eq!(decide_sandbox(false, true, true, no_probe), SandboxDecision::Plain);

        // A working sandbox always wins: neither the override nor an
        // approver ever preempts or disables it.
        assert_eq!(decide_sandbox(true, false, false, || true), SandboxDecision::Sandboxed);
        assert_eq!(decide_sandbox(true, true, false, || true), SandboxDecision::Sandboxed);
        assert_eq!(decide_sandbox(true, false, true, || true), SandboxDecision::Sandboxed);
        assert_eq!(decide_sandbox(true, true, true, || true), SandboxDecision::Sandboxed);

        // Probe failed: the override silences the ask entirely; without it
        // an approver gets the Ask arm; without either, refuse.
        assert_eq!(decide_sandbox(true, true, false, || false), SandboxDecision::Plain);
        assert_eq!(decide_sandbox(true, true, true, || false), SandboxDecision::Plain);
        assert_eq!(decide_sandbox(true, false, true, || false), SandboxDecision::Ask);
        assert_eq!(decide_sandbox(true, false, false, || false), SandboxDecision::Refuse);
    }

    #[test]
    fn refusal_names_cause_interactive_ask_and_override() {
        assert!(SANDBOX_REFUSAL.contains("user namespace"));
        assert!(SANDBOX_REFUSAL.contains("allow_bash_without_key_sandbox"));
        assert!(SANDBOX_REFUSAL.contains("other tools stay guarded"));
        // T21: the wording leads with the interactive per-command ask and
        // keeps the config override as the final, non-interactive answer.
        assert!(SANDBOX_REFUSAL.contains("asks for per-command approval"));
        let ask = SANDBOX_REFUSAL.find("per-command approval").unwrap();
        let override_pos = SANDBOX_REFUSAL.find("allow_bash_without_key_sandbox").unwrap();
        assert!(ask < override_pos, "the ask must come before the override");
    }
}
