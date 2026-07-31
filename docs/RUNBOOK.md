# RUNBOOK - Tier-1 live smoke (operator steps)

> Scope note (T7): the "What the builder staged" section below covers
> **operator staging only**: the single i686 binary at `/home/dev/dist/temur`
> installed into the appsvc runtime. Multi-arch **release** artifacts are a
> separate flow staged under `/home/dev/dist/release/`, see "T7 release
> procedure" at the end of this file.

Everything here is a **human/operator** procedure. The build agent (`dev`) cannot and
must not perform these steps: it can't read the credential, and it deliberately can't
write the binary that `appsvc` executes (a builder-writable binary would nullify the
secret boundary).

## What the builder staged

- Binary: `/home/dev/dist/temur`
  (i686 musl-static ELF, release, stripped: the `i686-unknown-linux-musl`
  build gated by `scripts/check.sh`)
- sha256: recorded by the builder at each staging; verify before install with
  `sha256sum /home/dev/dist/temur` against the value reported for that build.

## 1. Install (root: `wsl -d Ubuntu -u root`)

```sh
install -o appsvc -g appsvc -m 755 /home/dev/dist/temur /srv/rustcode-runtime/bin/app
mkdir -p /srv/rustcode-runtime/work && chown appsvc:appsvc /srv/rustcode-runtime/work
```

## 2. Inject the real credential (root; once; see SETUP.md)

```sh
install -o appsvc -g appsvc -m 600 /path/to/real-credential /srv/rustcode-secrets/credential
```

The app reads it by path via `APP_SECRET_FILE` (set by `run-app.sh`). It never appears
in argv, env values, logs, or the process's own output. The app does not read
`ANTHROPIC_API_KEY` at all.

Optional config (model, max_tokens, thinking): appsvc's HOME is
`/srv/rustcode-runtime`, so the file is
`/srv/rustcode-runtime/.config/temur/config.json`, e.g.
`{"model": "claude-sonnet-5"}`. Defaults: claude-sonnet-5, 32000 max_tokens,
thinking off.

Provider selection (T2): `{"provider": "openai-compat", "openai_compat":
{"base_url": "http://127.0.0.1:8080/v1", "model": "<model-id>"}}` targets any
OpenAI-compatible endpoint (llama.cpp, Ollama, vLLM, LM Studio, or a hosted
compat API). Local endpoints need no credential: omit `api_key_file` and no
auth header is sent. A keyed endpoint reads its key from `api_key_file`, a
file path with the same isolation rule as `APP_SECRET_FILE` (never env, never
argv). The default provider remains `anthropic`; selecting the compat
provider leaves the Anthropic fields untouched.

## 3. One-time SSE capture (golden fixtures), then the smoke

Run the first turn with capture enabled so the raw wire streams get frozen into the
test suite (SSE bodies contain no credentials: the key travels only in a request
header, which is never written):

```sh
cd /srv/rustcode-runtime/work
runuser -u appsvc -- env APP_SECRET_FILE=/srv/rustcode-secrets/credential \
    /srv/rustcode-runtime/bin/app --capture-sse /tmp/oc-capture
```

**UI selection (since milestone B):** on a real terminal the app opens the
ratatui TUI by default; piped/scripted runs (and anything non-TTY) get the v1
line REPL automatically. For a transcript-friendly scripted smoke, force the
line REPL with `--plain`; `--tui` forces the TUI; `tui-probe` verifies the
terminal (alt screen + keys) without touching the API. Note one plain-REPL
change since v1: provider-level errors now print as `[!] provider error: …`
on **stdout** (previously stderr). Adjust any stderr-filtering scripts.

At the `>` prompt (line REPL) or in the TUI input line, run the Tier-1 smoke
script (one prompt per line):

1. `Create a file named smoke.txt containing the single line "tier1", then read it back to me.`
   - expect: `write` then `read` tool activity; the reply quotes `tier1`.
2. `Run the shell command "uname -m" and tell me the output.`
   - expect: `bash` tool activity; the reply contains `i686` (via linux32 personality)
   or `x86_64` (host kernel); either proves live execution; note which.
3. `Change smoke.txt so it says "tier1 passed" instead, and confirm by reading it.`
   - expect: `edit` tool activity; the reply confirms the new content.
4. `exit`

**Pass criteria (Tier 1):** coherent streamed responses; at least one full tool
round-trip per prompt; `smoke.txt` on disk ends up containing `tier1 passed`; usage
lines show nonzero input/output tokens; no `[!]` provider errors.

## 4. Return artifacts to the builder (root)

```sh
mkdir -p /home/dev/dist/return
cp /tmp/oc-capture.*.sse /home/dev/dist/return/
script -c ... # (if you ran under `script`, also copy the transcript)
chown -R dev:dev /home/dev/dist/return
rm -f /tmp/oc-capture.*.sse
```

The builder freezes the captures into `tests/fixtures/live/` and runs the parser
conformance test over them; any drift from the hand-authored fixtures becomes an
offline test failure from then on.

## T3 offline acceptance - recorded result

2026-07-19: `scripts/offline_demo.sh` passed end-to-end on the operator
machine (rootless podman, WSL2), first attempt:

- server image: `ghcr.io/ggml-org/llama.cpp:server-b10068` (854 MB on
  disk), ctx 8192, `--jinja`
- model: `/home/dev/models/Qwen3-1.7B-Q4_K_M.gguf` (from
  `unsloth/Qwen3-1.7B-GGUF`, 1.11 GB), the docs/OFFLINE.md primary
  recommendation validated as-is; the pre-authorized Q8_0 fallback was
  not needed
- isolation: pod created with `--network none`; in-pod `tls-probe`
  FAILED as required (negative assertion held)
- proof: the model drove a real `bash` tool call and `proof.txt`'s
  content was verified from the host (`offline-demo-ok`)
- incidental live validation: llama.cpp reported prompt/completion/cached
  usage and the never-reported cache-write field rendered as `—`
  (absent-vs-zero display working against a real local server)
- transcript kept at
  `/home/dev/temur-t3-offline-demo-transcript-2026-07-19.txt`

Re-run any time with
`MODEL_GGUF=/home/dev/models/Qwen3-1.7B-Q4_K_M.gguf scripts/offline_demo.sh`:
the script never pulls; if an image is missing it prints the exact pull
command and exits.

## T4 weak-model acceptance - recorded result

2026-07-20: **T4 acceptance met: `scripts/weak_model_eval.sh` scored 5/6
(threshold `EVAL_MIN=5`)** with the compact prompt profile, against
Qwen3-1.7B Q4_K_M under llama.cpp `server-b10068`, ctx 8192, in a
`--network none` pod. Final scored run:

| task | name | result | seconds |
|---|---|---|---|
| 1 | write-file | PASS | 33 |
| 2 | read-extract | FAIL | 38 |
| 3 | edit-config | PASS | 36 |
| 4 | bash-mkdir | PASS | 77 |
| 5 | find-needle | PASS | 143 |
| 6 | bump-and-copy | PASS | 39 |

Iterations (disclosed, per the pre-agreed protocol: prompts/wording only,
no model swap): the first run scored 4/6 with the eval config's
`max_tokens: 1024`: tasks 2 and 5 both truncated at max_tokens because
the model's streamed reasoning counts against the completion budget, with
zero tool calls completing. The generated config was raised to
`max_tokens: 2048` (comment recorded in the script) and task 2's wording
made stepwise after the original phrasing ("JUST the value… nothing
else") provably sent the model into a reasoning spiral. Task 5 then
passed. Task 2's remaining failure is an honest capability floor, not a
harness artifact: the model issued its read and write calls in ONE
parallel batch, writing token.txt before the read result existed, then
claimed success in prose (the host-verified assertion correctly failed,
model prose is never evidence).

Live captures, frozen the same day: 14 raw SSE streams in
`tests/fixtures/live-openai/` (11 with `--jinja`, 3 without), covering
plain text, a single tool call, a live repeated call, arguments
fragmented across many chunks, a tool-error round-trip, a live PARALLEL
three-call response, and post-result texts.
`tests/live_conformance_openai.rs` walks them strictly (per-chunk key
allowlists derived from these captures (including llama.cpp's
`reasoning_content` deltas and `timings` extension), sequence invariants,
and a zero-tolerance assembler pass) and is green.

No-jinja finding: llama.cpp `server-b10068` emitted fully STRUCTURED
`tool_calls` for Qwen3-1.7B even without `--jinja`: the wire shape was
identical to the jinja run (preserved in `oc-openai-nojinja.1.sse`). The
text-tool-call nudge therefore did NOT fire, correctly: nothing
prose-shaped ever reached the detector, so its literals remain validated
only by the offline unit tables, not live. docs/OFFLINE.md's "--jinja is
REQUIRED" guidance is retained for older builds and other models.

`scripts/offline_demo.sh` re-run as a regression on the T4 tree: PASSED
(and, incidentally, exercised the untouched Full-profile default path
live, since the demo's generated config sets no `prompt_profile`).

Eval harness note: all task seeding (data.txt, config.ini, the needle
files, version.txt) happens inside the script itself, per task, before
each temur launch, nothing is left to the operator.

## T19 - prose tool-call execution (T4 rule amendment record)

What changed versus T4. T4's rule was "prose is never parsed into an
execution": a tool call written as plain text was DETECTED and
answered with a corrective nudge, nothing more. T19 P3 narrows that
rule, deliberately and with operator approval, following the same
pattern as T17's amendment of T14's "init never accepts key
material": the agent loop, and only it, may now execute a prose tool
call, and only when it is unambiguous.

The exact contract (implemented in src/agent/recover.rs,
extract_prose_tool_call, and the EndTurn arm of Session::turn):

- The assistant message ended the turn (EndTurn) with ZERO structured
  tool calls: the existing nudge gate.
- The text contains exactly ONE candidate call, in a shape the T4
  detector already knows: a single <tool_call>...</tool_call> block,
  or the WHOLE trimmed message as a fenced / leading-brace JSON
  object. Two or more candidates, prose around a bare JSON object, or
  any other marker shape ([TOOL_CALL], <function_call>, ...) never
  execute; they nudge exactly as before.
- The inner JSON parses via repair_json as LOSSLESS (fence and
  trailing-comma repair fine; truncation completion NEVER executes),
  is an object naming a REGISTERED tool under "name"/"tool", with an
  OBJECT under "arguments"/"input"/"parameters".
- Execution goes through Registry::execute exactly like a structured
  call, so the T18 key guard, redaction, and the T19 context-scaled
  truncation all apply by construction.
- No tool_use id exists on the wire, so the result returns as a
  PLAIN USER TEXT message ("Result of the <name> tool call you wrote
  as text (executed by prose-call recovery): ...", errors likewise
  with an Error prefix). Request-body goldens are untouched; plain
  user text is wire-legal on both providers. No ToolEnd event is
  emitted (no stream opened a tool cell; the TUI's FIFO
  ToolStart/ToolEnd pairing holds); a Notice announces the execution
  and its outcome instead.
- FAILED prose executions count toward the existing per-turn
  NUDGE_LIMIT, so a model stuck on a failing prose call still
  terminates; successes reset nothing and are uncapped (the
  max_iterations ceiling still bounds the turn).
- Config: prose_tool_calls (container-level serde default true).
  false restores T4's detect+nudge behavior byte-identically.

Honest limits: prose executions bypass the doom-loop fingerprint
guards (those key on structured calls); the bound on a repeating
FAILING prose call is the nudge cap, and on a repeating SUCCEEDING
one the max_iterations ceiling. A candidate that extraction rejects
falls back to detection, so weaker shapes keep today's nudge.

## T5 acceptance - recorded result

2026-07-21: **live resume smoke PASSED, first attempt**: the full cycle
live turn → save → SIGKILL → `--continue` → seeded history on the wire,
end-to-end through main.rs, reusing the T3/T4 infrastructure unchanged
(llama.cpp `server-b10068`, Qwen3-1.7B Q4_K_M, ctx 8192, `--jinja`,
`--network none` pod; in-pod `tls-probe` FAILED as required before any
turn; musl-static binary readelf-verified in preflight; compact profile,
max_tokens 2048, the T4 eval settings).

- **Run 1** (two turns): the model drove a real `write` creating
  `milestone.txt` (content `t5-resume-proof`, host-verified) and a real
  `todowrite`. The session file appeared after turn 1: first-save
  behavior confirmed live. `kill -9` of the temur process mid-idle.
- **Survival (host-verified):** the file survived SIGKILL intact: parsed
  as JSON, version 1, 8 messages, the todo present, 1661 bytes. No
  `.tmp` litter (the kill was mid-idle; any would be harmless per below).
- **Run 2** (`--continue`): notice rendered exactly as
  `[!] resumed session: 8 messages, ~10769 tokens in / 699 out`,
  matching the file's contents, with zero mismatch lines (same
  provider/model/cwd, as expected). Asked *without tools* what file it
  had created earlier and with what text, the model answered
  `/work/milestone.txt` / `t5-resume-proof` from the seeded history
  alone; a live `todoread` returned the seeded todo. Session usage
  continued from the saved totals (10769+2850 in on the first resumed
  turn) rather than restarting from zero.
- **Post-exit re-validation:** the grown file parses, version still 1,
  8 → 14 messages, 1661 → 2627 bytes: save-after-resume produces a
  valid file, not just a larger one.
- Incidental: `cache_creation_input_tokens` (never reported by
  llama.cpp) round-tripped save → SIGKILL → load as JSON `null`, and both
  runs displayed it as `—`, absent-vs-zero preserved across persistence.

Artifacts (transcripts, the session file pre- and post-resume) kept at
`/home/dev/temur-t5-resume-smoke-2026-07-21/`.

## T5 sessions (operator notes)

Live runs persist the conversation after every turn (mock runs never do).
Location: `$XDG_STATE_HOME/temur/sessions/`, falling back to
`~/.local/state/temur/sessions/`; for the appsvc runtime identity (HOME
`/srv/rustcode-runtime`) that is
`/srv/rustcode-runtime/.local/state/temur/sessions/`. One file per working
directory (`<basename>-<hash>.json`); `--continue` from the same directory
resumes it. Config: `sessions_dir` relocates the directory,
`session_max_bytes` caps the file (default 4 MiB).

- **Power-cut semantics:** saves are write→fsync→rename atomic. The previous
  complete file is always intact; after a crash you resume the last fully
  saved turn. A leftover `*.json.tmp.<pid>` file is harmless litter from an
  interrupted save, safe to delete, never loaded.
- **Two processes, same directory:** safe against corruption: each writes
  its own pid-suffixed temp file and renames a complete file into place.
  Last writer wins; the sessions aren't merged.
- **Reset:** `rm` the directory's file from the sessions dir (the startup
  error for a corrupt or version-mismatched file names the exact path).
- **`--continue` fails fast** on a missing, corrupt, or wrong-version file
  rather than silently starting fresh; run without `--continue` to start a
  new session.

- `secret: APP_SECRET_FILE is not set` - run via `run-app.sh` or pass the env as above.
- `secret: cannot read credential file` - check ownership/mode (`appsvc:appsvc` 600)
  and that you're running as `appsvc`.
- `api error (HTTP 401)` - the credential file's contents are wrong (whole file is
  used, trimmed, as the API key).
- The build environment never performs this procedure; per project rules the live API
  is only ever touched here, by the operator, as `appsvc`.

## T6 interruption (operator notes)

Esc during a TUI turn interrupts it cooperatively (status row shows
`interrupting…`, then a `turn interrupted` notice). Effect on the session
file: the turn lands on a wire-valid boundary and the normal after-turn
save runs, so the file ends in one of three shapes: partial assistant
text; an assistant message whose tool calls are answered by synthesized
`[interrupted by user]` error results (kept on `--continue`); or, when
the interrupt landed before any content, the bare user prompt (dropped on
`--continue` with the usual notice). All three resume cleanly.

A running `bash` is killed with its whole process group within ~200 ms,
no orphaned children. Esc cannot reach a FULLY stalled stream (no frames
arriving); double-Ctrl+C force-quit (exit 130) remains the escape hatch
there, and the session file then simply holds everything up to the last
completed turn (the in-flight turn was never saved). The plain line REPL
has no interruption, a documented T6 exclusion.

## T6 acceptance - recorded result

2026-07-22: **live interrupt + fuzzy-edit smoke PASSED, all seven gated
checks (2a–2g)**, on the T3/T4/T5 infrastructure unchanged (llama.cpp
`server-b10068`, Qwen3-1.7B Q4_K_M, ctx 8192, `--jinja`, `--network none`
pod; in-pod `tls-probe` FAILED as required before any turn; compact
profile, max_tokens 2048). The musl binary was installed to
`/srv/rustcode-runtime/bin/app` from the Windows-side root path (RUNBOOK
§1) and sha256-verified identical to the gated staged copy
(`20c98142…c3888`); the pod executed that same hash-verified staged
binary (T5 precedent: the runtime copy is not mountable from dev's
rootless podman, and the hash tie is the point). TUI driven by scripted
keystrokes over the podman pty; sessions on a host-mounted state dir.

- **2a interrupt mid-stream:** Esc sent 10 s into streaming (mid-stream
  proven by transcript growth +21 KB and cross-checked against server
  timing). `interrupting…` state, `turn interrupted` notice, partial
  lighthouse-story tail and the ▣ turn tail all rendered; the prompt was
  back within **≤2 s** of Esc (a Ctrl+D landing-probe quit the app).
  Server side, llama logged `srv stop: cancel task` **114 ms** after the
  Esc stamp: this build notices the dropped connection immediately, so
  the "post-interrupt slot hangover" watch item did not materialize.
- **2b/2g session validity:** checked with a python3 script (jq is not
  installed on the host and installing needs elevation, same check
  content, disclosed substitution). Pre-resume file: parsed, wire-valid.
  Final file after all runs: 17 messages, **3 tool_use ids, every one
  answered by a tool_result in the immediately following message**; the
  file ends with run 5's bare user prompt, the documented empty-landing
  shape that `prepare_seed` drops on the next `--continue`.
- **2c resume:** `--continue` rendered the resume notice; the server
  accepted the seeded history including the interrupted partial text (no
  400, clean turn). The model answered from pre-interrupt session content
  (Elaris story details recalled without tools). Honest nuance: asked
  *which* story was interrupted, it recounted the completed first story
  rather than the interrupted lighthouse one (a 1.7B comprehension slip;
  the wire-level acceptance and cross-interrupt recall are what the step
  gates, and both held.
- **2d bash interrupt:** the model ran `sleep 60` via bash; /proc scans
  inside the app container show the forked pair `sh -c sleep 60` +
  `sleep 60` BOTH running pre-Esc and BOTH GONE at Esc+4 s with temur
  still alive: the I3 process-group kill proven live, no orphaned
  children. History landed exactly the designed shape: assistant
  tool_use answered by `(interrupted by user)` `is_error` tool_result;
  no post-Esc server request (the pre-POST token check held). The
  transcript shows `interrupting…` → ▣ ~30 bytes apart (landing within
  ~one render frame).
- **2e pre-first-byte (ran LAST as its own `--continue` session,
  disclosed sequencing deviation):** Esc 0.3 s after Enter, during
  prompt processing. Server logged `cancel task` **446 ms** after the
  Esc: b10068 emits an early chunk before prompt eval completes, so the
  blocked-read residual barely bites on this server. The residual stands
  as documented for servers that send nothing until the first token.
  Notice + tail rendered; app quit by Esc+6 s (probe granularity).
- **2f live fuzzy edit:** tab-indented `app.py` seeded; the model called
  edit with an UNINDENTED `oldString` (`value = 1`), a legitimate exact
  substring match, so the fuzzy fallback was correctly NOT consulted
  (output marker absent, `Edited /work/app.py (1 replacement(s))`
  byte-identical v1 shape). Host-verified: `value = 2` with tab
  indentation preserved. Recorded as the plan anticipated: the binding
  fuzzy proof is the E3 scripted e2e plus the builder mock smokes (which
  exercised the fallback in both the gnu and musl binaries).
- **Watch item (synthesized-result reaction):** in run 4 the model had
  the `(interrupted by user)` bash result in context and simply
  proceeded with the new task: no confusion, no spontaneous retry of
  the aborted command. First live data point: benign.
- **Harness observation (not a product signal):** scripted keystrokes
  through the podman-attach pty occasionally coalesce, and crossterm
  clears its whole parse buffer per event: a batched `EscEsc+Ctrl+D`
  yields the Esc and silently drops the Ctrl+D, so some landing-probes
  registered one probe-interval late (also seen once with no Esc
  involved at all). Characterized with three offline probes (lone
  late Ctrl+D at idle: quits in <1 s); app-side landing is evidenced
  frame-adjacent in every interrupted run. Real-terminal input does not
  batch this way; noted as a follow-up curiosity, not a defect.
- **Run 1a baseline (driver retry, disclosed):** the first 2a attempt's
  Esc fired after the turn had already completed (a wrong server-log
  grep plus the lone-ESC pty ambiguity, fixed by transcript-growth
  detection and the ESC-ESC encoding, which crossterm 0.28.1 parses as
  exactly one Esc). That run is kept as a bonus baseline: a full live
  turn streamed, completed naturally, and saved wire-valid.

Residuals unchanged from the offline record: a FULLY stalled stream
stays force-quit-only; the plain REPL has no interruption; pre-first-byte
latency equals one blocked read on silent-until-first-token servers.

Artifacts (driver scripts + stamped logs, raw TUI transcripts, session
files after 2a and after all runs, tls-probe output, /proc scan evidence
in the run-3 log, llama server log, wire-validity outputs) kept at
`/home/dev/temur-t6-interrupt-smoke-2026-07-21/`.

## T7 release procedure

### Building and gating (builder side)

`scripts/release.sh` is the release gate. It refuses to pass any red step:

1. Runs `scripts/check.sh` in full (the standing i686 acceptance gate,
   untouched by T7). `SKIP_CHECK=1` skips it for iteration only: a real
   release run never sets it.
2. Leak gate, two layers:
   - **Operator patterns file** at
     `${LEAK_PATTERNS:-$HOME/.config/temur-release/leak-patterns.txt}`,
     one case-insensitive extended regex per line, `#` comments allowed.
     It is machine configuration and must **never be committed**; ask the
     operator for its contents. **A missing file is a hard fail by
     design**, not a skip: a release without the leak gate is not a
     release. Patterns run over all tracked files (`git grep -i -E`) and
     all commit messages in history (`git log --all -i --grep`).
   - An embedded generic key-shape scan (`sk-ant-`, AWS `AKIA…`, GitHub
     `ghp_…`, `PRIVATE KEY` blocks) that always runs.
3. Per target (`i686-unknown-linux-musl`, `x86_64-unknown-linux-musl`,
   `aarch64-unknown-linux-musl`, `armv7-unknown-linux-musleabihf`:
   release build, staticness (no INTERP, no NEEDED, `file` reports
   static), ELF class/machine match, armv7 `Tag_ABI_VFP_args: VFP
   registers` (blocking), and `--version` must equal the Cargo.toml
   version on every runnable binary (x86 natively, ARM via
   `qemu-*-static` when present).
4. Stages **bare binaries** (no tarballs) named
   `temur-v<ver>-<full-rust-triple>` plus `SHA256SUMS` (native
   `sha256sum` format, bare filenames, so `sha256sum -c
   --ignore-missing` works on a single downloaded artifact) at
   `/home/dev/dist/release/v<ver>/`, and self-verifies the sums.

The version is extracted from Cargo.toml and asserted against the
binaries' own `--version`, so tag, filename, and binary can never skew.

### Publish preflight (operator-gated: nothing is pushed without it)

All of the following, in order, before any tag/push/release:

- `git status` clean and synced with origin/main.
- `scripts/release.sh` green **at the exact HEAD being tagged**.
- Installer test green: serve `/home/dev/dist/release/v<ver>/` locally
  (`python3 -m http.server`) and run `scripts/install.sh` with
  `TEMUR_BASE_URL` pointing at it, into a temp `HOME`; installed
  `temur --version` must match.
- `gh auth status` OK (operator runs `gh auth login` if not).
- Repo visibility checked (`gh repo view --json visibility`): the
  closing gate (README one-liner) only works with the repo **public**;
  raw/release URLs 404 otherwise. Make it public before the gate (or
  accept a deliberate 404 and re-run the gate after flipping).

Then: annotated tag `v<ver>`, push main + tag, `gh release create` with
the four binaries + SHA256SUMS, and a live verification of the README
one-liner into a temp `HOME` (live download, live checksum, live
install, version match). Record the result here.

## T7 release acceptance - recorded result

2026-07-23: **v0.1.0 published and live-verified.** Annotated tag `v0.1.0`
(at `703a1bc`) pushed with main; GitHub release "temur v0.1.0" created
with the four gated binaries + `SHA256SUMS`:
<https://github.com/thekeoni1/Temur/releases/tag/v0.1.0>. Preflight at
the exact tagged HEAD: clean synced tree, full `release.sh` green
(`check.sh` ALL CHECKS PASSED, leak gate clean over tracked files and
all commit-message history, 4/4 artifacts gated, all four `--version`
asserted: x86 natively, ARM via qemu).

Closing gate: the README one-liner run **verbatim** into a temp `HOME`:
live download of `scripts/install.sh` from the `v0.1.0` tag, live
artifact + `SHA256SUMS` download from the release, checksum verified,
installed `temur --version` printed `0.1.0`, and the installed binary's
sha256 matched the staged `SHA256SUMS` entry byte-for-byte.

Sequencing note (disclosed): at the first closing-gate attempt the repo
was still private, so the raw one-liner URL 404ed: the installer is
fail-closed and installed nothing. The operator made the repo public
(`gh repo edit --visibility public`; gh 2.45.0 predates the newer
confirmation flag) and the gate re-ran green. ARM binaries remain
verified at build level + qemu-user per the ROADMAP T7 as-built note;
real-hardware smoke stays an open follow-up.

Post-verification state (deliberate): after the closing gate passed, the
operator returned the repo to **private** the same day. The release URL
above and the README one-liner therefore 404 for non-collaborators
**by design** until the repo is made public again; that is a pending
publication decision, not a broken release. The preflight visibility
check above exists so the next run handles this consciously.

## v0.1.1 - post-release review fixes (release procedure delta)

v0.1.1 fixes the 10 verified findings from the post-release code review
of the T6+T7 range (F1–F10; per-finding as-built summary in ROADMAP §3,
"v0.1.1"). Two new committed test scripts join the gates:

- `scripts/install_test.sh [staged-dir]` - the installer matrix: pass /
  corrupted-artifact / unlisted-artifact, each on the GNU host (curl,
  file:// mirrors) AND inside `busybox:stable` (busybox sh + wget +
  sha256sum over busybox httpd). The busybox column fails verbatim on
  the v0.1.0 installer (F2's GNU-only flags): it is the reason this
  release exists.
- `scripts/sigint_test.sh [binary]` - plain-REPL SIGINT black box (F4):
  single Ctrl+C mid-bash-turn lands the turn with no orphaned child in
  /proc and a clean exit on EOF; a second Ctrl+C exits 130.

**DECIDED (operator, at planning): v0.1.1 is tagged and released while
the repo stays PRIVATE.** The publish preflight above applies unchanged
EXCEPT the repo-visibility bullet: the closing gate for a private
release is `gh release download` (authenticated) → checksum verify →
`scripts/install.sh` against the downloaded artifacts via
`TEMUR_BASE_URL` → installed `temur --version`. The PUBLIC one-liner
gate is explicitly deferred to the visibility flip and recorded as the
one open item when it happens.

## v0.1.1 release acceptance - recorded result

2026-07-23: **v0.1.1 published (repo PRIVATE by decision) and
closing-gate verified.** Annotated tag `v0.1.1` (at `fa702a2`) pushed
with main (`a3220d6..fa702a2`); release "temur v0.1.1" created with the
four gated binaries + `SHA256SUMS`:
<https://github.com/thekeoni1/Temur/releases/tag/v0.1.1> (404s for
non-collaborators while private, by design). Preflight at the exact
tagged tree: clean, full `release.sh` green (check.sh ALL CHECKS PASSED
both paths, leak gate clean over tracked files + all commit-message
history, version/target skew gate OK, 4/4 artifacts gated, all four
`--version` asserted: x86 natively, ARM via qemu), installer matrix
6/6 (host + busybox), SIGINT matrix 2/2, `gh auth` OK, visibility
confirmed PRIVATE pre-push.

Closing gate (private variant, run in a fresh temp dir + temp HOME):
authenticated `gh release download v0.1.1` → `sha256sum -c SHA256SUMS`
all four OK → `scripts/install.sh` against the downloaded artifacts via
`TEMUR_BASE_URL` (fetched, verified, installed) → installed
`temur --version` printed `0.1.1` → installed binary's sha256 equals
the downloaded AND locally-staged `SHA256SUMS` entries byte-for-byte
(`a901884f…be3f6`, x86_64 artifact).

**OPEN ITEM (the only one): the PUBLIC one-liner gate.** When the
operator flips visibility, run the README one-liner verbatim into a
temp `HOME` (live raw-URL download of install.sh at the `v0.1.1` tag,
live artifact + SHA256SUMS from the release, checksum verified,
`--version` 0.1.1) and record the result here.

## T8-P1 acceptance - recorded result (no release)

2026-07-25: **T8-P1 (slash commands + named-profile switching) landed**
across five gated commits (P1 config profiles → P2 build_live +
Session seam → P3 command layer → P4 TUI rendering → P5 docs); full
`check.sh` (both paths) green at every phase; no tag, no version bump:
T8 releases as v0.2.0 when the milestone completes.

Live verification, scripted llama.cpp proof (build session, local
server only: cached `server-b10068` image, Qwen3-1.7B Q4_K_M, two
keyless profiles at the same endpoint with distinct nicknames/model
strings/max_tokens; isolated XDG config/state dirs). Transcript
excerpts, verbatim:

```
temur 0.1.1 (model=qwen-nickname-a, thinking=false)
>   [!] response truncated: max_tokens reached
>   [!] switched to qwen-b (openai-compat · qwen-nickname-b)
>   [!] profile: qwen-b
  [!] provider: openai-compat · model: qwen-nickname-b
  [!] thinking: off · max_tokens: 640
  [!] context: ~3161 of 8192 tokens used
> banana
  (turn: 2666 in / 259 out, cache read 2649 ...)
>   [!] session cleared
```

Then `--continue`:

```
  [!] resumed session was recorded with model "qwen-nickname-b"; this
      run uses "qwen-nickname-a" — continuing
  [!] resumed session: 0 messages, ~— tokens in / — out
```

Reading of the evidence: startup profile applied (banner model =
nickname-a); the switch confirmation and post-switch `/status` show the
second profile active with its own max_tokens; the second turn's
`cache read 2649` proves the full pre-switch history rode along to the
server; `/clear` + `--continue` resumes EMPTY, and the saved file on
disk records the POST-switch provider/model (`"model":
"qwen-nickname-b", "history":[]`), the advisory mismatch notice on
resume is the documented, correct behavior. Turn 1's truncation is
Qwen3 spending its 512-token budget on thinking: model behavior, not
a switching defect; turn 2 (640 tokens) answered as instructed. The
anthropic-switch path is proven by mock/fixture tests (atomic failed
switch through the real `build_live` with an unreadable key file and
with `APP_SECRET_FILE` unset); the operator dogfoods a real
local→sonnet switch with their own key file.

Environment note for future gate runs, RETIRED 2026-07-25 by T8-P2:
`check.sh` itself now isolates every host-side product invocation with
per-run `XDG_CONFIG_HOME`/`XDG_STATE_HOME` temp dirs, and
`tests/sigint.rs` is in the container suite list, so gate runs need no
workaround env regardless of what the operator's real config selects.
(Historical context: during T8-P1 the host TUI pty smoke read the real
`~/.config/temur/config.json` and failed whenever it selected
openai-compat; those gate runs used a neutral `XDG_CONFIG_HOME` by
hand. The failure mode was reproduced once more before the fix and
proven gone after it: a full no-workaround check.sh run is green with
the operator's openai-compat config in place.)

## T8-P2 acceptance (2026-07-25)

Markdown rendering + styling pass: full `check.sh` (gnu-debug +
musl-release paths, 26 container-suite results, REPL/TUI/pty smokes,
busybox) green at every sub-phase gate with no workaround env. The
representative markdown sample (heading + inline code + styled list +
fenced block) is frame-asserted at two widths and end-to-end through
the headless seam over `tests/fixtures/markdown_sample.sse`; the
severed-fence limitation is pinned in
`severed_fence_across_cells_renders_without_panic` (tests/tui.rs).

## T8-P3 acceptance (2026-07-25)

`scripts/serve.sh` (background llama.cpp launcher, `start|stop|status`)
live-verified end to end on the dev machine, model
`Qwen3-1.7B-Q4_K_M.gguf`, pinned image `server-b10068`:

- cold `start` → `/health` OK inside the 30×2s budget; prints the
  base_url match hint.
- `status` while healthy → exit 0 with container/image/model/port
  summary.
- One-window proof: a real keyless temur turn (`--plain`, isolated XDG,
  musl-static host binary) through the published port: the model ran
  `echo serve-one-window-ok > proof.txt` via the bash tool and the file
  content was verified from the host.
- `start` while running → "OK: already running", exit 0, still exactly
  one container.
- `stop` → removed; `status` → "not running", exit 1; `stop` again →
  "OK: not running", exit 0 (idempotent both ways).
- `MODEL_GGUF=/nonexistent start` → preflight FAIL, exit 1, no
  container created; unknown/missing subcommand → usage, exit 2.
- Final `podman ps -a` → no `temur-llama` residue.

Untested live (desk-checked only): the health-timeout branch (logs
tail, fail-closed `rm -f`, exit 1), forcing an honest 60s health
failure with a real model was not practical.

Live testing caught a real environment collision: WSL exports
`NAME=<hostname>`, so the planned `NAME` knob silently named the
container after the machine's hostname; the knob shipped as
`CONTAINER_NAME`. Full `check.sh`
(both paths) green at the P1 and P2 gates. No release: version stays
0.1.1 until the v0.2.0 close-out.

## v0.2.0 - T8 close-out (release procedure delta)

v0.2.0 ships the T8 milestone (slash commands + profiles, TUI markdown,
serve.sh; as-built notes in ROADMAP §T8). Bump + docs + gates + publish
only: no product code changes, no new dependencies, no new gates.

**DECIDED (operator, at launch): v0.2.0 is tagged and released while
the repo stays PRIVATE**, repeating the v0.1.1 flow verbatim: the
v0.1.1 procedure delta above applies unchanged, including the private
closing gate (`gh release download` → `sha256sum -c` → `install.sh` via
`TEMUR_BASE_URL` into a temp HOME → `--version` → installed-binary
sha256 equals both the downloaded and locally staged `SHA256SUMS`
entries). The PUBLIC one-liner gate remains **the** open release item,
deferred to the visibility flip exactly as recorded for v0.1.1: when
the flip happens, run it for the newest released tag.

Sequencing note (unchanged from v0.1.1, made explicit): the skew gate
reads the working tree and the RUNBOOK requires `release.sh` green at
the exact head being tagged, so the bump commit and this docs commit
land BEFORE the gate run, and the tag points at that head.

## v0.2.0 release acceptance - recorded result

2026-07-25: **v0.2.0 published (repo PRIVATE by decision) and
closing-gate verified.** Annotated tag `v0.2.0` ("temur v0.2.0 — T8
daily-driver UX") at head `104a629`, pushed with main
(`35258c2..104a629`); release "temur v0.2.0" created with the four
gated binaries + `SHA256SUMS`:
<https://github.com/thekeoni1/Temur/releases/tag/v0.2.0> (404s for
non-collaborators while private, by design).

Preflight: tree clean at `35258c2` with origin/main already synced;
`ANTHROPIC_API_KEY` absent; `gh auth` OK (repo scope); visibility
confirmed PRIVATE before and after publish; both qemu-user statics on
PATH; operator leak-patterns file present. The bump touched exactly the
six pinned sites; Cargo.lock regenerated via `cargo update -p temur`.

Gate results at the tagged head (no env overrides, check.sh under a
pty): full `check.sh` ALL CHECKS PASSED both paths; leak gate clean
(operator patterns + generic shapes, tracked files + all commit
messages); skew gate "install.sh + README match version 0.2.0 and all
targets"; `== RELEASE v0.2.0: 4/4 ARTIFACTS GATED ==` with all four
`--version` asserts printing `temur 0.2.0` (i686 + x86_64 native,
aarch64 + armv7 via qemu); SHA256SUMS self-verify 4/4 OK. Installer
matrix 6/6 (host + busybox, pass/corrupt/unlisted). Local-mirror test:
`python3 -m http.server` over the staged dir, `install.sh` via
`TEMUR_BASE_URL` into a temp HOME → checksum verified → installed
`temur --version` printed `0.2.0`.

One deviation, resolved in-cycle: the FIRST release.sh run FAILED at
the leak gate: the T8-P3 acceptance note above contained the literal
machine hostname, matching an operator leak pattern (text committed
with T8-P3, before this cycle; the pattern predates the commit). The
hostname appeared in exactly one tracked line and in no commit
message, so no history rewrite was needed: scrub commit `104a629`
reworded the line, and the full gate re-ran green at that head, which
is the head tagged. The gate did its job.

Closing gate (private variant, fresh temp dir + temp HOME):
authenticated `gh release download v0.2.0` → `sha256sum -c SHA256SUMS`
all four OK → `scripts/install.sh` against the downloaded artifacts
via `TEMUR_BASE_URL` (fetched, verified, installed) → installed
`temur --version` printed `0.2.0` → installed binary sha256 equals the
downloaded AND locally-staged `SHA256SUMS` entries byte-for-byte
(`053a5227…ce53`, x86_64 artifact).

**OPEN ITEM (unchanged, the only one): the PUBLIC one-liner gate.**
When the operator flips visibility, run the README one-liner verbatim
into a temp HOME (live raw-URL download of install.sh at the newest
released tag, live artifact + SHA256SUMS from the release, checksum
verified, `--version` matches) and record the result here.

## T9 acceptance - recorded result (no release)

2026-07-25: **T9 (command ergonomics) feature-complete on main**: four
phases, one commit each (P1 per-profile prompt profiles, P2 `/models` +
raw-id `/model`, P3 TUI command styling + Tab completion, P4 serve.sh
MODEL_GGUF default + docs). **No tag, no release, no version bump:
the version stays 0.2.0**; T9 ships later as v0.3.0 after dogfooding.

Gates: full `check.sh` (pty, both paths, no env overrides) ALL CHECKS
PASSED at the P0 baseline (head `241c2fe`, clean tree) and again at
every phase gate before its commit. Host suites 16/16 green at each
phase; container suites 13/13 per path (26 `test result: ok` per gate
log), staticness asserts (no INTERP / no NEEDED), mock REPL smokes
(anthropic + openai-compat wire), TUI pty smokes (host + container +
musl), and the bare-busybox checks all green in every run.

Live verification (offline-only, llama.cpp via `scripts/serve.sh`;
never Anthropic: the anthropic listing path is covered by the
parse_models_json unit tests against the documented wire shape, and
the operator dogfoods real Anthropic `/models` after acceptance):

- serve.sh default: `scripts/serve.sh start` with MODEL_GGUF unset
  printed `OK: defaulted MODEL_GGUF=` + the single `.gguf` under
  `$HOME/models` in preflight (the server was already up from earlier
  dogfooding, so start took the healthy already-running path). Both
  FAIL shapes exercised for real via `MODELS_DIR` pointed at prepared
  dirs: zero files → "found 0 .gguf files, need exactly 1 to default";
  two files → the same FAIL naming the dir and "found 2".
- Prompt swap: two keyless openai-compat profiles at the same server,
  `localc` (compact) and `localf` (full), startup on `localc`.
  `/status` showed `prompt: compact`; after `/model localf`, `prompt:
  full`, and the next live turn's 6.7k input tokens are consistent
  with the full tool text being on the wire.
- `/models` listed the server's id verbatim (count line + `/model.gguf`,
  exactly what the server's `data[].id` reports).
- `/model /model.gguf` (raw id) switched with `switched model to
  /model.gguf (openai-compat · profile settings kept)`; a live turn
  completed after the switch; `/status` then showed the profile line
  unchanged (`profile: localf`) with the new model line, and the saved
  session file recorded `"model": "/model.gguf"`.

Tab completion and the styling/hint behaviors are proven headless (a
scripted Tab through the real render loop completes and submits
`/status`) plus buffer-level style probes; pty smokes in check.sh
cover the terminal path as always.

Plain-REPL compatibility: all pre-T9 output shapes are byte-identical
except the two deliberate T9 surface changes (`/status` gained the
`· prompt:` field; `/help` is now derived from the COMMANDS table and
includes `/models`), both pinned by updated tests.

## T10 acceptance - recorded result (no release)

2026-07-26, builder session. T10 (session management: named
multi-session, /sessions + /resume + /new, --resume, rendered
backscroll) verified feature-complete at the P5 gate. No tag, no
release, no version bump: T10 ships with T9 as v0.3.0 after
dogfooding.

Gates: full `scripts/check.sh` (both paths: gnu-debug and
musl-release, host + i386 container + busybox bare, pty TUI smokes)
green after every phase P1–P4 and again at P5 on the final tree.

Live verification (offline llama.cpp via `scripts/serve.sh`, Qwen3-1.7B
Q4_K_M, keyless openai-compat, MUSL RELEASE binary, isolated
XDG_STATE_HOME/XDG_CONFIG_HOME under /home/dev/t10-live):

- Default session two live turns in proj-a; `/new alpha` printed
  `new session "alpha" — the file is created on the first turn`, and
  the named file appeared only after the next turn's save. `/status`
  showed `session file: …-alpha.json · session: alpha`.
- `/sessions` listed both with derived titles and the active star:
  `* (default) · /home/dev/t10-live/proj-a · 4 msg(s) ·
  proj-a-573ab248a33104e8.json · Reply with exactly the word ALPHA
  and nothing else. /no_thin…` (60-col ellipsis); after
  `/resume alpha` the star moved to alpha's line.
- `/resume alpha` rendered backscroll (`> Reply with exactly the word
  GAMMA…` + the reply) then `[!] resumed session: 2 messages, ~6680
  tokens in / 8 out`; `/resume proj-a-573ab248a33104e8.` (file-name
  prefix) switched back to the default with its full two-turn
  backscroll.
- TUI: `--tui --continue` over a pty (script(1), 24x100) showed the
  resumed backscroll in the live alternate screen (header title =
  first prompt, `▌`-bar user blocks, replies, yellow resumed-session
  notice), proving --continue still resumes the DEFAULT session even
  with a named sibling present.
- Cross-project (from proj-b): `/sessions` listed all three sessions
  newest-first; `/resume alpha` (globally-unique name) worked and
  printed `session was recorded in /home/dev/t10-live/proj-a; tools
  run in the current directory /home/dev/t10-live/proj-b`. Startup
  `--resume alpha` did the same with the T5 cwd mismatch advisory.
- CLI shapes, all exit 1 with clean messages: `--continue --resume`
  (mutually exclusive), `--resume` + `--mock` (unavailable),
  `--resume zzz` (`no saved session matches "zzz"`), `--resume` over
  an empty sessions dir (helpful "nothing to --resume yet"), and
  `--resume proj-a-` (ambiguous: both candidate files listed with
  their cwds).
- On disk: the named file carries `"name":"alpha"`; the default file
  has NO name key, byte-shape identical to pre-T10 (version still 1;
  FNV filenames unchanged, pinned by goldens).

Plain-REPL compatibility: pre-T10 output shapes byte-identical except
the deliberate `/status` session-file line extension; the resume
summary kept its exact `[!]` rendering, now preceded by backscroll.

## v0.3.0 - T9+T10 close-out (release procedure delta)

What ships: T9 command ergonomics (per-profile prompt profiles, /models
listing + raw-model-id switching, TUI command styling + Tab completion,
serve.sh single-gguf MODEL_GGUF default) and T10 session management
(named multi-session per project, /sessions + /resume + /new, --resume
CLI, rendered backscroll on resume and --continue; session format
unchanged, FORMAT_VERSION 1, compat both directions).

Procedure deltas vs v0.2.0:

- **CHANGELOG.md introduced** (repo root, newest first, retroactive
  0.1.0..0.2.0 plus the unreleased 0.3.0 entry). From v0.3.0 on the
  release body derives from the matching CHANGELOG section instead of
  being written ad hoc at publish time.
- **Em-dash sweep (operator-decided 2026-07-26):** all tracked repo
  markdown was rewritten without em-dashes, meaning-preserving, line by
  line. Byte-exact carve-out kept for verbatim quotes of immutable
  artifacts; the retained lines are exactly: docs/OFFLINE.md:239 and
  :301 plus docs/RUNBOOK.md:120 and :217 (the quoted `fmt_tokens`
  absent-usage glyph), docs/RUNBOOK.md:536-537 (fenced --continue
  transcript excerpt), docs/RUNBOOK.md:635 (the v0.2.0 tag annotation),
  docs/RUNBOOK.md:751 (the quoted /new notice string), and
  docs/TUI.md:98 and :100 (the quoted status-row hint format and the
  quoted unknown-command hint). Source, script, and test em-dashes are
  out of scope this cycle (no-source-changes rule) and remain a
  possible later sweep.
- **DECIDED (operator, 2026-07-26): PRIVATE release again,** repeating
  the v0.1.1/v0.2.0 procedure; the PUBLIC one-liner gate stays deferred
  to the visibility flip.
- **Tag and publish held until operator dogfood sign-off:** stage 1
  stops at gated artifacts (full release.sh + installer matrix over the
  close-out commits, artifacts staged under
  /home/dev/dist/release/v0.3.0), with NO tag, NO push, and NO GitHub
  release. Stage 2 (tag at the dogfooded head, push, private release,
  closing gate, acceptance record) runs as a separate prompt after
  sign-off. New vs v0.2.0, where the tag followed the gates the same
  day.

## v0.3.0 release acceptance - recorded result

2026-07-26: **v0.3.0 published (repo PRIVATE by decision) and
closing-gate verified.** Annotated tag `v0.3.0` ("temur v0.3.0 -
command ergonomics + session management (T9+T10)") at head `e74d2c6`,
main pushed in two stages (stage 1 `8b5615b..7f34169`, then the
CHANGELOG dating commit `7f34169..e74d2c6`) and the tag pushed after
dogfood sign-off; release "temur v0.3.0" created with the four gated
binaries + `SHA256SUMS`:
<https://github.com/thekeoni1/Temur/releases/tag/v0.3.0> (404s for
non-collaborators while private, by design).

Preflight: tree clean at `7f34169` with origin/main synced;
`ANTHROPIC_API_KEY` absent; `gh auth` OK (repo scope); visibility
confirmed PRIVATE before and after publish; no v0.3* tag existed
before this cycle's tag. The stage-1 bump touched exactly the six
pinned sites; Cargo.lock regenerated via `cargo update -p temur
--offline` (temur entry only).

Gate results at the tagged head `e74d2c6` (no env overrides, run under
a pty; the full gate was re-run at this exact head after the CHANGELOG
dating commit, per the release rule): full `check.sh` ALL CHECKS
PASSED both paths; leak gate "OK: leak grep clean (operator patterns +
generic shapes, files + history)"; skew gate "OK: install.sh + README
match version 0.3.0 and all targets"; `== RELEASE v0.3.0: 4/4
ARTIFACTS GATED ==` with all four `--version` asserts printing
`temur 0.3.0` (i686 + x86_64 native, aarch64 + armv7 via qemu);
SHA256SUMS self-verify 4/4 OK. Installer matrix 6/6 (host + busybox,
pass/corrupt/unlisted).

Dogfood sign-off: the operator dogfooded T9+T10 on 2026-07-26 and
signed off the same day. One finding, deferred to the multi-model
milestone: small-model indirect tool selection (not a temur defect).

Procedure deltas this cycle (recorded above in "v0.3.0 - T9+T10
close-out"): the tag and release were HELD after the stage-1 gate run
until dogfood sign-off, new vs v0.2.0; the release body derives from
CHANGELOG.md (introduced this cycle); all repo markdown is now
em-dash-free outside the byte-exact verbatim-quote carve-out. No
in-cycle scrubs were needed: the leak gate passed first try in both
stages.

Closing gate (private variant, fresh temp dir + temp HOME):
authenticated `gh release download v0.3.0` (x86_64 artifact +
SHA256SUMS) → `sha256sum -c --ignore-missing SHA256SUMS` OK → the
downloaded binary run with the temp HOME printed `temur 0.3.0` → the
downloaded binary's sha256 equals the locally-staged artifact's
byte-for-byte (`fb86e2041e152263…`, x86_64 artifact).

**OPEN ITEM (unchanged, the only one): the PUBLIC one-liner gate.**
When the operator flips visibility, run the README one-liner verbatim
into a temp HOME (live raw-URL download of install.sh at the newest
released tag, live artifact + SHA256SUMS from the release, checksum
verified, `--version` matches) and record the result here.

## T11 acceptance - recorded result (no release)

Recorded 2026-07-26. Feature work only, version stays 0.3.0: no tag, no
release, no push. Gates per phase: full check.sh (both paths) after P1,
P2, P4, and P5; cargo test for the P2 prompt caps (bash 766 of 1000
chars; the compact description total stays under the 8192-byte budget,
5804 by the test's own sum); sh -n for the P3 probe with its live run
folded into P5. Quoted program output below is byte-exact (the em-dash
carve-out applies); fixture paths live under the session scratchpad,
abbreviated here as `$S`.

### P1 serve.sh selection matrix (live, verbatim)

Fixture dirs: `$S/mzero` (empty), `$S/mmulti` (three dummy ggufs:
Llama-Tiny 1M, Qwen2.5-Coder-Test 2M, Qwen3-4B-Test 3M), fake meminfo
with `MemAvailable: 2048 kB`. The real `$HOME/models` held exactly one
gguf (Qwen3-1.7B) at matrix time.

(a) zero-gguf, no arg (`MODELS_DIR=$S/mzero`), exit 1:

```
FAIL: no .gguf files in $S/mzero; set MODEL_GGUF=/path/to/model.gguf or pass a model name
```

(b) one-gguf, no arg (real dir), part of the live cycle below:

```
OK: defaulted MODEL_GGUF=/home/dev/models/Qwen3-1.7B-Q4_K_M.gguf
```

(c) multi-gguf, no arg (`MODELS_DIR=$S/mmulti`), exit 1:

```
FAIL: 3 .gguf files in $S/mmulti, need exactly 1 to default; set MODEL_GGUF=/path/to/model.gguf or pass a model name:
    Llama-Tiny.gguf  (1M)
    Qwen2.5-Coder-Test.gguf  (2M)
    Qwen3-4B-Test.gguf  (3M)
```

(d) arg exact match, case-insensitive, with and without the `.gguf`
suffix (`start QWEN3-1.7B-Q4_K_M` and `start qwen3-1.7b-q4_k_m.gguf`),
both exit 0:

```
OK: selected /home/dev/models/Qwen3-1.7B-Q4_K_M.gguf
```

(e) arg unique substring (`start coder` in `$S/mmulti`), exit 0:

```
OK: selected $S/mmulti/Qwen2.5-Coder-Test.gguf
```

(f) arg ambiguous (`start qwen` in `$S/mmulti`), exit 1, matches marked:

```
FAIL: 'qwen' is ambiguous in $S/mmulti (2 matches, marked *); candidates:
    Llama-Tiny.gguf  (1M)
  * Qwen2.5-Coder-Test.gguf  (2M)
  * Qwen3-4B-Test.gguf  (3M)
```

(g) arg no match (`start mistral` in `$S/mmulti`), exit 1:

```
FAIL: no .gguf in $S/mmulti matches 'mistral'; candidates:
    Llama-Tiny.gguf  (1M)
    Qwen2.5-Coder-Test.gguf  (2M)
    Qwen3-4B-Test.gguf  (3M)
```

(h) `MODEL_GGUF` plus arg, exit 1:

```
FAIL: both MODEL_GGUF and a model name argument are set; choose one, not both
```

(i) WARN with `MEMINFO` pointing at the fake file, then CONTINUE (the
run proceeded into the already-running short-circuit), as printed:

```
WARN: model 1.0 GiB + overhead 1.0 GiB at ctx 8192 exceeds available 0.0 GiB RAM; expect thrashing or OOM
OK: image and model present (nothing will be pulled)
OK: already running
```

(j) no WARN normally: the live cycle below printed no WARN line.

Live cycle (real 1.7B model): `start` defaulted the model, reached
healthy, and printed the base_url hint; `status` reported healthy with
the mounted model; cases (d), (e), (i) then exercised selection against
the running server (selection resolves before the short-circuit);
`stop` removed the container. Verbatim start tail:

```
OK: llama.cpp serving /home/dev/models/Qwen3-1.7B-Q4_K_M.gguf on http://127.0.0.1:8080/v1
  matches temur's default base_url — a keyless openai-compat profile needs no base_url
```

### P5 per-model eval runs (compact profile, server-b10068, ctx 8192)

Downloads: `unsloth/Qwen3-4B-Instruct-2507-GGUF` Q4_K_M (2,497,281,120
bytes) and `unsloth/Qwen2.5-Coder-3B-Instruct-GGUF` Q4_K_M
(1,929,902,496 bytes) into `$HOME/models`, making the dir genuinely
multi-gguf (three files, kept on disk). Free RAM before sizing: 6.3 GiB
available of 7.6 total, so both candidates fit one at a time. Per
model: `serve.sh start <name>` (live selection), `status`, `stop`, then
the eval (its own pod; the serve.sh server was stopped first so two
copies of a model are never resident together, a deliberate reorder of
the listed start/eval/stop sequence).

Qwen3-1.7B (baseline re-run with the P2 hint in play), SCORE 7/7:

```
task name           result seconds
---- -------------- ------ -------
1    write-file     PASS   36
2    read-extract   PASS   82
3    edit-config    PASS   43
4    bash-mkdir     PASS   104
5    find-needle    PASS   120
6    bump-and-copy  PASS   40
7    indirect-delete PASS   20
```

Task 7 transcript core (the P2 hint appears to have closed the T11
dogfood gap; pre-T11 the same model refused "delete the file"):

```
>   → bash
  ✓ bash: rm -f obsolete.tmp
The file `obsolete.tmp` has been successfully deleted. You can verify the removal by checking the current directory contents.
```

Qwen3-4B-Instruct-2507 (`serve.sh start qwen3-4b` selected it live),
SCORE 7/7, notably faster per task than the 1.7B:

```
task name           result seconds
---- -------------- ------ -------
1    write-file     PASS   47
2    read-extract   PASS   16
3    edit-config    PASS   17
4    bash-mkdir     PASS   10
5    find-needle    PASS   42
6    bump-and-copy  PASS   16
7    indirect-delete PASS   8
```

Task 7 transcript core:

```
>   → bash
  ✓ bash: rm obsolete.tmp
```

Qwen2.5-Coder-3B-Instruct (`serve.sh start coder` selected it live),
SCORE 0/7. Every task failed the same way: the model chose the RIGHT
tool, including bash with rm on the indirect probe, but emitted every
call as a fenced JSON block instead of a structured tool call on this
stack; temur's prose-tool-call detection asked for the tool interface,
the model repeated the prose, and the turn ended. Wire format, not
reasoning. Task 7 transcript core (outer fence elided from the quote;
the model's output was a ```json code block):

```
{
  "name": "bash",
  "arguments": {
    "command": "rm obsolete.tmp"
  }
}
  [!] the model wrote a tool call as plain text; asked it to use the tool interface
```

### Shortlist outcome

Verified 2026-07-26: Qwen3-1.7B yes/yes (7/7), Qwen3-4B-Instruct-2507
yes/yes (7/7), Qwen2.5-Coder-3B-Instruct no/n-a (0/7, prose-only tool
calls). Qwen2.5-Coder-1.5B and Qwen3-0.6B stay "reported (pre-T11)".
Full table with sizes and RAM estimates: OFFLINE.md, "Recommended
small models".

## v0.4.0 - T11 close-out (release procedure delta)

What ships: T11 multi-model ergonomics (serve.sh model selection by
name + candidate listing + RAM fit warn, compact bash prompt file-ops
hint, weak-model eval indirect-tool-selection probe, Ollama + LM Studio
recipes, verified shortlist).

Procedure deltas vs v0.3.0:

- **DECIDED (operator, 2026-07-26): PRIVATE release,** repeating the
  v0.3.0 procedure; the PUBLIC one-liner gate stays deferred to the
  visibility flip.
- **Tag and publish held until operator dogfood sign-off:** stage 1
  stops at gated artifacts (full release.sh + installer matrix over the
  close-out commits, artifacts staged under
  /home/dev/dist/release/v0.4.0), with NO tag, NO push, and NO GitHub
  release. Stage 2 (tag at the dogfooded head, push, private release,
  closing gate, acceptance record) runs as a separate prompt after
  sign-off. Same as v0.3.0.

## v0.4.0 release acceptance - recorded result

2026-07-27: **v0.4.0 published (repo PRIVATE by decision) and
closing-gate verified.** Annotated tag `v0.4.0` ("temur v0.4.0 -
multi-model ergonomics (T11)") at head `ccfa1bd`, main pushed in one
range `f1670ff..ccfa1bd` (the three stage-1 close-out commits plus the
CHANGELOG dating commit) and the tag pushed after dogfood sign-off;
release "temur v0.4.0" created with the four gated binaries +
`SHA256SUMS`:
<https://github.com/thekeoni1/Temur/releases/tag/v0.4.0> (404s for
non-collaborators while private, by design).

Preflight: tree clean at `fe062ad` ahead of origin/main by exactly the
three stage-1 commits; `ANTHROPIC_API_KEY` absent; `gh auth` OK (repo
scope); visibility confirmed PRIVATE before and after publish; no
v0.4* tag existed before this cycle's tag. The stage-1 bump touched
exactly the six pinned sites; Cargo.lock regenerated via `cargo update
-p temur --offline` (temur entry only).

Gate results at the tagged head `ccfa1bd` (no env overrides, run under
a pty; the full gate was re-run at this exact head after the CHANGELOG
dating commit, per the release rule): full `check.sh` ALL CHECKS
PASSED both paths; leak gate "OK: leak grep clean (operator patterns +
generic shapes, files + history)"; skew gate "OK: install.sh + README
match version 0.4.0 and all targets"; `== RELEASE v0.4.0: 4/4
ARTIFACTS GATED ==` with all four `--version` asserts printing
`temur 0.4.0` (i686 + x86_64 native, aarch64 + armv7 via qemu);
SHA256SUMS self-verify 4/4 OK. Installer matrix 6/6 (host + busybox,
pass/corrupt/unlisted).

Dogfood sign-off: the operator dogfooded serve.sh model selection and
the Qwen3-4B-Instruct-2507 shortlist pick and signed off; stage 2 ran
2026-07-27. No findings carried out of the dogfood.

Procedure deltas this cycle (recorded above in "v0.4.0 - T11
close-out"): the tag and release were HELD after the stage-1 gate run
until dogfood sign-off, repeating the v0.3.0 procedure. No in-cycle
deviations: no scrubs (the leak gate passed first try in both stages),
no source or test changes, and only the two permitted stage-2 commits
(CHANGELOG dating, this record).

Closing gate (private variant, fresh temp dir + temp HOME):
authenticated `gh release download v0.4.0` (x86_64 artifact +
SHA256SUMS) → `sha256sum -c --ignore-missing SHA256SUMS` OK → the
downloaded binary run with the temp HOME printed `temur 0.4.0` → the
downloaded binary's sha256 equals the locally-staged artifact's
byte-for-byte (`5d141e3c5e28ccc6…`, x86_64 artifact).

**OPEN ITEM (unchanged, the only one): the PUBLIC one-liner gate.**
When the operator flips visibility, run the README one-liner verbatim
into a temp HOME (live raw-URL download of install.sh at the newest
released tag, live artifact + SHA256SUMS from the release, checksum
verified, `--version` matches) and record the result here.

## T12 - CI (as-built)

Two-tier GitHub Actions, first-party actions only (checkout, cache,
upload-artifact, all @v4). Both workflows set
`CARGO_TARGET_DIR: ${{ github.workspace }}/target`, which overrides
the machine-specific `target-dir` in `.cargo/config.toml` (env beats
config in cargo's precedence); no config change was needed.

Tier 1, `.github/workflows/ci.yml`, runs on every push to main plus
manual dispatch:

- Job `test`: cargo build + the full hermetic suite (cargo test; no
  network, no tty, i686 binaries execute on the x86_64 runner via
  gcc-multilib/libc6-i386) + the forbidden-dep scan mirrored from
  check.sh (openssl-sys and aws-lc-sys must not resolve).
- Job `release-gate`: the real `scripts/release.sh` with SKIP_CHECK=1
  (gate 1 alone is skipped; its container half is tier 2), staging to
  the runner temp dir and uploading the staged `v*` dir as a 7-day
  artifact. This exercises the generic key-shape leak scan over
  tracked files AND full commit history, the install.sh/README skew
  gate, the 4-target musl-static build with per-target
  readelf/VFP/--version asserts (i686 and x86_64 natively, ARM via
  qemu-user-static), and the SHA256SUMS self-verify. The job MUST
  check out with `fetch-depth: 0`: the history scan greps commit
  messages across all history, and a shallow clone would silently make
  it vacuous. release.sh reads binaries from the hardcoded
  `/home/dev/rustcode-target`, so the job bridges that path with a
  symlink to the workspace target dir (scripts stay CI-agnostic).

Tier 2, `.github/workflows/container-gate.yml`, dispatch-only
(verified green in a live run during T12): the full
`scripts/check.sh` (both paths) with
rootless podman on the runner, after pulling the two pinned images
(check.sh never pulls). check.sh is wrapped in
`script -qec '...' /dev/null` for pty insurance; `-e` propagates the
exit code so a red check fails the step. check.sh gained two CI-only
env knobs for this (defaults are behavior-identical):
`TEMUR_TARGET_DIR` (the hardcoded target dir) and `TEMUR_CHECK_TMP`
(the TUI smoke log dir).

Operator-only, deliberately NOT in CI: the real leak-patterns file
(machine configuration, never committed, never stored as a repo or
Actions secret) and the live release procedure (tag, publish, closing
gate). CI's release-gate runs with a placeholder patterns file written
at job time containing one active pattern that matches nothing in the
repo, so the operator-pattern half of the gate stays exercised end to
end; the embedded generic key-shape scan is the real CI leak coverage.
The placeholder pattern is spelled in the workflow with a bracketed
character class so the tracked workflow file cannot match its own
pattern text.

Rerun policy for timing-sensitive tests: `tests/tools.rs:212` and
`tests/tools.rs:325-337` assert real timing behavior and may flake on
loaded shared CI runners. If one fails in CI: rerun the job; if it
passes on rerun, it was scheduling noise, not a defect. The tests are
never to be loosened for CI (they gate real product behavior and pass
deterministically on dedicated hardware); a persistent CI-only failure
after reruns means the runner class is unsuitable for that assertion
and should be raised with the operator, not papered over.

## T14 acceptance - recorded result (no release)

2026-07-27: **live keyless onboarding + one-shot smoke PASSED, first
attempt, all four steps.** Server: llama.cpp (serve.sh pinned image)
serving Qwen3-4B-Instruct-2507 Q4_K_M, ctx 8192, loopback :8080. All
product runs in a fully isolated XDG dir (fresh config/state/home), the
gnu-debug binary, keyless openai-compat; no key material existed
anywhere in the flow. File-state claims below are host-verified (cat
from the shell), never taken from model prose. The `write — —` in the
stats lines is the plain REPL's verbatim absent-usage rendering.

Step 1, `temur init` with piped answers (`1\n\n`), exit 0:

```
temur init: guided starter config
Config will be written to: <XDG>/config/temur/config.json

Templates:
  1) local      llama.cpp / Ollama / LM Studio (openai-compat, keyless)
  2) anthropic  Anthropic API (key file)
  3) openai     OpenAI API (openai-compat, key file)
  4) gemini     Gemini API (openai-compat, key file)
Template [1]: Model id [qwen3-1.7b]: 
Wrote <XDG>/config/temur/config.json

Next: start your local server (see docs/OFFLINE.md), run temur doctor
to check the setup, then temur to start.
```

The written config matched the README local recipe byte for byte.

Step 2, `temur doctor` with the server up (real probe, no --no-network),
exit 0:

```
PASS: config parsed: <XDG>/config/temur/config.json
PASS: active selection: provider "openai-compat", model "qwen3-1.7b", http://127.0.0.1:8080/v1
PASS: credentials: keyless (no api_key_file configured)
PASS: sessions dir <XDG>/state/temur/sessions: absent, will be created on first save
PASS: reachable: http://127.0.0.1:8080/v1 (TCP connect)
doctor: 5 pass, 0 warn, 0 fail
```

Step 3, live one-shot with a real tool call, exit 0. Prompt: `Create a
file named oneshot.txt containing the single line "t14", then read it
back and tell me what it contains.` stdout (pure prose):

```
The file `oneshot.txt` contains the single line: "t14".
```

stderr (all chrome):

```
  → write
  ✓ write: <WORK>/oneshot.txt
  → read
  ✓ read: <WORK>/oneshot.txt
  (turn: 20705 in / 176 out, cache read 13788 write — — session: 20705 in / 176 out, cache read 13788 write —)
```

Host-verified: `oneshot.txt` contained exactly `t14`.

Step 4, chained `--continue -p` turn, exit 0. Prompt: `Append a second
line saying "chained" to that same file using your tools, then confirm
what it contains now.` stdout:

```
The file `oneshot.txt` now contains the two lines:

1. "t14"
2. "chained"
```

stderr (note the resumed backscroll and advisory notices land here,
keeping stdout pure; the context prewarn is the expected 8192-ctx
advisory, not a failure):

```
> Create a file named oneshot.txt containing the single line "t14", then read it back and tell me what it contains.
  ⚙ write
  ⚙ read
The file `oneshot.txt` contains the single line: "t14".
  [!] resumed session: 6 messages, ~20705 tokens in / 176 out
  → edit
  [!] context: ~7221 of 8192 tokens used; the next response may not fit (max_tokens 1024) — consider starting a new session
  ✓ edit: <WORK>/oneshot.txt
  → read
  ✓ read: <WORK>/oneshot.txt
  (turn: 21912 in / 199 out, cache read 21687 write — — session: 42617 in / 375 out, cache read 35475 write —)
```

Host-verified: `oneshot.txt` ended as the two lines `t14`, `chained`.
The model used `edit` rather than `bash` to append; that is a valid
tool path and the host-verified file state is the acceptance criterion.
Server stopped with `serve.sh stop` after the smoke. (`<XDG>`/`<WORK>`
abbreviate the throwaway smoke directory; every other byte is
verbatim.)

Addendum (P6, 2026-07-27): interrupted one-shot exit code. A `-p` turn
interrupted by Ctrl+C exits 130 (128+SIGINT, the same convention as the
T6 plain-REPL second-press force-quit); a completed turn stays 0 and
provider/startup errors stay 1. Interruption wins over a raced provider
error, matching the T6 rule that an error arriving with the cancel
token set is an interruption, not a failure. The mapping is the pure
`ui::oneshot::exit_code`, unit-tested on all arms; the SIGINT e2e in
tests/cli.rs is event-driven (it blocks on stderr for the tool-start
line, then signals; no sleeps, nothing scheduling-sensitive).

## v0.5.0 - close-out (release procedure delta)

What ships: T12 CI (two-tier GitHub Actions: hermetic test job +
release-gate on every push to main, dispatch-only container gate) and
T14 onboarding + one-shot mode (first-run quickstart, `-p` one-shot
with the 0/1/130 exit contract, `temur init`, `temur doctor`,
tests/cli.rs black-box suite), plus the stage-1 usage docs
(docs/USAGE.md, SETUP.md audience note, README links).

Procedure deltas vs v0.4.0:

- **Two milestones ship together** (T12 + T14 in one minor bump);
  v0.3.0/v0.4.0 each shipped one. No procedure change follows from
  this: one CHANGELOG section covers both.
- **T13 is explicitly NOT in this release:** hosted provider
  verification stays PARKED until API keys exist. The hosted
  Anthropic/OpenAI/Gemini `temur init` templates ship spec-written and
  live-unverified, and the README says so where they are offered.
- **Stage 1 stops at gated LOCAL artifacts** (full release.sh with no
  SKIP_CHECK + installer matrix, staged under
  /home/dev/dist/release/v0.5.0), with NO tag, NO push, and NO GitHub
  release. Stage 2 (tag, push, release, closing gate, acceptance
  record) runs as a separate prompt after operator sign-off. Same as
  v0.4.0.
- **The visibility decision (PRIVATE vs the public flip) is deferred
  to stage 2.** The PUBLIC one-liner gate and the
  hostname-blob-history decision stay queued behind it.

## v0.5.0 release acceptance - recorded result

2026-07-28: **v0.5.0 published (repo PRIVATE by decision, per the
standing stage-2 rule) and closing-gate verified.** Annotated tag
`v0.5.0` ("temur v0.5.0 - onboarding, one-shot mode, CI (T12+T14)") at
head `6a0471f`, main pushed in one range `fb22bfa..6a0471f` (the four
stage-1 close-out commits plus the CHANGELOG dating commit) and the tag
pushed after operator review of stage 1; release "temur v0.5.0" created
with the four gated binaries + `SHA256SUMS`:
<https://github.com/thekeoni1/Temur/releases/tag/v0.5.0> (404s for
non-collaborators while private, by design).

Preflight: tree clean at `9f3a086` ahead of origin/main by exactly the
four stage-1 commits; `ANTHROPIC_API_KEY` absent; `gh auth` OK (repo
scope); visibility confirmed PRIVATE before publish; no v0.5* tag
existed before this cycle's tag. Stage 1 had bumped exactly the six
pinned sites; Cargo.lock regenerated via `cargo update -p temur
--offline` (temur entry only).

Gate results at the tagged head `6a0471f` (no env overrides, run under
a pty; the full gate was re-run at this exact head after the CHANGELOG
dating commit, per the release rule): full `check.sh` ALL CHECKS PASSED
both paths; leak gate "OK: leak grep clean (operator patterns + generic
shapes, files + history)"; skew gate "OK: install.sh + README match
version 0.5.0 and all targets"; `== RELEASE v0.5.0: 4/4 ARTIFACTS GATED
==` with all four `--version` asserts printing `temur 0.5.0` (i686 +
x86_64 native, aarch64 + armv7 via qemu); SHA256SUMS self-verify 4/4
OK. Installer matrix 6/6 (host + busybox, pass/corrupt/unlisted). The
push of the dating commit fired exactly one ci run (30377806797), green
(test + release-gate, which re-validated the 0.5.0 skew and the
4-target build on the runner).

Procedure notes this cycle: stage 1 found the baseline tree NOT clean
(the T15 planned ROADMAP row from the 2026-07-28 planning session was
written but never committed); it rode the cycle in a git stash and was
committed immediately after publish, in the same push as this record.
No scrubs (the leak gate passed first try in both stages), no source or
test changes; the stage-2 commits are the CHANGELOG dating, the T15
row, and this record.

Closing gate (private variant, fresh temp dir + temp HOME):
authenticated `gh release download v0.5.0` (x86_64 artifact +
SHA256SUMS) → `sha256sum -c --ignore-missing SHA256SUMS` OK → the
downloaded binary run with the temp HOME printed `temur 0.5.0` → the
downloaded binary's sha256 equals the locally-staged artifact's
byte-for-byte (`7da4973999d512cd…`, x86_64 artifact).

**OPEN ITEM (unchanged, the only one): the PUBLIC one-liner gate.**
When the operator flips visibility, run the README one-liner verbatim
into a temp HOME (live raw-URL download of install.sh at the newest
released tag, live artifact + SHA256SUMS from the release, checksum
verified, `--version` matches) and record the result here, together
with the standing hostname-blob-history decision.

## T15 acceptance - recorded result (no release)

2026-07-28, five commits on main over the v0.5.0 baseline (2a6a5a3):
P1 1ff31b3 (init picker over the keyless listing GET), P2 ab314f1
(/model --save persistence), P3 eb17311 (doctor model check), P4
ca5c6bd (baked shortlist), P5 (docs + this record). Every phase ran
the full check.sh gate (pty, foreground) green; the P2 run also
proved the forbidden-deps gate clean with indexmap in the tree.

Security posture as amended for T15: the ONE new network capability
is `list_models_keyless(base_url, 3s timeout)`, an unauthenticated
GET of `{base}/models` that takes only a base URL, so it cannot
attach an auth header or touch a key file by construction; init and
doctor call only it, never `list_models_live`. The cli e2e picker
test asserts the captured request head carries no `authorization`
and no `x-api-key` header, end to end through the real binary.

Live smoke (Qwen3-1.7B-Q4_K_M via serve.sh, keyless, isolated XDG
dirs; all first-attempt green, transcripts verbatim):

(a) init with the server UP: picker listed the served id and a
number selected it:

    Template [1]: Base URL [http://127.0.0.1:8080/v1]: Models on http://127.0.0.1:8080/v1:
      1) /model.gguf
    Model (number or id) [/model.gguf]:
    Wrote /tmp/t15-demo/config/temur/config.json

(b) init with the server DOWN: fallback note + baked shortlist, then
the free-text question:

    Base URL [http://127.0.0.1:8080/v1]: could not list models from http://127.0.0.1:8080/v1: model listing GET http://127.0.0.1:8080/v1/models: io: Connection refused
    Known-good small models:
      Qwen3-1.7B Q4_K_M (~2.1 GB RAM at 8k context; the primary recommendation)
      Qwen3-4B-Instruct-2507 Q4_K_M (~3.4 GB RAM)
    Larger is better when RAM allows; 7B+ is qualitatively different.
    See docs/OFFLINE.md, section "Recommended small models".
    Model id [qwen3-1.7b]:

(c) doctor: all-PASS with the picked model ("PASS: model
\"/model.gguf\" is in the server listing at
http://127.0.0.1:8080/v1", "doctor: 6 pass, 0 warn, 0 fail", exit
0); with a bogus configured model, WARN naming it and the server
ids, exit unchanged:

    WARN: model "qwen3-bogus" is not in the server listing at http://127.0.0.1:8080/v1 (server lists: /model.gguf; advisory only, servers may alias ids)
    doctor: 5 pass, 1 warn, 0 fail

(d) live switch + save, then restart:

    >   [!] switched model to qwen3-1.7b (openai-compat · profile settings kept)
      [!] saved model qwen3-1.7b to /tmp/t15-demo/config/temur/config.json
    > bye

    temur 0.5.0 (model=qwen3-1.7b, thinking=false)
    >   [!] provider: openai-compat · model: qwen3-1.7b

(e) a hand-ordered pretty config with unknown fields ("future_knob"
top-level, "operator_note" inside openai_compat, model deliberately
not first) survived a save byte-comparably: diff before/after showed
exactly one changed line, the model value.

Bonus: one live one-shot turn against the picked server
(`temur -p "Reply with exactly: T15 SMOKE OK"`) printed
`T15 SMOKE OK`, exit 0.

Residuals, honest: the baked init shortlist is a hand-kept summary
of OFFLINE.md "Recommended small models" (drift risk; the source
comment names OFFLINE.md as canonical). The plan's base-anthropic
save site read "anthropic.model"; as built it is the top-level
"model" key, because that is the key resolve_base actually reads
(the schema has no nested anthropic object; a nested write would
have been dead weight the loader ignores). serde_json preserve_order
changed Value serialization order globally; request bodies are
pinned byte-identical through sorted-key serialization (goldens
enforce), while session files and other non-gated JSON may order
keys differently than before (cosmetic only).

## v0.6.0 - close-out (release procedure delta)

What ships: T15 model-selection onboarding polish (init model picker,
`/model --save`, doctor model check, baked shortlist) and T16
model-access footgun fixes (init anthropic profile set, /model hints +
advisory, cross-provider hop, riders), one CHANGELOG section covering
both - the two-milestones-together pattern from v0.5.0.

Procedure deltas vs v0.5.0:

- **T16 was pushed BEFORE stage 1** (34b5f27..718f43b, its on-push ci
  run green), where prior cycles started stage 1 from an already-synced
  main. No procedure change follows; the stage-1 baseline is simply the
  post-push head.
- **Stage 1 stops EARLIER than v0.5.0's: version bump + dated
  CHANGELOG + full check.sh gate only.** The four-target release.sh
  build, SHA256SUMS, and installer matrix move INTO stage 2 (tag,
  build, private release, closing gate) by operator instruction; prior
  stage 1s staged gated local artifacts. Rationale: stage 2 waits on
  planning-session verification of the stage-1 report plus an operator
  check of the haiku model alias baked into the init anthropic
  template, so building artifacts before that check could waste a
  cycle.
- **Still no tag, no push of the release-prep commits, no release** at
  stage 1; the visibility decision (repo stays PRIVATE) and the
  public-flip gate remain queued behind stage 2, unchanged.

## T16 acceptance - recorded result (no release)

2026-07-28, five commits on main over the T15 head (34b5f27):
P1 bb8295c (init anthropic profile set), P2 0fd83bd (/model hints +
cached-listing advisory + cache-clear-on-provider-change), P3 8268844
(cross-provider hop + --save site naming), P4 aac3852 (riders: local
4096, truncation source, sessions line), P5 (docs + this record).
Every phase ran the full check.sh gate (pty, foreground) green. No
new network calls anywhere in T16: the hop's only I/O is the key
file read inside the existing provider build path, and every T16
decision (rules 0-3) is computed from config and the cached /models
listing already in hand.

Live smoke (Qwen3-1.7B-Q4_K_M via serve.sh, keyless, isolated XDG
dirs, placeholder key files created by the smoke, NO live Anthropic
call - hop sessions ended before any turn; transcripts verbatim):

(a) init anthropic template, piped answers "2\n\n\n": the startup
profile question listed the four profiles and defaulted to sonnet;
the written config parses and resolves (doctor: "PASS: config
parsed", "active selection: profile \"sonnet\", provider
\"anthropic\", model \"claude-sonnet-5\""); the only finding was the
by-design empty-key-file FAIL ("paste your key in with your
editor").

(b) exact hop, then /status, no turn run:

    temur 0.5.0 (model=/model.gguf, thinking=false)
    >   [!] "claude-opus-5" is an anthropic model - switched to profile "opus" (anthropic, claude-opus-5)
    >   [!] profile: opus
      [!] provider: anthropic · model: claude-opus-5

(c) advisory then inexact hop with --save, one session:

    >   1 model id(s) from the provider:
        /model.gguf
    >   [!] switched model to bogus-id (openai-compat · profile settings kept)
      [!] note: "bogus-id" is not in the last /models listing; the switch stands — a wrong id surfaces as the provider's error on the next turn
    >   [!] "claude-opus-4-8" looks anthropic - hopped to profile "fable" (its key file and limits apply), model claude-opus-4-8
      [!] saved model claude-opus-4-8 to profile "fable" in /tmp/t16-demo/config/temur/config.json

    Config diff: semantically exactly one key changed
    (profiles.fable.model: claude-fable-5 -> claude-opus-4-8;
    asserted by JSON comparison). Restart proof: the next start's
    /model listing shows "fable — anthropic · claude-opus-4-8" with
    "local — openai-compat · /model.gguf (active)" and the two new
    hint lines after the profiles.

(d) init local picker regression: server UP listed "/model.gguf" and
a number selected it (config written with max_tokens 4096); dead
port fell back with the note + baked shortlist and the free-text
default survived.

Residuals, honest: (1) persist_model re-serializes the whole config
pretty (T15 behavior, unchanged in T16) - a hand-formatted init
render is reformatted on the first --save, so the plan's "one-line
config diff" holds semantically (one key) but not byte-wise; the
smoke asserts the semantic form. (2) The /model hint lines append
only to a non-empty profile listing; the empty-profiles branch keeps
its existing three guidance lines (the raw-id hint would duplicate
them). (3) The hop's inexact case builds the provider twice
(activation, then override) - both reads of the same key file, no
network; a mid-hop override failure leaves the activated profile
live and says so in two notices (unit-tested). (4) The README
anthropic recipe shows a representative /home/you key path; the
wizard writes the user's real expanded path, so that recipe is
illustrative, not byte-identical to a render (the local recipe
remains the byte-pinned one).

## v0.6.0 release acceptance - recorded result

2026-07-28 (PDT), staged and published same evening. Baseline: T16
pushed earlier that evening (34b5f27..718f43b, ci run 30419442358
green: test 1m05s, release-gate 4m24s). Stage 1 = three prep commits
(4d7445d bump at all six pin sites, 6842c04 CHANGELOG dated
2026-07-28, ff22b3a RUNBOOK delta + ROADMAP ship vehicle), full
check.sh green, busybox --version printed 0.6.0.

Stage 2, in order, all green:

- Prep commits pushed 718f43b..ff22b3a; on-push ci run 30420865108
  green (test 2m14s, release-gate 8m06s) - this run exercised the
  version-skew release gate against the bump.
- Annotated tag v0.6.0 at ff22b3a ("temur v0.6.0 - model selection
  and access (T15+T16)"), pushed. Unsigned as always; `git tag -v`
  shows the annotation plus "no signature found".
- Full release.sh (no SKIP_CHECK): check.sh both paths, leak grep
  clean (operator patterns + generic shapes, files + history) first
  try, skew gate "OK: install.sh + README match version 0.6.0 and all
  targets", 4/4 targets gated + version-asserted "temur 0.6.0" (i686
  + x86_64 native, aarch64 + armv7 via qemu), SHA256SUMS
  self-verified, staged at /home/dev/dist/release/v0.6.0/. Installer
  matrix 6/6 (pass+corrupt+unlisted, host + busybox).
- Staged sha256s: 6d740675f6d0... aarch64, e400fafb904f... armv7,
  d81cb6142642... i686, 8e3b8ac7025b... x86_64 (full sums in the
  release's SHA256SUMS asset).
- Private release github.com/thekeoni1/Temur/releases/tag/v0.6.0
  created, title "temur v0.6.0", notes = one context line + the
  CHANGELOG v0.6.0 section, 5 assets (4 binaries + SHA256SUMS), not
  draft; 404s while the repo is private, by design.
- Closing gate: authenticated download of SHA256SUMS diffed IDENTICAL
  against the staged file; downloaded x86_64 binary's independent
  sha256 (8e3b8ac7025b8a0c12c945946cf7b7d9b5a02ad581b83f544c1dfbb
  95cdefe72) matches both the staged sum and sha256sum -c; release
  shows 5 assets, isDraft false; git tag -v shows the annotated tag
  at ff22b3a; repo visibility PRIVATE confirmed.

Precondition satisfied before stage 2: the operator confirmed the
claude-haiku-4-5 alias live (the id the init anthropic template
bakes), closing the stage-1 gate item.

Open release items unchanged: the PUBLIC one-liner gate and the
hostname-blob-history decision stay queued behind the visibility
flip; ARM hardware smoke still pending hardware.

## T17 - init hidden key entry (T14 rule amendment record)

What changed versus T14. T14's rule was "init never accepts key
material": the wizard created key files EMPTY and the only sanctioned
way to fill one was a hand edit ("paste your key into <path> with
your editor"). T17 P3 narrows that rule, deliberately and with
operator approval, following the same pattern as T15's keyless-GET
amendment of "no network calls in init": the init wizard, and only
it, may now accept a key at a hidden prompt and write it straight to
the key file.

The exact contract (implemented in src/init.rs, prompt_key_entry):

- Offered ONLY inside `temur init` / `temur init --add`, right after
  a key file is created empty or found existing AND empty. NEVER in
  the REPL, TUI, one-shot, or any other surface. A non-empty existing
  key file is never touched and gets no prompt.
- Prompt: "Paste your API key (input hidden; Enter to skip and add it
  later): ". When stdin is a TTY, echo is disabled via termios (libc
  tcgetattr/tcsetattr) under an RAII guard that restores the terminal
  on ALL exits, error paths included. SIGINT is ignored for the span
  of the read (init installs no signal handler, so a Ctrl+C would
  otherwise kill the process and leave the operator's terminal not
  echoing); termios and the SIGINT disposition are restored together.
  The newline the disabled echo swallowed is printed by hand.
- Non-empty answer: trimmed, written to the key file with a trailing
  newline (secret::load_api_key_from trims), mode forced to 0600, the
  in-memory buffer overwritten best-effort (volatile zeroing) after
  the write; the confirmation is "key saved (hidden) to <path>". The
  key appears in no output, notice, or log. Empty answer or EOF:
  skip, and the T14 editor instruction prints exactly as before.
- The key is never accepted via argv or env; no --key flag exists.
- Non-TTY stdin (piped) reads a plain line so tests and scripts can
  drive the wizard; the test suites use obvious placeholder strings
  only ("placeholder-not-a-real-key"), never real key material.

Honest limit: Rust cannot guarantee zero in-memory copies of the
key. read_line buffers through BufRead internals and the file-write
path may copy; the volatile wipe zeroes the one buffer the wizard
owns. The by-path rule for every OTHER surface is unchanged: outside
this prompt, temur still never accepts, reads back, echoes, or
stores key material.

## T17 acceptance - recorded result (no release)

2026-07-29, five commits on main over the v0.6.0 head (3ef7b49):
P1 045bf6c (init --add merges as profiles, fail-closed collisions,
hop hint renamed), P2 0160971 (xai template + README recipe), P3
3e3e367 (hidden key entry, the T14 amendment; record above), P4
6312089 (doctor rotation WARN, key_rotate_warn_days), P5 (docs +
this record). Every phase ran the full check.sh gate (pty,
foreground) green. Version stays 0.6.0; T17 rides CHANGELOG
Unreleased. The amendment record ("T17 - init hidden key entry")
was committed with P3 and is part of this acceptance.

Live smoke (Qwen3-1.7B-Q4_K_M via serve.sh, keyless, isolated XDG
dirs under the session scratchpad, placeholder keys created by the
smoke only, NO live provider call - the hop session ended before any
turn):

(a) fresh init local (picker listed the served /model.gguf; number
selected it), then init --add anthropic piped "\n\n": the config
diff shows ONLY a "profiles" key appended with the four T16 profiles
in name order sharing the one key file; the base openai_compat
selection and max_tokens survive; NO startup "profile" key invented
(asserted by JSON load); key file created empty, mode 600, in a 700
.secrets dir. Known cosmetic residual: the fresh render's
hand-formatted openai_compat one-liner is re-emitted as pretty JSON
(the same whole-file pretty rewrite persist_model has done since
T15; semantically identical, asserted by parse).

(b) key entry under a REAL pty (script(1), answers fed with 1s gaps
because TCSAFLUSH drains type-ahead): temur init --add openai, the
placeholder "placeholder-not-a-real-key" pasted at the hidden
prompt. Key file content equals the placeholder + newline, mode 600,
and grep -c for the placeholder over the captured pty transcript is
0: nothing echoed. Transcript shows the prompt line, then "key saved
(hidden) to <path>", then the /model closing notice; no editor
instruction on the saved path.

(c) doctor: with the openai key file aged via touch -d "120 days
ago", doctor --no-network prints the new line 'WARN: profile
"openai" key file <path> unchanged for 120 days; consider rotating
the key at the provider and pasting the new one (temur init --add
re-prompts)' directly after that file's unchanged PASS line; all
other PASS/WARN lines byte-identical in shape to v0.6.0; exit still
healthy (0 fail). init --add xai merged one grok-4/api.x.ai profile
(verified by JSON load) and doctor listed its empty key file WARN
like any keyed profile.

(d) regression: plain init for all four original templates renders
exactly the v0.6.0 shapes (also pinned byte-exact in tests/cli.rs);
T16 hop still green: with a placeholder anthropic key in place,
/model claude-opus-5 on the local selection hopped to profile "opus"
("switched to profile \"opus\" (anthropic, claude-opus-5)"), session
ended before any turn; without an anthropic profile the raw switch
stands and the hint now reads "(temur init --add anthropic sets one
up)".

Residuals, honest: (1) the pretty-rewrite cosmetic above. (2) The
--add openai/gemini/xai model question is free text with no listing
(their endpoints need keys; T13 owns hosted live verification, xai
included). (3) EOF at the hidden prompt means skip, not an error,
diverging from ask()'s EOF-is-a-bug rule on purpose so pre-T17
piped answer scripts stay valid. (4) During the hidden read SIGINT
is ignored (not handled): Ctrl+C does nothing until Enter; accepted
as the simplest way to guarantee the terminal is never left
non-echoing. (5) The in-memory wipe is best-effort only, per the
amendment record.

## T18 acceptance - recorded result (no release)

2026-07-29, five commits on main over the T17 head (95ef0cc):
P1 5fc74a9 (KeyGuard identity guard wired into read/write/edit),
P2 1129636 (grep/glob walks guarded, one snapshot per execution),
P3 bd84579 (bash userns + /dev/null bind-mask sandbox, refusal +
allow_bash_without_key_sandbox override), P4 51a0947 (active-key
redaction at the Registry, doctor key-isolation and sandbox lines),
P5 (docs + this record). Every phase ran the full check.sh gate
(pty, foreground) green. Version stays 0.6.0; T18 rides CHANGELOG
Unreleased. No step read, copied, printed, or tested with real key
material: every test and smoke key is a placeholder string the run
created itself.

THE INVARIANT, verified at every layer: a keyless config behaves
byte-identically to pre-T18 builds. ToolCtx::new yields an empty
guard (every pre-existing construction unchanged); an empty guard
checks nothing; bash's keyless arm neither probes nor unshares
(asserted with a panicking probe in the decision-table test); no
redaction key is registered. With any key file configured: layer 1
denies tools the file, its directory siblings, and every alias of
its identity; layer 2 masks it from bash or refuses bash; layer 3
scrubs the active key from anything a tool returns.

Layer-1 denial message (verbatim shape):
  access to <path> is blocked: configured key files are not
  readable by tools (key isolation)
Bash refusal message (verbatim, one constant shared with tests):
  bash is disabled: key files are configured, and this kernel does
  not allow the unprivileged user namespace sandbox that isolates
  them from shell commands. The other tools stay guarded. To accept
  running bash WITHOUT the key sandbox, set
  "allow_bash_without_key_sandbox": true in config.json.

Sandbox sequence verified against local user_namespaces(7) and
mount_namespaces(7) before coding: unshare(NEWUSER|NEWNS); "deny"
to /proc/self/setgroups BEFORE gid_map (mandatory without
CAP_SETGID in the parent ns); single-line "uid uid 1" self-maps
(the one mapping an unprivileged process may write); MS_REC |
MS_PRIVATE on / (modern systems default to shared propagation);
then a /dev/null MS_BIND per existing key file. The pre_exec
closure is raw syscalls over pre-computed bytes: no allocation
between fork and exec. Availability is probed by actually running
the sequence around /bin/true and cached per process; doctor uses
the same helper.

Environment fact recorded during P3: this host's rootless
podman + crun PERMITS nested unshare(CLONE_NEWUSER), so the
container suites exercised the SANDBOXED arm in-container (the plan
had predicted the refusal arm there); the refusal decision is
covered deterministically by the injected-probe unit tests, and the
injected-probe doctor tests cover the WARN arms.

Live smoke (Qwen3-4B-Instruct-2507-Q4_K_M via serve.sh, keyless
local llama.cpp, isolated XDG dirs under the session scratchpad,
musl release binary; the keyed config guards a placeholder key file
the smoke created; active profile keyless "local", so no live call
ever needed a key):

(a) read of the key file: the model called read and quoted the
layer-1 denial verbatim: "access to <scratch>/work/secrets/api.key
is blocked: configured key files are not readable by tools (key
isolation)". (First attempt note: the model initially refused on
its own without calling the tool; the transcript kept is the
forced-call retry, which is the one that exercises temur.)

(b) sandboxed bash: "cat secrets/api.key; echo EXIT=$?" ran with
exit 0 and cat produced NOTHING (the /dev/null mask); the
placeholder string appears nowhere in the transcript; the host key
file was untouched.

(c) grep pattern "placeholder-not-a-real" over the tree: "No
matches found"; pattern "ordinary": one hit in notes.txt. The key
file is never read and never named.

(d) doctor (keyed config): "PASS: key isolation: 1 key file(s)
guarded (tools cannot read them)" and "PASS: bash key sandbox:
available (unprivileged user namespaces)", exit healthy. Keyless
config: "PASS: key isolation: keyless config, no key files to
guard" + "NOTE: bash key sandbox: not needed (keyless config)".

(e) keyless invariant: the same cat through the bash tool under the
keyless config printed the placeholder content exactly as any
pre-T18 build would (and read reads it freely). Byte-identical
old behavior confirmed live.

Residuals, honest: (1) The identity check knows a key only while
the configured path exists: a hardlink made BEFORE temur ran
escapes it if the key file itself is later removed or renamed away.
(2) Redaction covers the ACTIVE key only; inactive profiles' keys
are never read, so there is honestly nothing to redact them with.
(3) A /dev/null-masked write inside the bash sandbox is discarded
silently; the model is not told its write went nowhere. (4) The
parent-directory rule blocks the WHOLE directory holding a key
file: a key configured in a broad directory (home, project root)
blocks tool access to all of it; documented in README with the
"own directory" recommendation init already follows. (5) The
sandbox masks only key files that EXIST at spawn time; a missing
configured key has nothing to mask (layer 1 still guards its
path). (6) bash inherits process_group(0) kill semantics
unchanged; the sandbox adds no new kill path. (7) In one-shot
smokes the local model sometimes self-refuses key-adjacent
requests before any tool runs; irrelevant to the guards but noted
because it cost smoke retries.

## v0.7.0 - close-out (release procedure delta)

What ships: T17 provider onboarding (init --add, xai template, hidden
key entry, rotation reminder) and T18 key isolation guards (file
guard, bash sandbox, redaction, doctor lines), one CHANGELOG section
covering both - the two-milestones-together pattern from v0.5.0 and
v0.6.0.

Procedure deltas vs v0.6.0:

- **Both milestones were pushed BEFORE stage 1** (T17 at 95ef0cc, T18
  at a32ea30, each with its on-push ci run green), extending the
  v0.6.0 delta where only one of the pair was. The stage-1 baseline is
  the post-push head a32ea30; step A of the stage-1 prompt (push T18)
  had already been executed under a separate push authorization in the
  same implementing session, so stage 1 verified its end state (main
  == origin == a32ea30, ci 30492294811 green) instead of re-pushing.
- **Stage 1 keeps the v0.6.0 EARLY stop**: version bump + dated
  CHANGELOG + records + full check.sh gate only; the four-target
  release.sh build, SHA256SUMS, and installer matrix stay in stage 2
  (tag, build, private release, closing gate).
- **Still no tag, no push of the release-prep commits, no release** at
  stage 1; the visibility decision (repo stays PRIVATE) and the
  public-flip gate remain queued behind stage 2, unchanged.
- Pin-site note for the bump: the lone remaining 0.6.0 in Cargo.lock
  is the upstream ureq-proto crate's own version (the v0.6.0 cycle's
  equivalent was heck 0.5.0); not a temur pin.

## v0.7.0 release acceptance - recorded result

2026-07-29 (PDT), staged and published same afternoon. Baseline: T18
pushed earlier that day (95ef0cc..a32ea30, ci run 30492294811 green:
test 1m38s, release-gate 4m26s). Stage 1 = two prep commits (23bab4f
bump at all six pin sites, 1e2b1aa CHANGELOG dated 2026-07-29 +
RUNBOOK close-out delta + ROADMAP ship-vehicle rows), full check.sh
green, busybox --version printed 0.7.0.

Stage 2, in order, all green:

- Prep commits pushed a32ea30..1e2b1aa; on-push ci run 30494399614
  green (test 2m39s, release-gate 7m37s) - this run exercised the
  version-skew release gate against the bump.
- Annotated tag v0.7.0 at 1e2b1aa ("temur v0.7.0 - provider
  onboarding and key isolation (T17+T18)"), pushed. Unsigned as
  always.
- Full release.sh (no SKIP_CHECK): check.sh both paths, leak grep
  clean (operator patterns + generic shapes, files + history) first
  try, skew gate "OK: install.sh + README match version 0.7.0 and
  all targets", 4/4 targets gated + version-asserted "temur 0.7.0"
  (i686 + x86_64 native, aarch64 + armv7 via qemu), SHA256SUMS
  self-verified, staged at /home/dev/dist/release/v0.7.0/. Installer
  matrix 6/6 (pass+corrupt+unlisted, host + busybox).
- Staged sha256s: 58ec825d0f00... aarch64, a2ee870fb1bc... armv7,
  6951d57cdacd... i686, 08a99880a8ac... x86_64 (full sums in the
  release's SHA256SUMS asset).
- Private release github.com/thekeoni1/Temur/releases/tag/v0.7.0
  created, title "temur v0.7.0 - provider onboarding and key
  isolation (T17+T18)", notes = the CHANGELOG v0.7.0 section, 5
  assets (4 binaries + SHA256SUMS), not draft; 404s while the repo
  is private, by design. Repo visibility PRIVATE confirmed via gh
  BEFORE creating the release.
- Closing gate: authenticated download of the x86_64 binary +
  SHA256SUMS into a scratch dir; sha256sum -c OK; the downloaded
  binary's independent sha256 (08a99880a8ac1fd2d823878a8097c7bbc6
  4fcd0bd1f94526ceea435672711732) equals the staged one byte for
  byte.

Procedure note: the gh release create run must start inside the repo
worktree (it resolves the repo from git); the first attempt from the
staging dir failed with "not a git repository" and was rerun from
the repo with absolute asset paths. No other deltas vs v0.6.0.

Open release items unchanged: the PUBLIC one-liner gate and the
hostname-blob-history decision stay queued behind the visibility
flip; ARM hardware smoke still pending hardware.

## T19 acceptance - recorded result (no release)

2026-07-29, five commits on main over the v0.7.0 head (8988433):
P1 a36d8d1 (context-scaled head+tail truncation), P2 4ac0fe7
(write read-first enforcement + binary nudge), P3 4a49311
(prose tool-call execution; the T4 amendment record "T19 - prose
tool-call execution" sits next to the T4 acceptance section above
and is part of this acceptance), P4 11c9995 (eval tasks 8 and 9),
P5 (docs + this record). Every phase ran the full check.sh gate
(pty, foreground) green before its commit. Version stays 0.7.0;
T19 rides CHANGELOG Unreleased.

Live keyless smoke, all against llama.cpp server-b10068 serving
Qwen3-4B-Instruct-2507 Q4_K_M, ctx 8192, --jinja, compact profile,
max_tokens 2048, musl-static binary (readelf-verified in the eval
preflight), --network none pods, isolated XDG dirs. NO hosted or
keyed call anywhere; placeholderless (no key material exists in the
smoke at all).

(a) Extended eval, FIRST ATTEMPT, no wording iterations:

| task | name | result | seconds |
|---|---|---|---|
| 1 | write-file | PASS | 49 |
| 2 | read-extract | PASS | 15 |
| 3 | edit-config | PASS | 17 |
| 4 | bash-mkdir | PASS | 11 |
| 5 | find-needle | PASS | 22 |
| 6 | bump-and-copy | PASS | 18 |
| 7 | indirect-delete | PASS | 7 |
| 8 | binary-nudge | PASS | 25 |
| 9 | large-tail | PASS | 89 |

SCORE: 9/9. Task 8's transcript shows the intended path exactly
(write notes.txt as text, then bash "gzip /work/notes.txt"); the
host gunzip assertion is what scored it. Task 9's transcript shows
bash "cat data.log" then a write of tail.txt; the needle sits on
the LAST line of ~30k chars of output against an 8192-char cap, so
the pass is live proof of the tail keep.

(b) Truncation marker in situ (interactive REPL, bash cat of a
~30k-char big.log; host-verified from the saved session file since
the plain REPL prints tool chrome, not tool results):

  (output truncated: showing the first 4096 and last 4096 of 29971
  chars; narrow the command, e.g. grep or head/tail, to see the
  elided middle)

and the last-line needle KAPPA-2718 present in the kept tail; the
model's reply quoted it live.

(c) Read-first denial live: prompted to call ONLY write on an
existing locked.txt. Transcript: write ✗, then the model read the
file and rewrote it ✓ (the denial steered a live model into the
correct read-then-write shape on its own). Saved session carries
the denial verbatim: "locked.txt exists but has not been read in
this session. Read it first, or use edit for targeted changes."
First attempt of this prompt shape; an earlier softer prompt
("without reading it first...") did NOT trigger the denial because
the model chose to read first anyway - recorded as model good
behavior, not a harness gap.

(d) Prose-call recovery through the real binary (mock SSE, an
EndTurn whose only text is a leading-brace JSON write call):
transcript shows the notice "prose-call recovery: executed the
write tool call the model wrote as plain text", the file lands on
disk with the exact content, and the follow-up request completed
the turn. The mock e2e suites cover the same path plus the
failure/ambiguity/off-switch arms.

Deviations from the plan, all recorded: (1) the plan named
compact/write.txt, which does not exist; write serves the one
write.txt to BOTH profiles (asserted by the profile test), so the
single prompt edit covers both. (2) The truncation cap is wired
inside Session::build and Session::switch_provider rather than at
the main.rs call sites: the same two lifecycle moments as the T18
redaction key, but enforced in the session so no construction path
can forget it. (3) A planned "lossy prose still nudges" test
asserted the true behavior instead: detect_text_tool_call never
parsed truncated JSON pre-T19 either, so lossy prose ends the turn
with no nudge, unchanged.

Honest limits: prose executions bypass the doom-loop fingerprint
guards (failing calls are bounded by the nudge cap, succeeding ones
by max_iterations); the read-paths set is in-memory only and starts
empty on --continue/--resume by design; read of a file via bash
(cat) does not arm the write check - only the read/edit/write tools
do.

## T20 acceptance - recorded result (no release)

2026-07-29, four commits on main over the T19 head (4d650d6):
P1 27707cd (/compact, fail-closed summary compaction), P2 3b578af
(unified context advisory with a resume-time trigger), P3 c4a3370
(prefix-stability invariant tests, both providers; no violation
found, so no fix rode along and the request-body goldens are
untouched), P4 (docs + this record). Every phase ran the full
check.sh gate (pty, foreground) green before its commit. Version
stays 0.7.0; T20 rides CHANGELOG Unreleased. Per the grounded
plan, T20 added NO caching: the Anthropic cache_control
breakpoints have existed since the initial commit.

Live keyless smoke against llama.cpp server-b10068 serving
Qwen3-4B-Instruct-2507 Q4_K_M (ctx 8192, --jinja), musl-static
release binary on the host, isolated XDG dirs, config: keyless
openai-compat base_url http://127.0.0.1:8080/v1, model qwen3-4b,
context_window 4096, max_tokens 512, compact profile. NO hosted or
keyed call anywhere.

(a) Advisory fires live on the NEW 80% arm (three verbose answers;
80% of 4096 = 3277, and remaining 601 >= max_tokens 512 proves the
old arm could not have fired here):

  [!] context: ~3495 of 4096 tokens used; /compact frees the window by summarizing the conversation, or start a new session

Exactly one advisory in the session (latch); the later
"context: ~3911 of 4096 tokens used" line is /status output.

(b) Quit, then --continue: the resume-time trigger fires at seed
load, BEFORE any turn, and a live /compact lands (6 messages -> 2):

  [!] resumed session: 6 messages, ~9423 tokens in / 1099 out
  [!] context: ~3911 of 4096 tokens used; /compact frees the window by summarizing the conversation, or start a new session
  >   [!] compacted: 6 message(s) summarized into 2; the next request rebuilds the provider's cached prefix (one-time cost)
  [!] context: no usage reported yet

(the last line is /status right after: the estimate reset.)

(c) Saved session file verified on disk after (b): 2 messages,
last_context_used null, first message role user with TWO text
blocks, block 0 beginning
"[conversation summary (compacted)]\nGoal: Explain how TCP
congestion control, DNS resolution, and TLS 1.3 handshakes work"
and block 1 the verbatim tail prompt ("Now explain in about 300
words how TLS 1.3 handshakes work. ..."); last message role
assistant. Qwen3-4B produced all five structured headings (Goal /
State / Decisions / Files / Next steps) on the first attempt.

(d) --continue again, post-compact: NO advisory at seed load (the
restored estimate is null by design), the replayed backscroll shows the
summary + tail, and a live turn answered from the summary
("The three topics explained earlier were TCP congestion control,
DNS resolution, and TLS 1.3 handshakes."). Honest note: AFTER that
turn completed, the turn-loop advisory fired again at ~3422 of
4096. That is correct behavior, not a bug: the latch re-arms on
compaction, and the verbatim tail (a 300-word answer) plus system,
tools, and a fresh 300-word reply genuinely re-crossed 80% of this
deliberately tiny window. The resume-time assertion the plan asked
for (no advisory at seed load post-compact) holds in the same
transcript.

Deviations and honest limits, all recorded: (1) a cancelled or
empty-summary attempt still adds the summary call's reported usage
to the session totals: the tokens were really spent, and the plan's
"usage kept cumulative" rule is applied to failures too. (2) The
UI transcript/backscroll is NOT rebuilt on /compact: the display
keeps showing what happened; only the wire history (and the saved
file) compact. (3) The advisory estimate remains the T3 one
round-trip-stale advisory value; both arms judge the previous
response's usage. (4) The smoke's served model id is /model.gguf
(llama.cpp single-model listing); the config's model name qwen3-4b
is accepted by the server regardless, as in prior smokes. (5) The
CLI /compact is replay-guarded like /clear and /models, so the
mock-provider /compact coverage lives in the test suites (the
tests/agent.rs T20 section), not in a --mock transcript.

Pre-existing flake surfaced by the P4 gate, NOT a T20 defect and
NOT fixed here (out of scope; left for the planning session): the
tui suite's headless_command_flow tests race their scripted key
pump against the driver thread. App::handle_key drops Enter while
busy is true, and the headless ScriptedEvents source pumps keys
with no delay, so under heavy machine load (the gate's parallel
container work; the llama server was also up during the first
run) the Enter ending one scripted line can land before the
previous turn's TurnComplete/PromptOpen folds busy back to false.
Observed both ways in P4 gate runs: one hang (driver blocked in
read_input forever while the render thread spun on the exhausted
script at 100% CPU; killed after 3h) and one merged-line assert
("/model sonnet-next" + "/clear" submitted as one line,
tests/tui.rs:1076). 120 isolated runs + 40 full-suite runs on an
idle machine reproduce neither; the T20 diff touches no ui/tui
file. The passing gate recorded above is a clean full rerun.

## v0.8.0 - close-out (release procedure delta)

What ships: T19 model floor (context-scaled head+tail truncation,
write read-first enforcement, binary-format nudge, prose tool-call
execution, eval tasks 8+9) and T20 context lifecycle (/compact,
unified two-arm context advisory with the resume-time trigger,
prefix-stability invariant tests), one CHANGELOG section covering
both - the two-milestones-together pattern from v0.5.0 through
v0.7.0.

Procedure deltas vs v0.7.0:

- **One of the pair was pushed before stage 1, the other AS stage-1
  step 1** (the v0.6.0 shape, not the v0.7.0 both-pushed-early
  shape): T19 was already on main at 4d650d6 (ci 30502421688 green);
  T20 (27707cd..623c534) was pushed as the explicit first step of
  the stage-1 prompt under the planning session's authorization,
  on-push ci run 30561457680 green (test 1m39s, release-gate 4m10s),
  main == origin == 623c534 verified before the prep commits.
- **Stage 1 keeps the v0.6.0/v0.7.0 EARLY stop**: version bump +
  dated CHANGELOG + records + full check.sh gate only; the
  four-target release.sh build, SHA256SUMS, and installer matrix
  stay in stage 2 (tag, build, private release, closing gate).
- **Still no tag, no push of the release-prep commits, no release**
  at stage 1; the visibility decision (repo stays PRIVATE) and the
  public-flip gate remain queued behind stage 2, unchanged.
- Pin-site note for the bump: this cycle leaves NO current-version
  hit in Cargo.lock at all (v0.7.0's equivalent was ureq-proto's own
  0.6.x version; no dependency sits at 0.7.0), so the six bumped
  pins are the complete set and the repo-wide grep after the bump
  comes back empty outside historical records.
- The pre-existing headless-TUI key-pump flake recorded in the T20
  acceptance above remains open going into this release; it is test
  infrastructure only and does not gate the ship (the stage-1 gate
  run below was a clean full pass).

## v0.8.0 release acceptance - recorded result

2026-07-30 (PDT), staged and published same day. Baseline: T19 was
already on main (4d650d6, ci 30502421688 green); T20 pushed as
stage-1 step 1 (4d650d6..623c534, ci run 30561457680 green: test
1m39s, release-gate 4m10s). Stage 1 = three prep commits (db79b01
bump at all six pin sites, 678c126 CHANGELOG dated 2026-07-30,
9ed4d37 RUNBOOK close-out delta + ROADMAP ship-vehicle rows), full
check.sh green, busybox --version printed 0.8.0.

Stage 2, in order, all green:

- Prep commits pushed 623c534..9ed4d37; on-push ci run 30563031243
  green (test 2m05s 16:47:13Z..16:49:18Z, release-gate 6m46s
  16:47:18Z..16:54:04Z) - this run exercised the version-skew
  release gate against the bump.
- Annotated tag v0.8.0 at 9ed4d37 ("temur v0.8.0 - model floor and
  context lifecycle (T19+T20)"), pushed (tag object 22029eb).
  Unsigned as always.
- Full release.sh (no SKIP_CHECK): check.sh both paths green, leak
  grep clean (operator patterns + generic shapes, files + history)
  first try, skew gate "OK: install.sh + README match version 0.8.0
  and all targets", 4/4 targets gated + version-asserted
  "temur 0.8.0" (i686 + x86_64 native, aarch64 + armv7 via qemu),
  SHA256SUMS self-verified, staged at /home/dev/dist/release/v0.8.0/.
  Installer matrix 6/6 (pass+corrupt+unlisted, host + busybox).
- Staged sha256s: 23d6a09d4a7a... aarch64, 54e7add33e3a... armv7,
  1c0100b584d6... i686, 8f717e54a3bb... x86_64 (full sums in the
  release's SHA256SUMS asset).
- Private release github.com/thekeoni1/Temur/releases/tag/v0.8.0
  created FROM INSIDE the repo worktree per the v0.7.0 procedure
  note (no not-a-git-repository rerun needed this time), title
  "temur v0.8.0 - model floor and context lifecycle (T19+T20)",
  notes = the CHANGELOG v0.8.0 section, 5 assets (4 binaries +
  SHA256SUMS), not draft; 404s while the repo is private, by
  design. Repo visibility PRIVATE confirmed via gh BEFORE creating
  the release.
- Closing gate: authenticated download of the x86_64 binary +
  SHA256SUMS into a scratch dir; sha256sum -c OK; the downloaded
  binary's independent sha256 (8f717e54a3bb25d89d373d416d3a14220a
  ddda65599c35c82a7bd8257b6bfebe) equals the staged one byte for
  byte.

Honest residuals: the headless-TUI key-pump flake (T20 acceptance
record above) stayed quiet through every stage-2 gate run; still
open as test infrastructure, candidate T21 rider. No other deltas
vs v0.7.0.

Open release items unchanged: the PUBLIC one-liner gate and the
hostname-blob-history decision stay queued behind the visibility
flip; ARM hardware smoke still pending hardware.

## T21 acceptance - recorded result (no release)

2026-07-30, four commits on main over the v0.8.0 head (121ec74):
P1 4dcbe3e (per-command bash approval: Ask arm, approver plumbing,
amended refusal, ScriptedSteps harness, probe seam, three new test
surfaces), P2 b187fae (init key-shaped mis-paste catch + doctor
approval hint), P3 5b9dd68 (headless key-pump flake fix, harness
only), P4 (docs + this record). Every phase ran the full check.sh
gate (pty, foreground) green before its commit; the container suite
list gained the new approval suite. Version stays 0.8.0; T21 rides
CHANGELOG Unreleased. NOT pushed: push waits for the planning
session's verification, per the standing rule.

Decision table as built (decide_sandbox, unit-pinned across all 12
rows incl. panicking-probe keyless rows with and without an
approver): keyless -> Plain (never probes); probe ok -> Sandboxed
(neither override nor approver ever preempts it); override on ->
Plain (silences the ask); approver installed -> Ask; else ->
Refuse. Ask at execute time: an already-set cancel token denies
without prompting; the approver gets the exact command string;
approve = one PLAIN spawn, never cached; deny = the fixed
APPROVAL_DENIED constant ("the user declined to run this command")
as a normal is_error tool_result, turn continues.

Live smoke 2026-07-30, keyless-local only (llama.cpp server-b10068
serving Qwen3-4B-Instruct-2507 Q4_K_M, ctx 8192, --jinja; musl
release binary; isolated XDG dirs under the session scratchpad;
every key file a placeholder string; NO hosted or keyed call
anywhere):

(a) Working sandbox never prompts: keyed profile (placeholder key,
guard = 1 file) on the WSL2 host where userns works; "echo
smoke-a-ran > smoke-a-marker.txt" via the bash tool ran SANDBOXED
with no approval prompt anywhere in the pty transcript, marker
written. Approval never preempts a working sandbox, live.

(b) Ask arm GENUINELY live, no fallback needed: i386/debian
container with --network host and a seccomp profile denying
unshare (SCMP_ACT_ERRNO/EPERM, archs x86_64+x86+x32); in-container
`unshare -U true` fails "Operation not permitted", so the probe
fails for real (the TEMUR_TEST_SANDBOX_UNAVAILABLE seam was NOT
needed for this arm). Plain REPL over the podman pty, keyed
profile, live model. First command approved with y:

  [?] bash approval needed: the key sandbox is unavailable on this host,
      so this command would run with NO key isolation:
        echo live-approved > /smoke/home/live-marker.txt
      run it? [y/N]   ✓ bash: echo live-approved > /smoke/home/live-marker.txt

marker written with "live-approved". Second command denied with n:
✗ bash, deny-marker.txt ABSENT, and the model adapted on the same
turn's next round ("The command ... was not executed, as the user
declined to run it."). Session continued to a clean exit.

(c) init mis-paste catch live (pty, openai template):
"sk-placeholder-0123456789abcdef" answered at the key file PATH
question printed the WARNING block (looks like key material, path
question, hidden-prompt-only, rotate), re-asked, and the re-asked
real path won. grep over the smoke's config/state/home trees finds
the pasted value in NO file; the config carries the good path; the
key file was created empty mode 600. Honest note: the pty's own
input echo shows the typed value in the terminal capture, which is
exactly the exposure the rotate warning is about.

(d) doctor amended arm live, in the same seccomp container
(genuine probe failure, --no-network):

  WARN: bash key sandbox: unavailable on this kernel (no unprivileged user namespaces): an interactive session will ask per-command approval before running bash unsandboxed; non-interactive runs refuse. Setting allow_bash_without_key_sandbox to true in config.json accepts running it unsandboxed without asking (the other tools stay guarded; see README.md, section "Untrusted hosts")

P3 proof: the racy multi-line headless test (the T20-recorded
flake site) now drives the readiness-gated ScriptedSteps source;
40 consecutive full tui-suite runs green on an idle machine, then
40 more with every CPU saturated by shell busy-loops, 80/80.
App::handle_key's busy-Enter drop is untouched. Why both recorded
failure modes are impossible by construction: a Line step's first
key is delivered only once busy is false, delivery and key
handling share the render thread, and nothing can set busy again
before that line's own Enter, so no Enter is ever dropped (no
hang) and no chars ever type into a busy session (no merged line).

Design calls and honest limits, recorded: (1) the probe seam
TEMUR_TEST_SANDBOX_UNAVAILABLE ships in the release binary; it is
one-way (can only force the RESTRICTIVE direction, never fake a
working sandbox) and exists for the e2e suites and locked-host
diagnosis; flagging for planning-session review. (2) The TUI
approval prompt truncates the DISPLAY of a very long command to at
most 8 input-area rows; the approver always receives, and the
plain REPL always prints, the full exact command. (3) While a TUI
approval prompt is open it consumes every key, including Ctrl+C
and Esc-as-interrupt; deny (n or Esc) first, then interrupt. An
interrupt requested BEFORE the ask (cancel token already set)
denies without prompting. (4) In the plain REPL the approver reads
the same stdin as the prompt loop, so a y/N answer typed early
(type-ahead) is consumed as the answer, like any terminal prompt.
(5) The piped non-interactive e2e observes the refusal via the
error-marked tool cell and the absent marker; the refusal WORDING
on the wire is pinned by the bash.rs unit test and the tools.rs
equality assertion instead (the plain REPL never prints tool error
bodies). (6) The smoke's served model id remains /model.gguf vs
config name qwen3-4b, accepted as in all prior smokes.

NEXT: planning-session verification of these four commits
(read-only), then the ship vehicle decision (likely v0.9.0).

## v0.9.0 - close-out (release procedure delta)

What ships: T21 alone (bash approval mode: the Ask arm in
decide_sandbox with interactive per-command approval when keys are
guarded and the sandbox is unavailable, plain-REPL and TUI approvers,
amended SANDBOX_REFUSAL; init key-shaped mis-paste catch; doctor
three-outcome hint; harness-only headless key-pump flake fix; README
"Untrusted hosts" + USAGE approval guide), one CHANGELOG section.
First single-milestone release since v0.5.0; the planning session
proposed v0.9.0 = T21 alone and the operator directed the ship.

Procedure deltas vs v0.8.0:

- **T21 pushed AS stage-1 step 1** under the planning session's
  authorization (the v0.6.0/v0.8.0 shape): 4dcbe3e..4e5690b onto
  121ec74, on-push ci run 30595569722 green (test 1m20s,
  release-gate 4m23s), main == origin == 4e5690b verified before the
  prep commits.
- **Stage 1 keeps the EARLY stop**: version bump + dated CHANGELOG +
  records + full check.sh gate only; the four-target release.sh
  build, SHA256SUMS, and installer matrix stay in stage 2 (tag,
  build, private release, closing gate).
- **Still no tag, no push of the release-prep commits, no release**
  at stage 1; the repo stays PRIVATE and the public-flip gate stays
  queued behind stage 2, unchanged.
- Pin-site note for the bump: the four-file map from the v0.8.0 bump
  (db79b01) was the complete set again (Cargo.toml, Cargo.lock temur
  entry, install.sh VERSION, five README pins); the repo-wide grep
  after the bump comes back empty outside historical records. New
  this cycle: the `untrusted` crate (a ring dependency) itself sits
  at version 0.9.0 in Cargo.lock, so a future grep for the CURRENT
  version must not blind-replace that entry; the temur package entry
  is the only lock pin.
- The container suite list in check.sh gained the T21 approval suite;
  the stage-1 gate run below was a clean full pass at 0.9.0 with the
  bare-container line reading "temur 0.9.0".

## v0.9.0 acceptance - recorded result (SHIPPED, private)

2026-07-30 (local PDT; CI timestamps are Z). v0.9.0 ships T21 alone
(bash approval mode + untrusted-host riders), per the two-stage
procedure. Everything below ran on the private repo only.

- Stage-1 recap: T21 pushed 121ec74..4e5690b under planning-session
  authorization, on-push ci run 30595569722 green (test 1m20s,
  release-gate 4m23s); three LOCAL prep commits 86b31f3 (bump
  0.8.0 -> 0.9.0, four-file map per db79b01) + 6e0b14f (CHANGELOG
  "## v0.9.0 - 2026-07-30") + bab70b9 (RUNBOOK close-out delta +
  ROADMAP ship-vehicle row); full check.sh green at 0.9.0.
- Stage 2 prep push 4e5690b..bab70b9; on-push ci run 30601981066
  green (test 1m58s 03:35:58Z..03:37:56Z, release-gate 6m51s
  03:35:58Z..03:42:49Z) - the version-skew run against the bump.
- Annotated tag v0.9.0 at bab70b9 ("temur v0.9.0 - bash approval
  mode (T21)"), pushed (tag object c65049d). Unsigned as always.
- Full release.sh (no SKIP_CHECK): check.sh both paths green
  (bare-container line "temur 0.9.0"), leak grep clean (operator
  patterns + generic shapes, files + history) first try, skew gate
  "OK: install.sh + README match version 0.9.0 and all targets",
  4/4 targets gated + version-asserted "temur 0.9.0" (i686 + x86_64
  native, aarch64 + armv7 via qemu), SHA256SUMS self-verified,
  staged at /home/dev/dist/release/v0.9.0/. Installer matrix 6/6
  (pass+corrupt+unlisted, host + busybox).
- Staged sha256s: 8394bd0bd442... aarch64, d52bb8bf6de2... armv7,
  344464a4935f... i686, 9c118a62754d... x86_64 (full sums in the
  release's SHA256SUMS asset).
- Private release github.com/thekeoni1/Temur/releases/tag/v0.9.0
  created FROM INSIDE the repo worktree per the v0.7.0 procedure
  note, first try, title "temur v0.9.0 - bash approval mode (T21)",
  notes = the CHANGELOG v0.9.0 section, 5 assets (4 binaries +
  SHA256SUMS), not draft; 404s while the repo is private, by
  design. Repo visibility PRIVATE confirmed via gh BEFORE creating
  the release and re-confirmed after.
- Closing gate: authenticated download of the x86_64 binary +
  SHA256SUMS into a scratch dir; sha256sum -c OK; the downloaded
  binary's independent sha256 (9c118a62754d89bc3d3a860888c5e32872
  7dca7b4bda99e91bec430f53265651) equals the staged one byte for
  byte (cmp clean).

Honest residuals: none new this cycle; the T21 one-way probe seam
(TEMUR_TEST_SANDBOX_UNAVAILABLE, reviewed low-risk) ships in the
release binaries as recorded in the T21 acceptance. The headless-TUI
key-pump flake is FIXED as of T21 P3 (harness only, 80/80 proof) and
stayed quiet through every stage-2 gate run.

Open release items unchanged: the PUBLIC one-liner gate and the
hostname-blob-history decision stay queued behind the visibility
flip; ARM hardware smoke still pending hardware.
