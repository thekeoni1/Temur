use super::{parse_input, Tool, ToolCtx, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 600_000;

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
