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

## 2. Inject the real credential (root; once; see docs/SETUP.md)

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
(docs/USAGE.md, docs/SETUP.md audience note, README links).

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

## T22 - keyless /props probe (T15 amendment extension record)

What changed versus T15. The T15 keyless-GET amendment allowed init
and doctor exactly ONE network request: list_models_keyless, an
unauthenticated GET of {base}/models taking only a base URL. T22
extends that contract, deliberately and with the same shape, to
exactly TWO requests:

- GET {base}/models (list_models_keyless, unchanged), and
- GET {root}/props (probe_props_context, src/provider/mod.rs), where
  root is the base URL with an SDK-conventional trailing /v1
  stripped: llama.cpp serves /props at the server ROOT. The response
  field default_generation_settings.n_ctx is the server's actual
  context allocation (its -c flag), the true local limit.

Both requests are made only for KEYLESS openai-compat profiles,
never under --no-network, and both are incapable of auth by
construction: each function takes just a base URL and a timeout, so
it cannot attach an auth header or touch a key file even in
principle. Same global timeout (KEYLESS_LISTING_TIMEOUT_SECS, 3s).
The probe is additionally fail-silent: ANY problem (refused, 404,
unparseable body) is None, not an error, because non-llama.cpp
servers answering nothing useful at /props is normal. The provider
e2e test asserts the /props request head carries no authorization,
x-api-key, or bearer material, the same assertion the T15 listing
test makes. No other surface calls either function; list_models_live
(the in-session /models command) remains the only authenticated
listing path and is never called by init or doctor.

## T22 acceptance - recorded result (no release)

2026-07-31, four commits on main over the v0.9.0 baseline (cc28ae4):
P1 b8ced6e (keyless /props probe + doctor context checks + the
amendment extension record above), P2 a17eeb1 (init auto-fill + the
anthropic template's baked context_window 200000), P3 10fd849
(/models context enrichment on the anthropic wire), P4 (docs + this
record). Every phase ran the full check.sh gate (pty, foreground)
green. Version stays 0.9.0; T22 rides CHANGELOG Unreleased. One
harness incident, no code cause: the first P1 gate run hung ~11h in
the container TUI pty smoke step and was killed; the clean re-run
passed end to end, and every later gate run passed first try.

Security posture: the keyless-GET amendment now covers exactly TWO
requests (record above). The anthropic template's context_window
200000 is KNOWLEDGE-BASED, pending the operator's live confirmation
via /models on a real key (the P3 hint/warning notices make any drift
visible); the P2/P3 anthropic-side behavior is mock/fixture-tested
only, no live Anthropic call was made from the build session.

The T22-planned init-closing autosave line was found ALREADY SHIPPED
by T16 P4 (aac3852); it was verified live in this smoke instead of
being duplicated.

Live smoke (Qwen3-1.7B-Q4_K_M via serve.sh with CTX=6144, a value
deliberately distinct from the baked 8192; keyless, isolated XDG
dirs, gnu-debug binary; all first-attempt green, transcripts
verbatim):

(a) init with the server UP auto-filled the real allocation and named
the source, and (d) the closing autosave line printed:

    Model (number or id) [/model.gguf]: Detected a context allocation of 6144 tokens from the server (llama.cpp
    /props, n_ctx); writing "context_window": 6144.
    ...
    Next: start your local server (see docs/OFFLINE.md), run temur doctor
    to check the setup, then temur to start.
    Conversations are saved automatically per working directory; temur --continue
    resumes the last one.

with the written config carrying "context_window": 6144.

(b) doctor with the value hand-edited to 16384 WARNed naming both
values and the consequence direction, then PASSed once corrected:

    WARN: context_window 16384 is larger than the server context allocation (n_ctx 6144) at http://127.0.0.1:8080/v1: the context advisory fires too late and requests can fail at the real limit
    doctor: 7 pass, 1 warn, 0 fail

    PASS: context_window 6144 matches the server context allocation (n_ctx 6144) at http://127.0.0.1:8080/v1
    doctor: 8 pass, 0 warn, 0 fail

and with the value REMOVED, the WARN suggested the exact line:

    WARN: no context_window configured; the server at http://127.0.0.1:8080/v1 allocates n_ctx 6144: add "context_window": 6144 to the profile

(c) doctor --no-network with the value set printed no context line at
all (SKIP lines only); with the value unset it printed exactly the
offline NOTE:

    NOTE: no context_window configured: the context usage advisory and context-scaled tool-output caps are off for this profile

and against a 404-everything non-llama.cpp endpoint (python
http.server) with the value set, the context checks were silent, the
only new line being the pre-existing T15 model-check NOTE (HTTP 404).

Honest residuals: Ollama context detection (/api/show) is deliberately
not built; OFFLINE.md says so and it stays a possible future rider.
The anthropic 200000 confirmation is the operator follow-up above.

## v0.10.0 - close-out (release procedure delta)

What ships: T22 alone (context-window detection + discoverability:
the keyless llama.cpp /props probe and doctor context checks with the
keyless-GET amendment extended to two requests, init context_window
auto-fill and the anthropic template's baked 200000, /models
max_input_tokens enrichment on the anthropic wire), plus the two
planning-approved prose riders (addendum-2 docs claims audit,
P5-follow-up em-dash sweep + AI-agent/bring-your-own-model scope
wording), one CHANGELOG section. Second single-milestone release in a
row.

Procedure deltas vs v0.9.0:

- **T22 pushed AS stage-1 step 1** under the planning session's
  authorization (the standing shape): b8ced6e..9e390e0 onto cc28ae4
  (six commits: four phases + two prose riders), on-push ci run
  30649402355, main == origin == 9e390e0 verified before the prep
  commits. Job timings recorded in the stage-1 report.
- **Stage 1 keeps the EARLY stop**: version bump + dated CHANGELOG +
  records + full check.sh gate only; the four-target release.sh
  build, SHA256SUMS, and installer matrix stay in stage 2 (tag,
  build, private release, closing gate).
- **Still no tag, no push of the release-prep commits, no release**
  at stage 1; the repo stays PRIVATE and the public-flip gate stays
  queued behind stage 2, unchanged.
- Pin-site note for the bump: the four-file map from the v0.9.0 bump
  (86b31f3) was the complete set again (Cargo.toml, Cargo.lock temur
  entry, install.sh VERSION, five README pins); the repo-wide grep
  after the bump comes back empty outside historical records. The
  0.9.0 leftover in Cargo.lock is the untrusted crate's own version
  (this cycle's heck/ureq-proto equivalent), not a temur pin.
- Wording note: the release title should reflect the T22 scope, e.g.
  "temur v0.10.0 - context-window detection (T22)"; stage 2 decides
  the exact string.

## v0.10.0 acceptance - recorded result (SHIPPED, private)

2026-07-31 (local PDT; CI timestamps are Z and cross into 2026-08-01).
v0.10.0 ships T22 alone (context-window detection + discoverability,
plus the two planning-approved prose riders), per the two-stage
procedure. Everything below ran on the private repo only.

- Stage-1 recap: T22 pushed cc28ae4..9e390e0 under planning-session
  authorization, on-push ci run 30649402355 green (test 1m35s,
  release-gate 5m35s); three LOCAL prep commits b9bb443 (bump
  0.9.0 -> 0.10.0, four-file map per 86b31f3) + 2cf4909 (CHANGELOG
  "## v0.10.0 - 2026-07-31") + 45ebaee (RUNBOOK close-out delta +
  ROADMAP ship-vehicle row); full check.sh green at 0.10.0.
- Stage 2 prep push 9e390e0..45ebaee; on-push ci run 30681425471
  green (test 2m25s 03:09:13Z..03:11:38Z, release-gate 7m49s
  03:09:07Z..03:16:56Z) - the version-skew run against the bump.
- Annotated tag v0.10.0 at 45ebaee ("temur v0.10.0 - context
  detection (T22)"), message kept to one short line deliberately
  after the v0.9.0 truncation lesson, verified verbatim via
  git tag -n1 before the push; pushed (tag object 6911606).
  Unsigned as always. The close-out's suggested wording said
  "context-window detection"; stage 2 fixed the exact string as
  "context detection (T22)" in the ship prompt.
- Full release.sh (no SKIP_CHECK): check.sh both paths green
  (bare-container line "temur 0.10.0"), leak grep clean (operator
  patterns + generic shapes, files + history) first try, skew gate
  "OK: install.sh + README match version 0.10.0 and all targets",
  4/4 targets gated + version-asserted "temur 0.10.0" (i686 + x86_64
  native, aarch64 + armv7 via qemu), SHA256SUMS self-verified,
  staged at /home/dev/dist/release/v0.10.0/. The container TUI pty
  smoke passed FIRST TRY in this run (no rerun needed; the stage-1
  gate had needed two reruns for the known flake, see below).
  Installer matrix 6/6 (pass+corrupt+unlisted, host + busybox).
- Staged sha256s: f41d8ae7d275... aarch64, 3e45111a5e7d... armv7,
  d642393f9074... i686, 57bdc0e623e9... x86_64 (full sums in the
  release's SHA256SUMS asset).
- Private release github.com/thekeoni1/Temur/releases/tag/v0.10.0
  created FROM INSIDE the repo worktree per the v0.7.0 procedure
  note, first try, title "temur v0.10.0 - context detection (T22)",
  notes = the CHANGELOG v0.10.0 section, 5 assets (4 binaries +
  SHA256SUMS), not draft; 404s while the repo is private, by
  design. Repo visibility PRIVATE confirmed via gh BEFORE creating
  the release and re-confirmed after.
- Closing gate: authenticated download of the x86_64 binary +
  SHA256SUMS into a scratch dir; sha256sum -c OK; downloaded
  SHA256SUMS byte-identical to the staged file (cmp clean); the
  downloaded binary's independent sha256 (57bdc0e623e9bd98bf950
  58c8c4bc1db168ec515e056295e22d982c8bd4220b2) equals the staged
  one byte for byte (cmp clean).

Honest residuals: the check.sh container TUI pty smoke flake
(scripted keys dropped while the busy latch is set; the T21 P3
readiness fix covers the in-repo harness, not check.sh's podman pty
path) hung TWO stage-1 gate runs this cycle before a clean third run,
then stayed quiet through every stage-2 gate; rider candidate stays
open: readiness-gate or timeout+retry for that step in check.sh. The
anthropic template's baked context_window 200000 remains
knowledge-based until the operator's live /models confirmation
(rides T13 hosted verification, front of queue).

Open release items unchanged: the PUBLIC one-liner gate and the
hostname-blob-history decision stay queued behind the visibility
flip; ARM hardware smoke still pending hardware.

## v0.11.0 - close-out (release procedure delta)

What ships: T23 alone (launch-readiness documentation pass: root
tidy, README rebuild + USAGE reference merge, milestone codes out of
user-facing lead lines, the "How this was built" section, and
scripts/bump_version.sh), plus the operator-approved rider (the
CLAUDE.md preface exactly as drafted in P4, and the README caps
polish dropping the last two ALL-CAPS stress words). Prose, layout,
and one POSIX helper script; zero Rust and zero gate-script changes,
the first release with no behavior delta at all.

Procedure deltas vs v0.10.0:

- **T23 pushed AS stage-1 step 1** under the planning session's
  authorization: a33bd6e..80b0dc3 onto b6d41ed (seven commits: six
  phases + the rider), on-push ci run 30704288792 green (test 59s,
  release-gate 4m56s), main == origin == 80b0dc3 verified before the
  prep commits.
- **The version bump used scripts/bump_version.sh for the first real
  time** (its scratch-branch run under the T23 record was the
  rehearsal): the printed diff touched exactly the four-file map and
  nothing else, and the post-bump repo-wide grep found 0.10.0 only in
  CHANGELOG/ROADMAP/RUNBOOK historical records. The helper stays
  advisory; release.sh gate 3 remains the skew authority.
- Stage 1 keeps the early stop: bump + dated CHANGELOG + records +
  full check.sh gate only; tag, four-target build, SHA256SUMS,
  private release, and the closing gate stay in stage 2. No tag, no
  push of the prep commits, no release; the repo stays PRIVATE.

## T23 acceptance - recorded result (no release)

2026-07-31, six commits on main over the v0.10.0 baseline (b6d41ed),
prose and layout only, no Rust and no gate-script change: P1 a33bd6e
(root tidy: the setup and v1-plan documents to docs/, live
references updated), P2 9474ebb (README rebuilt 533 -> 237 lines,
deep reference merged into docs/USAGE.md 523 -> 854 lines), P3
5406bcb (milestone codes out of user-facing lead lines), P4 f6dff2a
(README "How this was built"; CLAUDE.md deliberately untouched, see
residuals), P5 377a0cf (scripts/bump_version.sh) plus the close-out
commit carrying this record. A baseline check.sh gate ran green at
b6d41ed before any edit, and every phase ran the full check.sh gate
green (pty via script(1), launched as a background task per the
accepted v0.10.0 deviation); the known container TUI pty smoke flake
did not appear once this cycle. Version stays 0.10.0; T23 rides
CHANGELOG Unreleased and ships as v0.11.0.

Hard constraints verified: the compiled-string headings (README
"## Configure", "## Untrusted hosts", docs/OFFLINE.md "## Recommended
small models") survive; the five tag-pinned install lines in README
are byte-identical (grep -Fx against the pre-restructure file, and
release.sh gate 3 logic re-ran green inside every check.sh run); the
tests/cli.rs pointer assertions are untouched and green in all
container suites.

bump_version.sh scratch-branch test (branch t23-bump-test, created at
377a0cf, reverted and deleted after): all three refusal paths
exercised (tag-shaped version exit 2, unchanged version exit 1, dirty
tree exit 1), then a real `bump_version.sh 0.11.0` run produced
exactly the four-file diff (Cargo.toml version, Cargo.lock temur
entry, scripts/install.sh VERSION=0.11.0, and all five README tag-pin
lines flipped to v0.11.0, zero stragglers reported) and committed
nothing. The working tree was verified clean after the revert, main
untouched at version 0.10.0.

Sweeps: git diff origin/main..HEAD added lines carry ZERO em dashes;
the only bare SETUP.md references left repo-wide are two CHANGELOG
history lines (kept as written); every relative markdown link in
README.md and docs/ resolves (scripted check, 7 files, 0 broken).

Honest residuals: the CLAUDE.md preface (framing it as the checked-in
AI-builder instruction set) is DRAFTED but uncommitted, because the
plan requires the operator to see the full file in session and sign
off, and the operator was not available during the build run; it is
the one open T23 item. The README landed at 237 lines against the
~180 target: the byte-identical install block, the nine-row eval
table, and the honesty material were kept whole rather than cut to
the number. The three README badges 404 while the repo is private
(expected; they resolve at the public flip). The P3 Cargo.toml
comment reword covered five comment lines, not just the two the plan
named; the three extras carried the same kind of bare milestone code
and got the same one-word treatment.

## v0.11.0 acceptance - recorded result (SHIPPED, private)

2026-08-01, stage 2 all green, ships T23 launch-readiness docs plus
two riders. Tag lands on the stage-2 rider head, not on the stage-1
prep head, per the stage-2 prompt's step 0.

- Stage-2 rider e919600 "clear stray executable bits (drvfs
  artifact)": 41 tracked files normalized 100755 -> 100644
  (.gitignore, Cargo.lock, seven src/*.rs, ten prompts, twenty
  fixtures, live_conformance.rs, three TUI files), diff mode-only
  with 0 insertions and 0 deletions, every scripts/*.sh verified
  still 755; full check.sh green before commit.
- Push 80b0dc3..e919600 (the three stage-1 prep commits + the
  rider); on-push ci run 30707441331 green (test 1m59s
  16:07:36Z..16:09:35Z, release-gate 7m30s 16:07:43Z..16:15:13Z,
  the version-skew run); main == origin == e919600 verified.
- Annotated tag v0.11.0 at e919600, message exactly "temur v0.11.0 -
  launch-readiness docs (T23)" (one short line per the v0.9.0
  truncation lesson), verified verbatim via git tag -n1 BEFORE the
  push; tag object e6a4aca, pushed and verified via ls-remote.
  Unsigned as always.
- Full release.sh, no SKIP_CHECK, green FIRST TRY (the container TUI
  pty smoke stayed quiet the whole cycle, zero reruns): inner
  check.sh ALL CHECKS PASSED, leak grep clean (operator patterns +
  generic shapes, files + history), skew gate "OK: install.sh +
  README match version 0.11.0 and all targets", 4/4 targets gated
  and version-asserted "temur 0.11.0" (i686 + x86_64 native,
  aarch64 + armv7 qemu), SHA256SUMS self-verified (4/4 OK), staged
  at /home/dev/dist/release/v0.11.0/.
- Staged sha256s: 137e581539f1... aarch64, ffb2c9eeef80... armv7,
  178b4a2a4e1d... i686, b31be50da303... x86_64 (full sums in the
  release's SHA256SUMS asset).
- Private release github.com/thekeoni1/Temur/releases/tag/v0.11.0
  created from inside the repo worktree with absolute asset paths,
  title per tag, notes = the CHANGELOG v0.11.0 section, 5 assets
  (4 binaries + SHA256SUMS), not draft; repo isPrivate true
  confirmed via gh BEFORE creating the release and again after.
- Closing gate: authenticated download of SHA256SUMS + the x86_64
  binary; sha256sum -c OK; downloaded SHA256SUMS byte-identical to
  staged (cmp clean); downloaded x86_64 sha
  (b31be50da303bcea1664c0b551ffdcb6321707499b509fb050503c329a24cf66)
  equals the staged artifact byte for byte (cmp clean).
- Installer matrix 6/6 (pass + corrupt + unlisted, GNU host and
  busybox container).

Honest residuals: none new this stage. Still open and unchanged: ARM
hardware smoke pending hardware; the PUBLIC one-liner gate, the
hostname-blob-history decision, and the demo GIF recording stay
queued behind the visibility flip; T13 hosted verification (with the
anthropic context_window 200000 live /models confirmation) is next in
the queue.

## T13 acceptance - recorded result (no release)

2026-08-03 to 2026-08-05. Live verification of the openai-compat
provider against real hosted endpoints, plus the Anthropic path,
unparked once keys existed. Two-session protocol throughout: the
operator typed every live command in their own terminal, the build
session never saw key material, and every transcript was skimmed
before it was pasted back. Preserved evidence lives outside the repo
at /home/dev/t13-live: the operator checklist with a RESULT block per
leg, the corrupt session that proved finding 10
(evidence/finding10-dangling-tool-use-session.json), and the two curl
captures that settled finding 12 (evidence/f12-stream.txt and
f12-nostream.txt, opaque signature blobs, no credentials).

Provenance note for findings 1 to 8: the original P2 (2026-08-03) and
hosted-legs (2026-08-05) reports arrived as terminal-garbled pastes.
The statements recorded here are the planning session's verified
record, with every code claim confirmed in-tree at verification time
and live values taken from the operator transcripts. Findings 9 to 12
are the build session's own, from the code it changed.

Thirteen findings, with dispositions.

- **Finding 1 (P2). Anthropic template context_window under-reports
  5x.** The anthropic init template baked context_window 200000 into
  all four profiles via the single constant ANTHROPIC_CONTEXT_WINDOW
  (then src/init.rs:90), used by both the wizard row and --add,
  pinned in two goldens, recipe repeated 4x in docs/USAGE.md. The
  live API reported max_input_tokens 1000000 for claude-sonnet-5, so
  a fresh profile started at roughly a fifth of the real window.
  CHANGELOG and RUNBOOK had labeled the 200000 knowledge-based
  pending live confirmation; the confirmation arrived and
  contradicted it. DISPOSITION: fixed inside T13 as P2.5 (178576a), a
  per-model table: claude-sonnet-5, claude-opus-5, claude-fable-5 =
  1000000; claude-haiku-4-5 = 200000, measured on
  claude-haiku-4-5-20251001 (the bare alias is absent from
  /v1/models, so /models can never judge a haiku profile; that blind
  spot is its own roadmap item). Error direction recorded:
  under-reporting fires the usage advisory early, which is safe; a
  blanket 1000000 would have overstated haiku past its real limit and
  failed requests; the per-model table avoids the dangerous
  direction.
- **Finding 2 (P2). Turn footers relabel retroactively.**
  Cell::TurnTail carries no model field; src/ui/tui/view.rs:192
  formats the footer from app.model at draw time, so after /model
  hops the scrollback shows whichever model is active now on every
  past turn. Model switching itself is correct. Cheap fix: carry the
  model in the cell at push time. DISPOSITION: queued (roadmap,
  footer relabel).
- **Finding 3 (P2). The model refuses before the guards can fire.**
  Asked to read a credential-looking path, claude-sonnet-5 refused on
  its own judgment before emitting any tool call, so the naive
  key-isolation prompt tests the model layer, not the product layer,
  and is inconclusive as a guard test. That is real defense in depth,
  but the docs must say it honestly instead of implying the obvious
  prompt proves the guards. The mechanism itself was then tested with
  a harmless 55 byte decoy file registered as a profile key path: the
  read tool was denied with the key isolation message, and sandboxed
  bash saw 0 bytes for the file (the /dev/null bind mask,
  namespace-local), while the file stayed intact outside the
  namespace. DISPOSITION: docs-honesty item, landed in P4 (USAGE key
  isolation note pointing at this record).
- **Finding 4 (P2). Stale installed binary, no skew warning.** The
  installed ~/.local/bin/temur was 0.5.0, four minor versions behind
  the tree; the operator's first three runs exercised the old binary
  and produced old wording, an old doctor, and two false alarms
  before a version assert was added to the checklist. The product
  gives no signal that the running binary is stale. DISPOSITION:
  queued as a feature candidate (version-skew warning).
- **Finding 5 (hosted legs). OpenAI template default absent from the
  live listing.** The openai template's default model, gpt-4o-mini,
  was not among the 11 ids the live account listing returned, so init
  --add openai wrote a profile that could not complete a single call.
  DISPOSITION: fixed inside T13 in the P3.5 template repair
  (6ea78d4); the template now bakes gpt-4o, which passed live.
- **Finding 6 (hosted legs). Hosted templates baked no max_tokens,**
  so openai, gemini, and xai profiles inherited the Anthropic-sized
  global DEFAULT_MAX_TOKENS of 32000 (src/config.rs:9). That exceeds
  gpt-4o's 16384 completion cap, so the first OpenAI turn returned
  400. DISPOSITION: fixed inside T13 in P3.5 (6ea78d4): the openai
  template bakes max_tokens 16384 via the compat_max_tokens lookup;
  gemini bakes none, because Gemini accepted 32000 live.
- **Finding 7 (hosted legs). The gpt-5 family rejects max_tokens**
  and requires max_completion_tokens; our request encoding always
  sends max_tokens. Combined with finding 5 at capture time, every
  current OpenAI model was unreachable from a fresh profile: the
  gpt-4 era ids that accept max_tokens were largely gone from the
  listing, and the gpt-5 ids that remained refused the parameter.
  This is a request encoding change, not a config value.
  DISPOSITION: queued (roadmap, max_completion_tokens support; until
  it lands, gpt-5 era ids stay unreachable and the docs say so).
- **Finding 8 (hosted legs). Gemini template default retired, and
  every listed id is prefixed.** The template's default,
  gemini-2.5-flash, returned 404 "no longer available to new users"
  in Google's own words, and all 59 ids in the live listing carry a
  models/ prefix. DISPOSITION: fixed inside T13 in P3.5 (6ea78d4):
  the template now bakes bare gemini-3.6-flash, which R2 and R2''
  verified reaches the model. Ruling recorded for the docs: the bare
  id is kept deliberately, and appearing in the listing is no
  guarantee an id is usable, since models/gemini-2.5-flash is listed
  and 404s.
- **Finding 9 (hosted legs). Array-wrapped error bodies parse to
  nothing.** Google wraps the OpenAI error envelope in a
  one-element JSON array, which the object-only parser dropped to
  defaults: a live 404 printed "api error (HTTP 404) api_error:" with
  no message at all, hiding the one sentence that explained the
  failure. DISPOSITION: fixed inside T13 in P3.5 (b52ff5e).
  ErrorPayload accepts both shapes at both parse sites. Variant order
  is load-bearing and commented as such: serde builds a struct from a
  sequence positionally, so an object-first untagged enum silently
  reproduces the bug, which it did once during implementation. The
  label also falls back to "status" when a body carries no "type" and
  a numeric "code", which is exactly the captured Gemini shape.
- **Finding 10 (hosted legs). Assembled tool calls were discarded in
  silence.** Gemini's streaming response reports finish_reason "stop"
  while attaching real tool calls; its non-streaming response reports
  "tool_calls" for the identical request, pinned by curl. temur
  streams. The agent loop dispatches only on ToolUse and the
  prose-recovery fallback is guarded by "no ToolUse block present",
  so a well-formed write call fell past every recovery path: no tool
  ran, no file appeared, nothing printed, and the saved session was
  left holding a tool_use with no tool_result. Reproduced on two
  models and two tools. DISPOSITION: fixed inside T13 in P3.5
  (b52ff5e), with rider 3bf706b. A response that assembled tool calls
  means tool use regardless of what finish_reason says or fails to
  say, generalizing the older absent-finish_reason quirk from
  "absent" to "absent or wrong". Refusal is the one exception
  (rider): a filtered completion must not dispatch side-effectful
  calls, and unlike finding 10 the refusal path is visible, discards,
  never auto-retries, and breaks without pushing the assistant turn,
  so it cannot leave a dangling tool_use either. finish_reason
  "length" still surfaces truncation, and now does so even when calls
  were assembled, the truncation riding in stop_details so it
  survives the override.
- **Finding 11 (hosted legs). Thinking tokens are not accounted.**
  Gemini does not report them separately in the usage it returns, so
  /status understates session spend on that provider: the total is a
  floor presented as a total. DISPOSITION: queued (roadmap, usage
  accounting); the docs name the caveat in the meantime.
- **Finding 12 (P3R, and only reachable because finding 10 was
  fixed).** With calls finally dispatching, the Gemini leg got one
  request further and died on the next one: HTTP 400
  INVALID_ARGUMENT, "Function call is missing a thought_signature in
  functionCall parts ... position 2". Single-shot tool calling
  worked; the agent loop broke on its first round trip, which is the
  shape that matters. The operator settled the decisive question by
  curl in both wire modes: Google does hand us the signature, at
  tool_calls[].extra_content.google.thought_signature, streaming and
  non-streaming alike, and our ToolCallDelta had no such field, so
  serde dropped it on the way in and there was nothing left to echo
  on the way out. DISPOSITION: fixed inside T13 as P3.6 (efe1e5b)
  with rider 8605e14. The neutral ContentBlock::ToolUse gains one
  optional opaque field, provider_state, documented against the
  ContentBlock::Thinking.signature precedent it copies; openai-compat
  fills it from extra_content and re-emits it verbatim; the anthropic
  converter drops it in both directions, exactly as openai-compat
  drops Anthropic thinking signatures, so a session that switched
  providers cannot leak one wire's opaque state into the other's
  request. Absent is the common case and costs nothing:
  skip_serializing_if keeps the field out of the JSON entirely, so
  request goldens and session files are byte-identical wherever it
  does not apply.

- **Finding 13 (P4 gate). The TUI busy-loops when stdin is not a
  TTY.** Found by the P4 gate hanging twice at the container TUI pty
  smoke, and worth separating from the harness flake it was filed as
  for three release cycles: the container's TUI came up correctly and
  rendered its prompt ("# new session", "ask anything"), the smoke's
  scripted input was never consumed, and no turn ever ran, but the
  process did not sit idle waiting. It emitted roughly 1.6 KB/s of
  redraw output at about 7% CPU for the entire 2.5 hour stall,
  measured live (2662252 bytes at T0, 2670469 five seconds later) and
  totalling 2.6 MB of ANSI redraws for one static screen. podman's own
  warning names the condition: "The input device is not a TTY".
  Whether the spin is reachable outside this harness is NOT
  established, and deliberately was not investigated mid-milestone.
  Rider 2 closed part (b) and, on a reproduction of the same shape
  (the container TUI left redrawing with no alt-screen leave),
  measured the stall instead of inferring it: the output is a steady
  ten redraws per second rather than a busy loop, it is byte-for-byte
  unchanged when stdin is held open past the turn, so pipe EOF is not
  the trigger, and what strands the TUI is the smoke's own blind
  timing, since container startup measured between 1.8 and 3.0
  seconds against a one second sleep while the mock turn takes 0.2
  seconds, so a slow start lands the scripted "exit" Enter inside the
  running turn where the TUI ignores Enter by design.
  DISPOSITION: queued in two parts (roadmap): (a) product, the TUI
  should block on its event source or refuse non-TTY stdin with an
  error pointing at one-shot `-p`; (b) harness, check.sh needs a
  readiness gate or timeout on the pty smokes so a hang fails in
  minutes with a diagnosis rather than sitting for hours.

Live verification results, per provider:

- **Anthropic**: verified 2026-08-03 and 2026-08-04. Four-profile
  template, doctor with the sandbox PASS gate, one agentic turn with
  autosave, profile hops, both key-isolation arms, and the per-model
  max_input_tokens capture that produced finding 1's table.
- **OpenAI**: verified 2026-08-05 on gpt-4o. The write-tool turn
  succeeded first try (17 bytes, correct content, session autosaved,
  no key material in either artifact) once findings 5 and 6 were
  worked around by hand; both are now fixed in the template. The
  regression leg after the finding 10 fix passed unchanged, which was
  its purpose: the override sits in conversion code the OpenAI path
  shares.
- **Gemini**: verified 2026-08-05 on the P3.6 binary, after two
  failed attempts that produced findings 8, 10, and 12. The final run
  is a full pass of the whole loop, not just the first response:
  three tool calls (glob, write, read) across four requests, every
  round trip carrying the signature back, no 400, final assistant
  text, and the file on disk with the right single line. The saved
  session holds 3 tool_use and 3 tool_result, each paired, and all
  three signatures preserved (2460, 376, and 176 chars).
- **xAI**: not verified. No key was available; the template is
  written to the published spec and the docs say so.

Gate results: full scripts/check.sh green at every code phase (P1,
P2.5, P3.5, the P3.5 rider, and P3.6). The musl TUI pty smoke hung
once during the P3.6 rider gate, ten minutes of silence with the
container still up; recovery was to kill the run, podman kill the
stuck container, confirm no remnants, and rerun clean, which passed.

P4's own gate never went green in this environment, and that is
recorded here rather than smoothed over. Two consecutive runs on the
P4 commit hung at the gnu-debug container TUI pty smoke (the second
for 2.5 hours before it was stopped), which is finding 13 above. Both
runs passed everything up to that step: all fifteen test suites, both
mock REPLs, and the host pty smoke; neither reached the musl
acceptance path. The planning session accepted P4 on the evidence
rather than ordering a third identical rerun: P4 is docs-only, the
binary it gates is byte-identical to the one that had just taken a
full green gate at 8605e14, and everything but the one hanging step
was green twice. check.sh was not modified to work around it.

Honest residuals:

- Four findings ship unfixed by deliberate ruling: 2, 4, 7, and 11,
  all queued on the roadmap with the docs naming the two that a user
  can actually hit (gpt-5 era ids unreachable, Gemini spend
  understated).
- xAI remains unverified, and no amount of care in the template
  substitutes for one live call.
- The R2 session landing from the finding 12 failure was overwritten
  before it was captured: sessions are one file per working
  directory, and the OpenAI regression leg ran in the same directory
  next. Accepted rather than reproduced, since the failure was a
  provider error and the landing policy has its own tests.
- The Gemini template default is a concrete pinned id, which is what
  retired under us once already. It is kept deliberately: it reached
  the model live, and listing membership is not a usability
  guarantee.
- One cosmetic, unfiled beyond the roadmap queue: the /models listing
  rendered two ids on one line in the operator's terminal, and a
  refused turn that had already streamed a tool call leaves its tool
  cell open (pre-existing on the refusal path, reachable again now
  that Refusal is excluded from the dispatch override).

## v0.12.0 - close-out (release procedure delta)

What ships: T13 alone (hosted-provider verification: the openai-compat
provider run against the real OpenAI and Gemini endpoints, with the
init key-path question, the anthropic per-model context_window, the
four openai-compat correctness fixes, the Gemini thought-signature
round-trip, and the docs plus acceptance record), plus the two P4
riders (finding 13 promoted from harness flake to product finding on
measured evidence, and the container TUI pty smoke re-gated on the
app rather than on the clock). Third single-milestone release in a
row, and the first whose ship head is a gate-script commit.

Procedure deltas vs v0.11.0:

- **T13 pushed AS stage-1 step 1** under the planning session's
  authorization (the standing shape): faf202d..6c9a3ee onto de9901d
  (ten commits: six phases and phase riders, plus the two P4 riders),
  on-push ci run 31211229630 green, main == origin == 6c9a3ee
  verified before the prep commits. Job timings recorded in the
  stage-1 report.
- **The check.sh smoke stalls that shadowed the last three cycles are
  fixed, not endured.** Every prior stage-1 record carried a
  kill-and-rerun note for the container TUI pty smoke; rider 2
  root-caused it to the smoke's own blind timing and replaced the
  sleeps with readiness and turn gates over a held-open fifo, bounded
  at 180s. The stage-3 prep gate is therefore the first stage-1 gate
  run whose smoke either passes or fails loudly, and a hang here is a
  stop-and-report, not a rerun.
- **The version bump used scripts/bump_version.sh** for the second
  time, printed diff touching exactly the four-file map: Cargo.toml,
  the temur line of Cargo.lock (the third-party untrusted crate stays
  pinned at its own 0.9.0), scripts/install.sh VERSION, and the five
  README tag-pin lines. The helper stays advisory; release.sh gate 3
  remains the skew authority.
- Stage 1 keeps the early stop: bump + dated CHANGELOG + records +
  full check.sh gate only; tag, four-target build, SHA256SUMS,
  private release, installer matrix, and the closing gate stay in
  stage 2. No tag, no push of the prep commits, no release; the repo
  stays PRIVATE.

## v0.12.0 acceptance - recorded result (SHIPPED, private)

2026-08-07, stage 2 all green, ships T13 hosted-provider verification
plus the two P4 riders. No pre-tag rider this cycle: the tag lands
directly on the stage-1 prep head 837995c.

- Push 6c9a3ee..837995c (the three stage-1 prep commits: 3c60b89
  version bump, 537fa1b CHANGELOG release cut, 837995c docs
  close-out); on-push ci run 31225282702 green (test 2m09s
  22:52:23Z..22:54:32Z, release-gate 7m32s 22:52:23Z..22:59:55Z);
  main == origin == 837995c verified.
- Annotated tag v0.12.0 at 837995c, message exactly "temur v0.12.0 -
  hosted verification (T13)" (one short line), verified verbatim via
  git cat-file tag BEFORE the push: object line 837995c, message that
  one line and nothing more. Tag object
  5ee4adac5287adb0014586c2f2077931e3d23844, pushed. Unsigned as
  always. Not retagged.
- Full release.sh, no SKIP_CHECK, green FIRST TRY: inner check.sh ALL
  CHECKS PASSED, leak grep clean (operator patterns + generic shapes,
  files + history), skew gate "OK: install.sh + README match version
  0.12.0 and all targets", 4/4 targets gated and version-asserted
  "temur 0.12.0" (i686 + x86_64 native, aarch64 + armv7 qemu),
  SHA256SUMS self-verified (4/4 OK), staged at
  /home/dev/dist/release/v0.12.0/.
- **Rider 2's smoke fix held on its second independent exercise.**
  All three TUI pty smokes in this gate run (host gnu-debug,
  container gnu-debug, container musl) passed without stalling, well
  inside the 180s bound, on the first attempt. Zero kill-and-rerun
  for the first time in four cycles; the bare busybox leg printed
  "temur 0.12.0" as expected.
- Staged sha256s: db27e673ea86... aarch64, 8cc324b0c5b6... armv7,
  53726b1b5e02... i686, 44e81cdb9102... x86_64; SHA256SUMS itself
  f34a00c0c09f... (full sums in the release's SHA256SUMS asset).
- Private release github.com/thekeoni1/Temur/releases/tag/v0.12.0
  created with title per tag, notes = the CHANGELOG v0.12.0 section,
  5 assets (4 binaries + SHA256SUMS), not draft; repo isPrivate true
  confirmed via gh BEFORE creating the release and again after.
- Closing gate, fresh files in a scratch dir: downloaded x86_64 sha
  44e81cdb9102327d2765e3bdfecb0e05a4df2a06848c3986249f55b4be6922eb
  equals the staged value and cmp against the staged archive is
  clean; the downloaded SHA256SUMS is cmp-identical to the staged
  one.
- Installer matrix 6/6 twice: once against the staged directory and
  once against a fresh full download of all five published assets
  (pass + corrupt + unlisted, GNU host and busybox container).

Honest residuals: none new this stage. The ARM artifacts remain
verified at build level under qemu only, hardware smoke pending
hardware. Still queued behind the visibility flip: the PUBLIC
one-liner gate, the hostname-blob-history decision, and the demo GIF
recording. Also open and unchanged from the T13 record: the anthropic
1M-context tiers were confirmed against the live models API but the
hosted acceptance itself covered OpenAI and Gemini, so no live
Anthropic agent-loop run rides this release.

## v0.13.0 - close-out (release procedure delta)

What ships: T24 alone (session cost visibility: the `/status` estimate
line for keyed priced profiles, the pure `src/cost.rs` computation
with Anthropic's cache multipliers, the per-model list rates baked
into the anthropic template, and the docs that name both error
directions). Fourth single-milestone release in a row.

Procedure deltas vs v0.12.0:

- **T24 pushed AS stage-1 step 1** under the planning session's
  authorization (the standing shape): 78efab3..e4c99cf onto 78efab3
  (three commits: P1+P2, P3, P4), on-push ci run 31231658836 green,
  main == origin == e4c99cf verified before the prep commits. Job
  timings recorded in the stage-1 report.
- **T24 was gate-verified offline, by design.** No live smoke rides
  this milestone: the estimate is computed locally from usage counts
  the provider already reported, so it is offline-computable end to
  end, and the one thing a live run would add is a keyed session
  showing the line in situ. That check rides a future operator
  session rather than blocking the release.
- **The version bump used scripts/bump_version.sh** for the third
  time, printed diff touching exactly the four-file map: Cargo.toml,
  the temur line of Cargo.lock (the third-party untrusted crate stays
  pinned at its own 0.9.0), scripts/install.sh VERSION, and the five
  README tag-pin lines. The helper stays advisory; release.sh gate 3
  remains the skew authority.
- **One unreproduced test failure from the T24 build cycle stays on
  watch.** A single `--lib` run failed once during the build session
  and never again, and the failing test's name was not captured, so
  there is nothing to reproduce against. Every gate run since has
  been green. The standing instruction for this cycle and the next:
  if any test fails even once, capture the exact test name and output
  before anything else. A name is the thing we lack.
- Stage 1 keeps the early stop: bump + dated CHANGELOG + records +
  full check.sh gate only; tag, four-target build, SHA256SUMS,
  private release, installer matrix, and the closing gate stay in
  stage 2. No tag, no push of the prep commits, no release; the repo
  stays PRIVATE.

## v0.13.0 acceptance - recorded result (SHIPPED, private)

2026-08-07, stage 2 all green, ships T24 alone (the session cost
estimate). No pre-tag rider this cycle: the tag lands directly on the
stage-1 prep head 2d389b5.

- Push e4c99cf..2d389b5 (the three stage-1 prep commits: be2bf73
  version bump, 7acbd0b CHANGELOG release cut, 2d389b5 docs
  close-out); on-push ci run 31233011210 green (test 2m08s
  01:35:35Z..01:37:43Z, release-gate 8m20s 01:35:35Z..01:43:55Z);
  main == origin == 2d389b5 verified.
- Annotated tag v0.13.0 at 2d389b5, message exactly "temur v0.13.0 -
  session cost estimate (T24)" (one short line), verified verbatim via
  git cat-file tag BEFORE the push: object line 2d389b5, message that
  one line and nothing more, 43 bytes, ASCII hyphen, no trailing text.
  Tag object 5c650abc7c4bafde3f3f72ee35b8075010b98d8c, pushed. Unsigned
  as always. Not retagged.
- Full release.sh, no SKIP_CHECK, green FIRST TRY: inner check.sh ALL
  CHECKS PASSED, leak grep clean (operator patterns + generic shapes,
  files + history), skew gate "OK: install.sh + README match version
  0.13.0 and all targets", 4/4 targets gated and version-asserted
  "temur 0.13.0" (i686 + x86_64 native, aarch64 + armv7 qemu),
  SHA256SUMS self-verified (4/4 OK), staged at
  /home/dev/dist/release/v0.13.0/.
- **The full gate output was teed to a log file this cycle**, a new
  procedure step, with no tail in the pipe, so that a single test
  failure would keep its name. Nothing to catch: every "test result:"
  line in the 281-line log reads "0 failed", across all three test
  legs (host i686-gnu, container gnu-debug, container musl-release).
  The unreproduced --lib failure from the T24 build cycle therefore
  stays unnamed and unreproduced; it did not recur here. The capture
  procedure is cheap and stays on for the next cycle.
- **Rider 2's smoke fix held on its third independent exercise.** All
  three TUI pty smokes (host, container gnu-debug, container musl)
  passed on the first attempt, well inside the 180s bound. Second
  consecutive cycle with zero kill-and-rerun; the bare busybox leg
  printed "temur 0.13.0" and its mock REPL passed.
- Staged sha256s: 6837e458ed6b... aarch64, 76c1f46d7534... armv7,
  05114b34bb23... i686, b5dfa1d70b3c... x86_64; SHA256SUMS itself
  79d0ba124453... (full sums in the release's SHA256SUMS asset).
- Private release github.com/thekeoni1/Temur/releases/tag/v0.13.0
  created with title per tag, notes = the CHANGELOG v0.13.0 section,
  5 assets (4 binaries + SHA256SUMS), not draft; repo isPrivate true
  confirmed via gh BEFORE creating the release and again after.
- Closing gate, fresh files in a scratch dir: downloaded x86_64 sha
  b5dfa1d70b3c33eb89edccfc1d3b2ca2e583aabeee97d9b363e3996c10724b48
  equals the staged value and cmp against the staged binary is clean;
  the downloaded SHA256SUMS is cmp-identical to the staged one.
- Installer matrix 6/6 twice: once against the staged directory and
  once against a fresh full download of all five published assets
  (pass + corrupt + unlisted, GNU host and busybox container). That
  fresh download also self-verified 4/4 against the published
  SHA256SUMS.

Honest residuals: none new this stage. T24 shipped without a live
keyed session showing the estimate line in situ, by design (see the
close-out); that check rides a future operator session. The ARM
artifacts remain verified at build level under qemu only, hardware
smoke pending hardware. Still queued behind the visibility flip: the
PUBLIC one-liner gate, the hostname-blob-history decision, and the
demo GIF recording.

## T25 acceptance - recorded result (no release)

2026-08-10. Both T25 fixes taken to the real endpoints, one arm each,
under the standing two-session protocol: the operator typed every live
command in their own terminal, the build session never saw key
material, and each transcript was skimmed before it was pasted back.
Preserved evidence lives outside the repo at /home/dev/t13-live:
evidence/f7-400-max-tokens.txt (the OpenAI 400) and
evidence/t25-gemini.0.sse (the Gemini streaming capture, 858 bytes),
with work/t25-gpt5.txt as the OpenAI arm's tool-turn artifact.

**F7, the token cap under the gpt-5 wire name.** Profile gpt5scratch,
model gpt-5, base https://api.openai.com/v1. WITHOUT
max_tokens_parameter the first turn failed, and the 400 body that T13
described in prose on 2026-08-05 but never captured is now on disk,
rendered as:

    provider error: api error (HTTP 400) invalid_request_error:
    Unsupported parameter: 'max_tokens' is not supported with this
    model. Use 'max_completion_tokens' instead.

That is one line in the product's output; the evidence file wraps it
across two, and the wrapped second line carries the TUI's gutter
chrome, which is not part of the server message. With
"max_tokens_parameter": "max_completion_tokens" set on the same
profile, the same prompt completed normally and a tool turn wrote
work/t25-gpt5.txt, 28 bytes, "max_completion_tokens works". The
parameter was accepted, and no other field was objected to: the fix is
exactly as wide as it needed to be. What the leg did NOT show is that
the cap's VALUE is enforced. The turn answered in one token, which
proves nothing about a 32000 ceiling, so the claim on both sides is
acceptance of the parameter, not enforcement of the number.

**F11, the thinking-token gap through include_usage.** One streaming
turn captured with --capture-sse (gemini-3.6-flash). Wire usage:
prompt_tokens 6498, completion_tokens 1, total_tokens 6526, so the gap
is 27. The saved session recorded session_usage input_tokens 6498,
output_tokens 28, both cache fields null, and last_context_used 6526.
28 is 1 + 27, so the fold survives the accumulator on the path temur
actually uses. Pre-fix, that turn would have reported 1 output token
against a real 28.

**New wire fact: usage repeats on every chunk.** Gemini sends usage on
EVERY chunk, not only a final one. Both chunks in the capture carry
identical usage objects, and the finish chunk has a NON-EMPTY choices
array (it also carries a thought signature). temur reports 28 rather
than 56 only because ChunkAccumulator::push assigns usage last-wins
instead of accumulating (src/provider/openai_compat/types.rs:486). An
additive assembly would have doubled the count on every Gemini turn,
so this was right by construction rather than by intent. The
Chunk::usage doc comment had said "Final-chunk-only", which the
capture disproves; it is corrected in this rider, citing the capture
date and file. The accumulator's own comment about a usage-only final
chunk with an empty choices array is left alone: that shape is still
real for OpenAI. tests/fixtures/openai/gemini_thinking_gap.sse, whose
envelope T25 P2 had modeled rather than captured, is rebuilt from this
capture (two chunks, identical usage on both, finish_reason "stop" and
a non-empty choices array on the second, opaque signature blob
shortened), and its test now pins that repeated usage is counted once
rather than summed.

**F12 bonus observation, now verified rather than assumed.** The
thought signature arrived on a plain assistant delta
(delta.extra_content.google.thought_signature) on a turn that made no
tool call, where T13 F12 had only ever seen it attached to tool calls.
temur's Delta type has no extra_content field, so a plain-delta
signature is parsed away by design. This paragraph originally said
there was nothing to fix, which was an inference at the time it was
written; the operator then went and tested it. On 2026-08-10 a
tool-free Gemini session ran two consecutive thinking turns, captured
at t13-live/evidence/t25-f14.0.sse and t25-f14.1.sse. Turn one answered
"one" and carried a signature on its finish delta; turn two answered
"two" with turn one already in history (prompt_tokens rose from 6498 to
6511) and completed normally, with no signature echoed back and no
rejection. So the echo requirement F12 found is tool-call-scoped: the
ToolCall path carries those verbatim, which T13 verified live across
four requests, and dropping a plain-delta signature is now known-safe
rather than known-harmless-by-hope. It is still worth knowing that the
field is broader than the tool-call path implies, if a future 400 ever
points at a text-only turn.

Gate: full `scripts/check.sh` green first try under a pty, both paths,
no env overrides and no reruns, ALL CHECKS PASSED with 30 suite
results and zero failures (openai_compat 45 in both the gnu-debug and
musl-release containers, unchanged in count since the rebuilt fixture
replaced a modeled one rather than adding a case). Both container pty
smokes and the host pty smoke were quiet: no hang, no kill, no rerun.
Bare busybox container printed
"temur 0.13.0"; the version is unchanged and this rider still rides
Unreleased.

Honest residuals:

- gpt5scratch was a scratch profile in the operator's live tree
  outside the repo. Nothing about gpt-5 is baked anywhere: the OpenAI
  template still defaults to gpt-4o, which still wants the classic
  parameter name.
- Neither live profile configured prices, so T24's cost line was
  correctly absent on both arms. The cost estimate is still
  unexercised live, and that check continues to ride a future keyed
  session.
- One model per arm, gpt-5 for F7 and gemini-3.6-flash for F11, and
  one turn each. Other gpt-5 era ids and other Gemini models are
  inferred from the same wire behavior rather than tested. The F11
  turn was a short text turn (1 reported completion token), so the
  fold is verified on the streaming path rather than at scale.
- xAI remains unverified, unchanged by this run.

## v0.14.0 - close-out (release procedure delta)

What ships: T25 alone (the token cap under either wire name via
`max_tokens_parameter`, and the `total_tokens` fold that recovers
Gemini's unnamed thinking tokens into the output count), plus the
operator live leg that turned both claims from offline-verified into
live-verified and the acceptance record it produced. Fifth
single-milestone release in a row.

Procedure deltas vs v0.13.0:

- **T25 pushed AS stage-1 step 1** under the planning session's
  authorization (now the standing shape): 54d2c62..595aab1 onto
  54d2c62, four commits (P1 the request-side fix, P2 the response-side
  fix, P3 docs plus the staged live checklist, P5 the post-leg claim
  finalization), on-push ci run 31441954564 on headSha 595aab1 green
  in both jobs (test 1m27s, release-gate 4m38s). main == origin ==
  595aab1 verified before the prep commits.
- **A real operator live leg rode this milestone**, unlike v0.13.0.
  Both arms ran against real hosted endpoints under the two-session
  protocol, the build session never saw key material, and the leg
  changed the shipped content rather than merely confirming it: it
  captured a 400 body that had only ever been described in prose,
  disproved a doc comment about final-chunk-only usage, and caused a
  test fixture to be rebuilt from a real capture instead of a modeled
  envelope. Evidence stays outside the repo at /home/dev/t13-live.
- **The version bump used scripts/bump_version.sh** for the fourth
  time, printed diff touching exactly the four-file map: Cargo.toml,
  the temur line of Cargo.lock (the third-party untrusted crate stays
  pinned at its own 0.9.0, and does not appear in the lock diff at
  all), scripts/install.sh VERSION, and the five README tag-pin lines.
  The helper stays advisory; release.sh gate 3 remains the skew
  authority.
- **Three doc riders were deliberately folded into this close-out
  commit** rather than pushed as their own gate cycle. They are prose
  only, no Rust, so riding an existing gate costs nothing and spends
  one gate run instead of two. Two of them narrow claims the live leg
  did not actually establish: the README bullet and the RUNBOOK T25
  record both said the server accepted the cap instead of silently
  dropping it, when what the leg showed was that the PARAMETER was
  accepted. The turn answered in one token, which says nothing about
  whether a 32000 ceiling is enforced. The third upgrades the F12
  bonus observation from inference to evidence: the tool-free
  two-turn Gemini captures at t13-live/evidence/t25-f14.0.sse and
  t25-f14.1.sse show a plain-delta signature going un-echoed with no
  rejection on the following turn, so the echo requirement is
  confirmed tool-call-scoped.
- **The unreproduced test failure from the T24 build cycle stays on
  watch.** It has not recurred in any gate run since. The standing
  instruction carries into this cycle unchanged: if any test fails
  even once, capture the exact test name and full output before
  anything else. A name is still the thing we lack.
- Stage 1 keeps the early stop: bump + dated CHANGELOG + records +
  full check.sh gate only; tag, four-target build, SHA256SUMS,
  private release, installer matrix, and the closing gate stay in
  stage 2. No tag, no push of the prep commits, no release; the repo
  stays PRIVATE.

## v0.14.0 acceptance - recorded result (SHIPPED, private)

2026-08-11, stage 2 all green, ships T25 alone (the token cap under
either wire name, and the thinking-token fold). No pre-tag rider this
cycle: the tag lands directly on the stage-1 prep head 0db4ccf.

- Push 595aab1..0db4ccf (the three stage-1 prep commits: 32992fe
  version bump, 3faf21a CHANGELOG release cut, 0db4ccf docs
  close-out); on-push ci run 31516204323 green (test 2m04s
  17:10:25Z..17:12:29Z, release-gate 9m33s 17:10:25Z..17:19:58Z);
  main == origin == 0db4ccf verified.
- Annotated tag v0.14.0 at 0db4ccf, message exactly "temur v0.14.0 -
  wire fixes (T25)" (one short line), verified verbatim via git
  cat-file tag BEFORE the push: object line 0db4ccf, message that one
  line and nothing more, 33 bytes, hexdumped to confirm the separator
  is an ASCII hyphen 0x2d and the only other control byte is the
  closing 0x0a, cmp-identical to the intended string. Tag object
  6d63d92223fc4ac54c723fd17119ecc8633fd5e5, pushed. Unsigned as
  always. Not retagged.
- Full release.sh, no SKIP_CHECK, green FIRST TRY: inner check.sh ALL
  CHECKS PASSED, leak grep clean (operator patterns + generic shapes,
  files + history), skew gate "OK: install.sh + README match version
  0.14.0 and all targets", 4/4 targets gated and version-asserted
  "temur 0.14.0" (i686 + x86_64 native, aarch64 + armv7 qemu),
  SHA256SUMS self-verified (4/4 OK), staged at
  /home/dev/dist/release/v0.14.0/.
- The teed-log procedure stays on, second cycle. All 48 "test result:"
  lines in the 279-line log read "0 failed", zero panics, across all
  three test legs (host i686-gnu, container gnu-debug, container
  musl-release). The unreproduced --lib failure from the T24 build
  cycle did not recur here either, so it stays unnamed and
  unreproduced across two full ship cycles now. The capture procedure
  is cheap and stays on.
- **The pty smoke fix held on its fourth independent exercise.** All
  three TUI pty smokes (host, container gnu-debug, container musl)
  passed on the first attempt, inside the 180s bound, in both the
  stage-1 gate and this release.sh run. Third consecutive cycle with
  zero kill-and-rerun; the bare busybox leg printed "temur 0.14.0" and
  its mock REPL passed.
- Staged sha256s: 162ac2c9d218... aarch64, 60431f598eb4... armv7,
  bc965146dfb1... i686, 177e6a49d113... x86_64; SHA256SUMS itself
  f3e5038d9342... (full sums in the release's SHA256SUMS asset).
- Private release github.com/thekeoni1/Temur/releases/tag/v0.14.0
  created with title per tag, notes = the CHANGELOG v0.14.0 section,
  5 assets (4 binaries + SHA256SUMS), not draft; repo isPrivate true
  confirmed via gh BEFORE creating the release and again after.
- Closing gate, fresh files in a scratch dir: downloaded x86_64 sha
  177e6a49d1138c714012bf27b57433fa50ca1cfcf2a7de8462cd6e7fc917526a
  equals the staged value and cmp against the staged binary is clean;
  the downloaded SHA256SUMS is cmp-identical to the staged one.
- Installer matrix 6/6 twice: once against the staged directory and
  once against a fresh full download of all five published assets
  (pass + corrupt + unlisted, GNU host and busybox container). That
  fresh download also self-verified 4/4 against the published
  SHA256SUMS.

Honest residuals: none new this stage. The two claim narrowings and
the F12-to-evidence upgrade landed in the stage-1 close-out rather
than here, so the shipped docs already say only what the live leg
showed: the gpt-5 arm proves the PARAMETER is accepted, not that the
cap's value is enforced. T24's cost line is still unexercised live and
continues to ride a future keyed session. The ARM artifacts remain
verified at build level under qemu only, hardware smoke pending
hardware. Still queued behind the visibility flip: the PUBLIC
one-liner gate, the hostname-blob-history decision, and the demo GIF
recording.

## v0.15.0 - close-out (release procedure delta)

What ships: T26 alone, the mid-session cost advisory. It is the
escalated half of the dogfood cost item, whose other half shipped as
T24 in v0.13.0: `/status` could already answer "what has this cost",
but only to someone who thought to ask, and the run that motivated the
whole item reached roughly $26 inside ONE agentic `-p` turn and was
found afterward by pricing the usage line by hand. Sixth
single-milestone release in a row.

Procedure deltas vs v0.14.0:

- **T26 pushed AS stage-1 step 1**, the standing shape for the third
  cycle: 46e2c9f..05188f9 onto 46e2c9f, three commits (P1 the pure
  crossing arithmetic plus the config knob, P2 the session plumbing,
  P3 docs), on-push ci run 31523491975 on headSha 05188f9 green in
  both jobs (test 1m14s, release-gate 5m26s). main == origin ==
  05188f9 verified before the prep commits.
- **No live leg, by design and stated in advance.** The feature is
  offline-computable end to end: the estimate is arithmetic over token
  counts the provider already reported, and the trigger is arithmetic
  over the estimate. The mock provider drives spend across a threshold
  mid-turn, across two thresholds in one response, and across a
  resume, which is every behavior the milestone claims. T24's keyed
  live check still rides a future operator session, and T26 inherits
  that residual rather than adding one.
- **The version bump used scripts/bump_version.sh** for the fifth
  time, printed diff touching exactly the four-file map: Cargo.toml,
  the temur line of Cargo.lock (the third-party untrusted crate stays
  pinned at its own 0.9.0 and does not appear in the lock diff at
  all), scripts/install.sh VERSION, and the five README tag-pin lines.
- **One code rider rode this close-out**, a single-character fix to
  punctuation the build session had introduced two commits earlier: an
  em-dash in the T26 turn-loop comment in src/agent/mod.rs, replaced
  with a colon. It is recorded because the standing rule is BOTH no
  em-dashes in new prose AND no introduce-then-sweep, and this cycle
  broke the first and then used the second to repair it. The rider is
  the honest way to close that, not evidence the rule works.
- **The em-dash sweep's real scope, stated plainly.** The
  user-facing prose files are at zero and stay there: README.md,
  CHANGELOG.md, and ROADMAP.md have no em-dash at all. The character
  remains common in src/, tests/, scripts/, and docs/, roughly 450
  hits, all predating this milestone: product UI strings the tests pin
  verbatim, Rust doc and line comments, and quoted live transcripts in
  the records. The check that matters per cycle is therefore
  differential, not absolute: no line ADDED since the previous
  release head may contain one, which is what was verified here after
  the rider.
- **The unreproduced test failure from the T24 build cycle stays on
  watch.** Three full gate runs this build cycle plus the stage-1 gate,
  all green first try, and it has still never recurred. The standing
  instruction is unchanged: if any test fails even once, capture the
  exact test name and full output before anything else.
- Stage 1 keeps the early stop: bump + dated CHANGELOG + records +
  full check.sh gate only; tag, four-target build, SHA256SUMS,
  private release, installer matrix, and the closing gate stay in
  stage 2. No tag, no push of the prep commits, no release; the repo
  stays PRIVATE.

## v0.15.0 acceptance - recorded result (SHIPPED, private)

2026-08-11, stage 2 all green, ships T26 alone (the mid-session cost
advisory). No pre-tag rider this cycle: the tag lands directly on the
stage-1 prep head 32a458d.

- Push 05188f9..32a458d (the three stage-1 prep commits: 81dba18
  version bump, 217a937 CHANGELOG release cut, 32a458d docs
  close-out); on-push ci run 31526177340 on headSha 32a458d green in
  both jobs (test 13m13s 19:06:32Z..19:19:45Z, release-gate 8m27s
  19:06:33Z..19:15:00Z); main == origin == 32a458d verified.
- **The test job's 13 minutes were runner infrastructure, not this
  repo.** Roughly the first 12 were spent inside step 3, "Install
  32-bit build and run packages", before the build step started; the
  suite itself ran in its usual time once it reached it. Worth
  recording so the next cycle reads a slow apt mirror as what it is
  rather than as a regression, and so a genuinely slow TEST step is
  still distinguishable.
- Annotated tag v0.15.0 at 32a458d, message exactly "temur v0.15.0 -
  mid-session cost advisory (T26)" (one short line), verified verbatim
  via git cat-file tag BEFORE the push: object line 32a458d, message
  that one line and nothing more, 48 bytes, hexdumped to confirm the
  separator is an ASCII hyphen 0x2d at offset 0x0e and the only other
  control byte is the closing 0x0a, cmp-identical to the intended
  string, no byte outside printable ASCII. Tag object
  4a6e906700e768bd66a0a5dada7fde30c6851934, pushed. Unsigned as
  always. Not retagged.
- Full release.sh, no SKIP_CHECK, green FIRST TRY: inner check.sh ALL
  CHECKS PASSED, leak grep clean (operator patterns + generic shapes,
  files + history), skew gate "OK: install.sh + README match version
  0.15.0 and all targets", 4/4 targets gated and version-asserted
  "temur 0.15.0" (i686 + x86_64 native, aarch64 + armv7 qemu),
  SHA256SUMS self-verified (4/4 OK), staged at
  /home/dev/dist/release/v0.15.0/.
- The teed-log procedure stays on, third cycle. All 48 "test result:"
  lines in the 278-line log read "0 failed", zero panics, across all
  three test legs (host i686-gnu, container gnu-debug, container
  musl-release). The unreproduced --lib failure from the T24 build
  cycle did not recur, so it stays unnamed and unreproduced across
  three full ship cycles now, plus this cycle's four build-side gate
  runs. The capture procedure is cheap and stays on.
- **The pty smoke fix held on its fifth independent exercise.** All
  three TUI pty smokes (host, container gnu-debug, container musl)
  passed on the first attempt, inside the 180s bound, in both the
  stage-1 gate and this release.sh run. Fourth consecutive cycle with
  zero kill-and-rerun; the bare busybox leg printed "temur 0.15.0" and
  its mock REPL passed.
- Staged sha256s: 2491a2a44028... aarch64, 501d8005ee0b... armv7,
  d651725bfcd4... i686, 9003343c4d85... x86_64; SHA256SUMS itself
  36c494df9949... (full sums in the release's SHA256SUMS asset).
- Private release github.com/thekeoni1/Temur/releases/tag/v0.15.0
  created with title per tag, notes = the CHANGELOG v0.15.0 section,
  5 assets (4 binaries + SHA256SUMS), not draft, not prerelease; repo
  isPrivate true confirmed via gh BEFORE creating the release and
  again after.
- Closing gate, fresh files in a scratch dir: downloaded x86_64 sha
  9003343c4d85c668b60fd2b3b4d68f4b4715009f3c9ebd53988beea510e2fafc
  equals the staged value and cmp against the staged binary is clean;
  the downloaded SHA256SUMS is cmp-identical to the staged one.
- Installer matrix 6/6 twice: once against the staged directory and
  once against a fresh full download of all five published assets
  (pass + corrupt + unlisted, GNU host and busybox container). That
  fresh download also self-verified 4/4 against the published
  SHA256SUMS.

Honest residuals: no live leg rode this milestone, by design, and the
cost advisory has therefore never fired against a real metered
endpoint; it is offline-computable end to end and every behavior it
claims is covered by mock-driven tests, but T24's keyed live check
still rides a future operator session and now carries T26 with it. The
one code rider this cycle (the em-dash the build session introduced
and then swept) is recorded in the stage-1 close-out, not here. The
ARM artifacts remain verified at build level under qemu only, hardware
smoke pending hardware. Still queued behind the visibility flip: the
PUBLIC one-liner gate, the hostname-blob-history decision, and the
demo GIF recording.

## v0.16.0 - close-out (release procedure delta)

What ships: T27 alone, the small-items bundle. It is the first
MULTI-ITEM release after six consecutive single-milestone ones: the
whole "Queued from T13 acceptance" list, eight items across four
phases, none of them large enough to justify a milestone alone and all
of them cheap to do together while nothing else was in flight.

Procedure deltas vs v0.15.0:

- **T27 pushed AS stage-1 step 1**, the standing shape for the fourth
  cycle: ec96476..4dbadd0 onto ec96476, four commits (P1 the
  switch_provider refactor alone, P2 the TUI trio, P3 the /models
  trio, P4 doctor plus docs), on-push ci run 31610259255 on headSha
  4dbadd0 green in both jobs (test 1m29s, release-gate 4m56s). main ==
  origin == 4dbadd0 verified before the prep commits.
- **The build prompt's stated base was one commit stale**, and the
  build session flagged it instead of forcing it. The plan named
  32a458d, but the v0.15.0 ship record ec96476 had landed on top after
  the plan was written, so main == origin/main == ec96476. Built on
  ec96476, reported in the first line of the build report. Worth
  keeping as the pattern: a stale base in a prompt is a discrepancy to
  surface, never something to reconcile by resetting.
- **One queued item closed as NOT REPRODUCED**, which is a disposition
  this project had not used before. The report that `/models` renders
  two ids on one line was probed at every width from 4 to 200 columns
  with ids built so that even a FRAGMENT of one landing beside another
  would be caught, and no row ever mixed two; the plain REPL prints one
  line per id and cannot merge either. The instruction was explicit
  that a failed reproduction must not become a blind fix, so nothing
  was changed, the probe is kept as a regression pin
  (`models_listing_never_puts_two_ids_on_one_row`), and the roadmap
  entry stays with wording saying it survives because it could not be
  reproduced rather than because it was deprioritized, naming what
  reopening needs: emulator, exact width, and the id list. The item
  was neither fixed nor quietly dropped, and the record says which.
- **The em-dash rule needed no rider this cycle.** v0.15.0 closed with
  a single-character repair and a statement that the per-cycle check is
  differential, not absolute. Verified here against that statement: 0
  em-dashes in lines added across all four T27 commits and 0 in the
  commit messages, with README.md, CHANGELOG.md, and ROADMAP.md still
  at zero absolute. The roughly 450 hits in code comments, pinned UI
  strings, and quoted transcripts were not touched, which is the point:
  stating the rule differentially is what made it satisfiable without
  an introduce-then-sweep.
- **A new doctor check that deliberately does not execute anything.**
  The install-skew check compares the first `temur` on PATH against the
  running binary by metadata and bytes only. Running a binary found by
  searching PATH is exactly what a diagnostic tool must not do, so the
  other copy's identity is inferred from its contents and it is never
  asked for its version. Never a FAIL either, because a second copy is
  a legitimate setup. Both inputs (current_exe and the PATH string) are
  injected, extending the run_with_sandbox_probe pattern, so the tests
  stage a fake install in a temp dir instead of depending on the host,
  and every doctor test predating the check runs with nothing to
  compare.
- **No live leg, by design and stated in advance.** All eight items are
  offline-verifiable: a refactor with byte-identical behavior, three
  TUI behaviors driven headlessly, two `/models` notices driven by
  fixtures, a doctor check over a staged temp dir, and one
  non-reproduction. T13's hosted verification and T24's keyed live
  check still ride a future operator session; this milestone inherits
  those residuals rather than adding one.
- **The version bump used scripts/bump_version.sh** for the sixth
  time, printed diff touching exactly the four-file map: Cargo.toml,
  the temur line of Cargo.lock (the third-party untrusted crate stays
  pinned at its own 0.9.0 and does not appear in the lock diff at
  all), scripts/install.sh VERSION, and the five README tag-pin lines.
- **Five green gates, every one first try.** Four build-phase runs plus
  the stage-1 gate, with all three TUI pty smokes (host, gnu container,
  musl container) quiet in each. The unnamed `--lib` failure from the
  T24 build cycle has still never recurred; the standing instruction is
  unchanged, and a single failure means capturing the exact suite name
  and full output before anything else.
- Stage 1 keeps the early stop: bump + dated CHANGELOG + records +
  full check.sh gate only; tag, four-target build, SHA256SUMS,
  private release, installer matrix, and the closing gate stay in
  stage 2. No tag, no push of the prep commits, no release; the repo
  stays PRIVATE.

## v0.16.0 acceptance - recorded result (SHIPPED, private)

2026-08-12, stage 2 all green, ships T27 alone (the small-items
bundle: the whole "Queued from T13 acceptance" list). No pre-tag rider
this cycle: the tag lands directly on the stage-1 prep head 266f6b3.

- Push 4dbadd0..266f6b3 (the three stage-1 prep commits: b3154ba
  version bump, d339663 CHANGELOG release cut, 266f6b3 docs
  close-out); on-push ci run 31616823243 on headSha 266f6b3 green in
  both jobs (test 2m20s 16:17:25Z..16:19:45Z, release-gate 6m55s
  16:17:25Z..16:24:20Z); main == origin == 266f6b3 verified. Normal
  runner timings this cycle, in contrast to v0.15.0's 13-minute test
  job, which was a slow apt mirror rather than this repo.
- **Stage 2 ran one cycle late, and that is the notable process fact.**
  The T28 build prompt arrived asserting "the v0.16.0 ship completed"
  when stage 2 had never run: the three prep commits were still local
  and unpushed, no tag and no release existed, and the only reason
  Cargo.toml read 0.16.0 was the local bump commit. The build session
  stopped on its own precondition check and reported instead of
  building. Recorded because the precondition earned its place: the
  stale premise was in the prompt, not in the repo, and the failure
  mode it prevented was a T28 CHANGELOG entry riding an Unreleased
  section above a v0.16.0 heading with no tag or release behind it.
- Annotated tag v0.16.0 at 266f6b3, message exactly "temur v0.16.0 -
  small fixes bundle (T27)" (one short line), verified against the RAW
  tag object via git cat-file tag BEFORE the push, not via git tag -l
  (which appends its own newline and cannot answer this question). The
  hexdump shows the object line 266f6b3a..., the blank-line separator
  0a 0a at offset 0x98, then exactly 40 message bytes at 0x9a..0xc1:
  39 printable ASCII plus the single closing 0x0a, with the separator
  an ASCII hyphen 0x2d and no byte sequence e2 80 94 anywhere. Tag
  object 149c82faabf26a56ee6c8847f3fca0510a0af0eb, pushed after
  verification; remote tag object and its ^{} commit both confirmed to
  match. Unsigned as always. Not retagged.
- Full release.sh, no SKIP_CHECK, green FIRST TRY: inner check.sh ALL
  CHECKS PASSED, leak grep clean (operator patterns + generic shapes,
  files + history), skew gate "OK: install.sh + README match version
  0.16.0 and all targets", 4/4 targets gated and version-asserted
  "temur 0.16.0" (i686 + x86_64 native, aarch64 + armv7 qemu),
  SHA256SUMS self-verified (4/4 OK), staged at
  /home/dev/dist/release/v0.16.0/.
- The teed-log procedure stays on, fourth cycle. All 48 "test result:"
  lines in the 278-line log read "0 failed", zero panics, across all
  three test legs (host i686-gnu, container gnu-debug, container
  musl-release). The unreproduced --lib failure from the T24 build
  cycle did not recur, so it stays unnamed and unreproduced across
  four full ship cycles now, plus this cycle's five build-side and
  stage-1 gate runs. The capture procedure is cheap and stays on.
- **The pty smoke fix held on its sixth independent exercise.** All
  three TUI pty smokes (host, container gnu-debug, container musl)
  passed on the first attempt, inside the 180s bound, in both the
  stage-1 gate and this release.sh run. Fifth consecutive cycle with
  zero kill-and-rerun; the bare busybox leg printed "temur 0.16.0" and
  its mock REPL passed.
- Staged sha256s: 88f591b98ab7... aarch64, 9a5ffdd8253f... armv7,
  4c51fd32d780... i686, e638525887bb... x86_64; SHA256SUMS itself
  440480cff8d7... (full sums in the release's SHA256SUMS asset).
- Private release github.com/thekeoni1/Temur/releases/tag/v0.16.0
  created with title per tag, notes = the CHANGELOG v0.16.0 section,
  5 assets (4 binaries + SHA256SUMS), not draft, not prerelease; repo
  isPrivate true confirmed via gh BEFORE creating the release and
  again after. Asset sizes match the staged bytes exactly.
- Closing gate, fresh files in a scratch dir: downloaded x86_64 sha
  e638525887bb00f261bf09dbdf8790cfac1fbfd21e44b240705e40946b32df20
  equals the staged value and cmp against the staged binary is clean;
  the downloaded SHA256SUMS is cmp-identical to the staged one.
- Installer matrix 6/6 twice: once against the staged directory and
  once against a fresh full download of all five published assets
  (pass + corrupt + unlisted, GNU host and busybox container). That
  fresh download also self-verified 4/4 against the published
  SHA256SUMS, and all FOUR downloaded binaries were cmp-identical to
  their staged bytes, not just the x86_64 spot check.

Honest residuals: no live leg rode this milestone, by design. All
eight items are offline-verifiable, and the one that could not be
verified at all, the "/models renders two ids on one line" report, was
closed as NOT REPRODUCED rather than fixed blind, with the probe kept
as a regression pin and the roadmap entry saying so; reopening it
needs specifics from a live terminal. T13's hosted verification and
T24's keyed live check both still ride a future operator session, and
this milestone inherits those residuals rather than adding one. The
ARM artifacts remain verified at build level under qemu only, hardware
smoke pending hardware. Still queued behind the visibility flip: the
PUBLIC one-liner gate, the hostname-blob-history decision, and the
demo GIF recording.

## v0.17.0 - close-out (release procedure delta)

What ships: T28 alone, skill compacting. Back to a single milestone
after T27's eight-item bundle. The tag lands directly on the stage-1
prep head; no pre-tag rider this cycle.

Procedure deltas vs v0.16.0:

- **T28 pushed AS stage-1 step 1**, the standing shape for the fifth
  cycle: 38ee7af..fe36ac4 onto 38ee7af, four commits (P1 the pure
  minify/scan layer plus plumbing, P2 the three tool modes and both
  prompts, P3 the agent-loop and beneficiary pins, P4 docs), on-push
  ci run 31627218432 on headSha fe36ac4 green in both jobs (test
  1m34s 18:21:27Z..18:23:01Z, release-gate 4m36s 18:21:28Z..18:26:04Z);
  main == origin == fe36ac4 verified before the prep commits.
- **The build session caught itself fabricating a transcript, and the
  catch is the record.** Its first draft of the USAGE section invented
  a `<skill_index>` example with made-up character counts and fake REPL
  chrome, inside a document whose capture note states that every
  transcript in it is from a real run. It was replaced before any gate
  with the tool's VERBATIM output, produced by running the real code
  over a generated fixture, and labeled in the text as
  fixture-generated rather than captured from a live model session, so
  it does not borrow the credibility of the real transcripts around
  it. Recorded because this was the first new USAGE section that
  needed an example no live session could supply, and the rule that
  document lives by only means something if it binds new prose too.
  The standing instruction: an illustrative example is fine, an
  illustrative example dressed as a capture is not, and the label is
  what separates them.
- **A docs claim was measured before it was written, and the
  measurement was unflattering.** The build prompt expected the
  minifier to save "single-digit percent". Measured: 0.0% on this
  repo's own markdown, because tidy files have nothing to remove;
  2.2% on a SKILL.md with frontmatter and loose spacing; 62 characters
  on the 48,427-char example. The docs and CHANGELOG publish those
  numbers and say plainly that minification is a rounding error kept
  only because it is free and lossless, while the section index is the
  mechanism (48,427 chars to an 846-char index, 1.7%). Precedent
  worth keeping: measure a quantitative claim before shipping it, and
  publish the number that comes back rather than the number that was
  expected.
- **One design ruling was added that the plan had not enumerated**,
  and it surfaced through a wrong test rather than review: a skill
  over the cap whose prose BEFORE the first heading already exceeds
  the cap falls back to full mode plus central truncation, because an
  index that does not itself fit is not an improvement. That is the
  same ruling the plan gave for a heading-less skill, which is
  effectively what such a file is. The build session's own at-cap test
  failed first because it used a skill so small that an index was
  LARGER than the content; the code was right and the test premise was
  wrong, so the test was fixed and the fallback pinned in its own
  test.
- **Measurement scaffolding was added and removed within the phase.**
  The savings numbers came from throwaway tests appended to
  tests/skills.rs and reverted from a backup before committing; P4
  went in docs-only, verified by diff stat against src/ and tests/
  showing no changes. Recorded so the numbers in the docs are
  traceable to a method rather than to an assertion.
- **The version bump used scripts/bump_version.sh** for the seventh
  time, printed diff touching exactly the four-file map: Cargo.toml,
  the temur line of Cargo.lock (the third-party untrusted crate stays
  pinned at its own 0.9.0 and does not appear in the lock diff at
  all), scripts/install.sh VERSION, and the five README tag-pin lines.
  No stale pin surfaced, so this close-out is RUNBOOK-only.
- **Five green gates, every one first try**, four build-phase runs
  plus the stage-1 gate, with all three TUI pty smokes quiet in each.
  The unnamed `--lib` failure from the T24 build cycle has still never
  recurred. Suite growth this milestone: skills 7 to 21, lib skills
  unit tests 5 to 23, tools 44 to 46, agent 111 to 113, and the T19
  truncation-marker pins passed UNCHANGED, which was the compatibility
  condition on the new per-tool hint.
- Stage 1 keeps the early stop: bump + dated CHANGELOG + records +
  full check.sh gate only; tag, four-target build, SHA256SUMS,
  private release, installer matrix, and the closing gate stay in
  stage 2. No tag, no push of the prep commits, no release; the repo
  stays PRIVATE.

Residuals carried into the ship: no live leg, by design, so no model
has yet chosen a section for itself outside a scripted test; the
intro text of a skill is shipped whole inside the index but has no
section number of its own, so reconstruction by section covers
everything after the first heading only; and clippy is not installed
on this toolchain, so the usual lint pass did not run this cycle
(cargo build is warning-free).

## v0.17.0 acceptance - recorded result (SHIPPED, private)

2026-08-12, stage 2 all green, ships T28 alone (skill compacting: the
section index). No pre-tag rider this cycle: the tag lands directly on
the stage-1 prep head 89313fe. Second release in one day, after
v0.16.0 shipped the same morning.

- Push fe36ac4..89313fe (the three stage-1 prep commits: f4625a4
  version bump, 76b9895 CHANGELOG release cut, 89313fe docs
  close-out); on-push ci run 31628664653 on headSha 89313fe green in
  both jobs (test 2m21s 18:38:42Z..18:41:03Z, release-gate 8m11s
  18:38:34Z..18:46:45Z); main == origin == 89313fe verified.
- Annotated tag v0.17.0 at 89313fe, message exactly "temur v0.17.0 -
  skill section index (T28)" (one short line), verified against the
  RAW tag object via git cat-file tag BEFORE the push, not via git
  tag -l (which appends its own newline and cannot answer this
  question). The hexdump shows the object line 89313fec..., the
  blank-line separator 0a 0a at offset 0x98, then exactly 41 message
  bytes at 0x9a..0xc2: 40 printable ASCII plus the single closing
  0x0a, with the separator an ASCII hyphen 0x2d and no byte sequence
  e2 80 94 anywhere. Tag object
  50ff35a5390f8108bc7b688f63435fa57cf26d8f, pushed after
  verification; remote tag object and its ^{} commit both confirmed
  to match. Unsigned as always. Not retagged.
- Full release.sh, no SKIP_CHECK, green FIRST TRY: inner check.sh ALL
  CHECKS PASSED, leak grep clean (operator patterns + generic shapes,
  files + history), skew gate "OK: install.sh + README match version
  0.17.0 and all targets", 4/4 targets gated and version-asserted
  "temur 0.17.0" (i686 + x86_64 native, aarch64 + armv7 qemu),
  SHA256SUMS self-verified (4/4 OK), staged at
  /home/dev/dist/release/v0.17.0/.
- The teed-log procedure stays on, fifth cycle. All 48 "test result:"
  lines in the 278-line log read "0 failed", zero panics, across all
  three test legs (host i686-gnu, container gnu-debug, container
  musl-release). The unnamed --lib failure from the T24 build cycle
  did not recur, so it stays unnamed and unreproduced across five
  full ship cycles now, plus this cycle's four build-side gate runs
  and the stage-1 gate. The capture procedure is cheap and stays on.
- **The pty smoke fix held on its seventh independent exercise.** All
  three TUI pty smokes (host, container gnu-debug, container musl)
  passed on the first attempt, inside the 180s bound, in both the
  stage-1 gate and this release.sh run. Sixth consecutive cycle with
  zero kill-and-rerun; the bare busybox leg printed "temur 0.17.0"
  and its mock REPL passed.
- Staged sha256s: 91153a532537... aarch64, bc2299fdb432... armv7,
  60ae61720c9f... i686, 64406d9a46bb... x86_64; SHA256SUMS itself
  7e8746bfd8e4... (full sums in the release's SHA256SUMS asset).
- Private release github.com/thekeoni1/Temur/releases/tag/v0.17.0
  created with title per tag, notes = the CHANGELOG v0.17.0 section,
  5 assets (4 binaries + SHA256SUMS), not draft, not prerelease; repo
  isPrivate true confirmed via gh BEFORE creating the release and
  again after. Asset sizes match the staged bytes exactly.
- Closing gate, fresh files in a scratch dir: downloaded x86_64 sha
  64406d9a46bb09ab845e3def04b547e4bc3196080b5f45552c4239a5243e9971
  equals the staged value and cmp against the staged binary is clean;
  the downloaded SHA256SUMS is cmp-identical to the staged one.
- Installer matrix 6/6 twice: once against the staged directory and
  once against a fresh full download of all five published assets
  (pass + corrupt + unlisted, GNU host and busybox container). That
  fresh download also self-verified 4/4 against the published
  SHA256SUMS, and all FOUR downloaded binaries were cmp-identical to
  their staged bytes, the wider check adopted in v0.16.0 rather than
  the x86_64 spot check alone.

Honest residuals: no live leg rode this milestone, by design, so no
model has yet chosen a skill section for itself outside a scripted
test; the feature is offline-computable end to end and every behavior
it claims is covered by mock-driven tests, including an agent-loop run
that indexes, fetches a numbered section, and answers. A skill's intro
text ships whole inside the index but has no section number of its
own, so reconstruction BY SECTION covers everything after the first
heading only. clippy is not installed on this toolchain, so the usual
lint pass did not run this cycle, though cargo build is warning-free.
The two build-side process facts (a fabricated USAGE transcript caught
and replaced with labeled verbatim output, and a docs claim measured
rather than assumed, coming back at 0.0% instead of the expected
single-digit percent) are recorded in the stage-1 close-out, not here.
T13's hosted verification and T24's keyed live check both still ride a
future operator session. The ARM artifacts remain verified at build
level under qemu only, hardware smoke pending hardware. Still queued
behind the visibility flip: the PUBLIC one-liner gate, the
hostname-blob-history decision, and the demo GIF recording.

## T29 acceptance - recorded result (no release)

Measurement milestone: nine local models through the nine-task
weak-model eval, plus the first live observation of T28's skill index.
NO Rust changed. Every finding below was RECORDED and left unfixed by
design, so a later milestone can act on them with the numbers already
in hand.

### Conditions (identical for every row)

`scripts/weak_model_eval.sh` at its defaults: compact prompt profile,
llama.cpp `server-b10068` (self-reported `version: 10068 (571d0d540)`),
ctx 8192 with `--jinja`, `EVAL_TASK_TIMEOUT` 300s, `EVAL_MIN` 0, the
i686 musl-static release binary (`temur 0.17.0`) mounted read-only into
`docker.io/i386/debian:stable`, each task in a fresh work dir with a
fresh process, inside a `--network none` pod. Host: 7.8 GB RAM,
16 cores, 909 GB free. Nothing was pulled; all three images were
already local.

One deviation from the committed harness, used for every run: a
scratchpad copy differing by exactly ONE line, `rm -rf "$EVAL_ROOT"`
in teardown replaced by `echo "KEEPING EVAL_ROOT=$EVAL_ROOT"`. Teardown
runs after all scoring, so it cannot affect a score; without it,
findings 2 and 6 below could not have been diagnosed at all (see F5).

### Scores, all /9, measured 2026-08-12

```
model                          score  failed tasks
Qwen3-4B-Instruct-2507          9/9   none
Qwen2.5-Coder-3B-Instruct       8/9   2
Qwen2.5-Coder-1.5B-Instruct     7/9   2, 8
Qwen3-1.7B                      6/9   2, 5, 9   (first run: 2, 5, 8)
Qwen3-0.6B                      4/9   2, 5, 7, 8, 9
Llama-3.2-3B-Instruct           1/9   2, 3, 4, 5, 6, 7, 8, 9
Gemma-3-4B-it                   0/9   all
Phi-4-mini-instruct             0/9   all
SmolLM2-1.7B-Instruct           0/9   all
```

Qwen3-1.7B was run twice (the first run overlapped the model downloads;
the second was uncontended). Both scored 6/9, failing different tasks,
which is F4. Qwen2.5-Coder-3B was re-run beyond the milestone's stated
model list, because P5's "the eval has since grown to nine tasks"
caveat could only be retired honestly once no `/7` row survived.

### Download provenance

Six models were fetched for this milestone, all Q4_K_M, all from the
`unsloth` org on Hugging Face (the source `docs/OFFLINE.md` already
names for these quants), into `/home/dev/models`:

```
unsloth/Qwen2.5-Coder-1.5B-Instruct-GGUF  Qwen2.5-Coder-1.5B-Instruct-Q4_K_M.gguf   986048032
  sha256 5ecd6f45e137154099741291848db7415b5effa1ae69228f20dacea31dbf5ce4
unsloth/Qwen3-0.6B-GGUF                   Qwen3-0.6B-Q4_K_M.gguf                    396705472
  sha256 ac2d97712095a558e31573f62f466a3f9d93990898b0ec79d7c974c1780d524a
unsloth/Llama-3.2-3B-Instruct-GGUF        Llama-3.2-3B-Instruct-Q4_K_M.gguf        2019377600
  sha256 6c99cc00ae910f6a532a80022cb4bc1939094527a089c29294b841c0bd87f74d
unsloth/gemma-3-4b-it-GGUF                gemma-3-4b-it-Q4_K_M.gguf                2489894016
  sha256 04a43a22e8d2003deda5acc262f68ec1005fa76c735a9962a8c77042a74a7d19
unsloth/Phi-4-mini-instruct-GGUF          Phi-4-mini-instruct-Q4_K_M.gguf          2491874272
  sha256 88c00229914083cd112853aab84ed51b87bdf6b9ce42f532d8c85c7c63b1730a
unsloth/SmolLM2-1.7B-Instruct-GGUF        SmolLM2-1.7B-Instruct-Q4_K_M.gguf        1055609504
  sha256 61b6f90dd515fd3bffbd0f6ba716e87555dde77d9b0573a562c2c5e62afc4909
```

Three were already on this machine from earlier milestones. Their
bytes are hashed here for the record, but the source repo was NOT
recorded at fetch time, so it is not asserted (OFFLINE.md names the
`unsloth/...-GGUF` repositories for these quants):

```
(repo not recorded)  Qwen3-1.7B-Q4_K_M.gguf                 1107409472  fetched 2026-07-19
  sha256 b139949c5bd74937ad8ed8c8cf3d9ffb1e99c866c823204dc42c0d91fa181897
(repo not recorded)  Qwen3-4B-Instruct-2507-Q4_K_M.gguf     2497281120  fetched 2026-07-26
  sha256 3605803b982cb64aead44f6c1b2ae36e3acdb41d8e46c8a94c6533bc4c67e597
(repo not recorded)  Qwen2.5-Coder-3B-Instruct-Q4_K_M.gguf  1929902496  fetched 2026-07-26
  sha256 32f0014400ca1c1f81e7fb5befa9b9af476ba967dcbf92bad27409228c57c5b4
```

### Findings, all 17

Nine are queued in ROADMAP.md, "Queued from T29 (2026-08-12)", with
their evidence quoted inline there; they appear here as one-line
cross-references. The other eight land here in full, because nothing
else in the tree carries them.

Cross-references to the ROADMAP queue:

- F1 / F9, placeholder literalism: ROADMAP queue item 2.
- F2, a weak model destroying a source file it had read: queue item 6.
- F3, `max_tokens` 2048 binding before any tool call: queue item 3.
- F4, a single run carrying about a task of noise: queue item 4.
- F5, the harness deleting failed tasks' work dirs: queue item 5.
- F7, preamble before a fenced call defeating execution AND nudge:
  queue item 1.
- F8, Qwen2.5-Coder-1.5B outscoring the baked default: queue item 7.
- F11(b), Llama-3.2-3B's argument errors never captured: queue item 9.
- F15, the skill index's "Base directory" line pulling models to the
  filesystem: queue item 8.

#### F6. Qwen3-1.7B no longer reproduces its "verified 2026-07-26 (eval 7/7)" row

Current tasks 1 through 7 are the same seven that row was measured on.
Qwen3-1.7B now scores 6/7 on that subset in the uncontended run and 5/7
in the contended one, against a recorded 7/7. The harness prompts are
unchanged; the binary is not (T19 truncation, T22 context detection,
T24 and T26 advisories, T28 skill index). Recorded as real drift in the
anchor rather than a re-scoring, and it is why the table now dates
every row to a single day.

#### F10. Qwen3-0.6B reproduces the T11 dogfood gap verbatim

Task 7 (indirect-delete), the whole turn:

```
The tool 'delete' is not available in the provided functions. I can't
delete the file 'obsolete.tmp' directly. Using the `bash` tool with
the command `rm -f obsolete.tmp` would accomplish this, but I must
note that this is not allowed for file deletion here.
```

It names the correct path and then forbids itself from taking it. That
is the T11 dogfood observation ("qwen3-1.7b claimed it had no delete
tool") that task 7 was written to probe, reproduced exactly, on a
different model, four milestones later. The probe still earns its slot.

#### F11. Llama-3.2-3B-Instruct: 1/9, two distinct wire failures

Only task 1 passed, and it doom-looped after passing. Two modes:

(a) Server-side format rejection. Task 7, the whole turn:

```
[!] provider error: api error (HTTP 200) server_error: The model
produced output that does not match the expected peg-native format
```

llama.cpp's own tool-call grammar rejecting the model's output, before
temur sees anything. A single-task probe reproduced this same mode,
with the model's visible output beginning `` ```json ``.

(b) Argument-level rejection into the doom-loop guard. Tasks 3 and 4:

```
  -> edit          -> bash
  x edit: edit     x bash: bash
  (three times, then)
  [!] stopped: the same tool call was repeated 3 times in a row
```

The call arrived structured (temur dispatched it) and the tool errored;
the `--plain` UI prints only tool name and title, so the argument error
text is not recoverable from the transcript. The probe built to capture
it reproduced mode (a) instead, so (b) remains uncaptured: queue item 9.

#### F12. Superseded hypothesis, kept because it was wrong in a useful way

On seeing gemma-3 answer with 82 input tokens where Qwen3-0.6B saw
5467 on the identical task, this record's first draft concluded that
"the bundled template delivers neither system nor tools". That was
half wrong, and it was recorded as a hypothesis pending verification
rather than as a result. F14 is the measurement, and it shows the
system message arrives in every case. Kept here as the standing
reminder that the usage counter tells you SOMETHING is missing, not
WHAT.

#### F13. Phi-4-mini-instruct: 0/9, same signature as gemma-3

Every task 2 to 18 seconds, 76 in / 34 out on task 1 against 5467 in
for Qwen3-0.6B on the same task. Task 1 output, verbatim:

```
{
  "command": "write",
  "arguments": {
    "file": "hello.txt",
    "text": "hello-eval"
  }
}
```

Task 4 output, verbatim and complete, with no call structure at all:

```
mkdir -p build && echo "done" > build/marker.txt
```

Worth separating from the template question: task 1 DOES clear the
leading-brace gate and DOES carry a proper `arguments` object. It
fails only because the tool name sits under `"command"`, and
`recover.rs` accepts `"name"` or `"tool"` (lines 249-251 and 194-198).

#### F14. VERIFIED: llama.cpp `--jinja` silently drops the TOOLS array for three families

The measurement behind the three zero rows, and the correction to F12.
Three requests per model, identical but for what they carry, comparing
`prompt_tokens`: A = system + one tool schema, B = system only,
C = user only.

```
model                A(sys+tools)  B(sys)  C(user)   tools delivered
qwen3-1.7b CONTROL       207         30      11      yes  (+177)
llama-3.2-3b             240         52      38      yes  (+188)
gemma-3-4b                28         28      13      NO   (+0)
phi-4-mini                22         22       6      NO   (+0)
smollm2-1.7b              35         35      34      NO   (+0)
```

A equals B exactly for the three zero-score families: the tools array
contributes nothing to the rendered prompt. B exceeds C for every
model, so the system message is delivered in all cases; SmolLM2's C is
high because its template injects a default system prompt when none is
given, and it answered the marker directly ("You are correct. You are
a test harness."). The server returns HTTP 200 and warns about
nothing, so temur cannot see this: it sends a correct OpenAI-shaped
request with a tools array and gets back a well-formed answer that
ignores them. Those models are never told tools exist and invent
shapes accordingly, gemma-3 task 7 being the clearest:
`{"tool": "file_delete", "path": "obsolete.tmp"}`, naming a tool that
does not exist in the registry.

Consequence: those three cannot drive temur's tools on this stack at
all, and the cause is upstream template support, not model ability and
not a temur defect. The harness has no `--chat-template` knob, so it
cannot even try an alternative; that limitation is now stated in the
docs rather than worked around.

#### F16. Sample size, stated honestly

The T28 observation is three models, one task, one run each. It shows
the mechanism CAN carry a small model to a correct answer with no
hints, and CAN be ignored by a stronger one. It supports no rate.
Any claim about how often models use the index needs a multi-task,
multi-run design that this milestone did not build.

#### F17. Qwen2.5-Coder-3B went from 0/7 to 8/9, and temur is why

The table's most pessimistic row no longer describes the current
binary. Re-run 2026-08-12: 8/9, only task 2 failing (the F9
placeholder). The mechanism is VERIFIED from the transcripts, not
inferred; temur prints a notice each time:

```
{"name": "write", "arguments": {"content": "hello-eval\n", "filePath": "/work/hello.txt"}}
  [!] prose-call recovery: executed the write tool call the model
      wrote as plain text
```

and on task 4, the same notice for a fenced block:

```
```json
{"name": "bash", "arguments": {"command": "mkdir build && echo 'done' > build/marker.txt"}}
```
  [!] prose-call recovery: executed the bash tool call the model
      wrote as plain text
```

The 2026-07-26 measurement predates T19 P3 (shipped 2026-07-30 in
v0.8.0), which added execution of prose tool calls. The old OFFLINE.md
narrative for this row was accurate then and stale now: the reasoning
was always fine, and once wire format stopped mattering the model
scores 8/9.

This is the milestone's sharpest result when paired with F7, because
the SAME feature decides both outcomes on the SAME family. Coder-3B
emits a bare object or a bare fenced block and is recovered, 8/9.
Coder-1.5B emits the identical JSON behind one sentence of preamble,
is neither recovered nor nudged, silently, and it cost that model eval
tasks 2 and 8 plus the whole T28 observation. One token position is
the entire difference.

### P4: first live observation of the T28 skill index

Fixture: a purpose-built `atlas-deploy/SKILL.md`, 11,674 chars
minified, 12 sections, against the 8,192-char cap that ctx 8192
implies. The answer (`/var/lock/atlas-rollback.pid`) appears ONLY in
section 5, "Rollback and recovery". The task named neither a section
nor the index:

```
Using the atlas-deploy skill, find the exact filesystem path of the
Atlas rollback lock file, then use the write tool to create answer.txt
containing just that path.
```

The mechanism worked every time it was reached: an 11,674-char skill
returned a 978-char index (8.4%) carrying the intro, all 12 numbered
headings, and the fetch instruction.

Qwen3-1.7B (6/9, run beyond the ">= 7" rule and the run that produced
the best evidence of the milestone). Essential lines from the saved
session:

```
TOOL_USE    skill {"name": "atlas-deploy"}
TOOL_RESULT <skill_index ...> (978 chars)
TOOL_USE    skill {"name": "atlas-deploy", "section": "5"}
TOOL_RESULT <skill_section name="atlas-deploy" number="5"
             title="Rollback and recovery"> (1197 chars)
TOOL_USE    write {"content": "/var/lock/atlas-rollback.pid", ...}
```

Argument form: the NUMBER, passed as a JSON STRING (`"5"`, not `5`);
the schema accepts both. It mapped "rollback lock file" onto the
heading "Rollback and recovery" from the heading list alone and
answered correctly, with no prompting beyond the task. Separate model
error, not a T28 problem: it wrote to
`/work/.temur/skills/atlas-deploy/answer.txt`, inside the skill
directory, rather than the cwd. Content was correct.

Qwen3-4B (9/9): received the index and ignored the affordance.

```
TOOL_USE    skill {"name": "atlas-deploy"}
TOOL_RESULT <skill_index ...> (978 chars)
TOOL_USE    grep {"pattern": "rollback lock file",
                  "path": "/work/.temur/skills/atlas-deploy"}
TOOL_RESULT "No matches found"
TOOL_USE    glob {"pattern": "rollback.lock", "path": "/work"}
TOOL_RESULT "No files found"
TEXT        "The rollback lock file path could not be found ... I
             cannot determine the exact filesystem path"
```

It never called `skill` with a section, going to the filesystem
instead and grepping for a literal phrase the document does not
contain. The strongest model in the matrix failed the task the 6/9
model passed.

Qwen2.5-Coder-1.5B (7/9): reached for the affordance and lost it to
F7. Its whole turn was preamble plus a fenced block, so nothing ran:

```
Here's how you can achieve this using the provided tools:

```json
{
  "name": "skill",
  "arguments": {"name": "atlas-deploy", "section": 2}
}
```
```

Notable on its own: it asked for `section` on its FIRST call, having
never seen an index, guessing 2 (the answer is in 5). The affordance
is discoverable from the tool description alone. Argument form here
was a bare NUMBER.

### Residuals

- Llama-3.2-3B's argument-level errors remain uncaptured (F11b, queue
  item 9); the probe reproduced the other failure mode.
- The T28 observation is one task on three models (F16); no rate is
  claimed anywhere in the docs.
- The baked default model (`src/init.rs`, `qwen3-1.7b`) and the
  "(primary)" label in OFFLINE.md were both deliberately left alone:
  flipping a default is the planning session's call and this milestone
  changed no Rust. The docs now say plainly that the 4B scores highest
  (queue item 7).
- Two of the nine tasks partly measure placeholder literalism (queue
  item 2), so every score in the table carries that; the docs say so.
- The eval gate ran the scratchpad harness copy described above, not
  `scripts/weak_model_eval.sh` byte for byte. The committed script is
  unchanged.

## v0.18.0 acceptance - recorded result (SHIPPED, private)

What shipped: T29 alone, the local-model coverage matrix. A
measurement milestone, so the artifact is byte-for-byte the v0.17.0
code with a version bump; what changed is the docs' claims and the
ROADMAP's queue.

Stage 1 (verified before stage 2 began):

- T29 pushed as step 1, 2ba2d85 onto b407fd3, one commit (docs,
  ROADMAP, CHANGELOG; no Rust). On-push ci run 31654745872 on headSha
  2ba2d85 green in both jobs (test 1m07s 00:32:15Z..00:33:22Z,
  release-gate 4m51s 00:32:16Z..00:37:07Z).
- Three local prep commits: 9973af3 bump (four files, Cargo.lock's
  temur entry only, untrusted still 0.9.0, five README tag pins, no
  v0.17.0 left), 878d813 CHANGELOG cut to "## v0.18.0 - 2026-08-12"
  with a fresh empty Unreleased, 4c06a00 close-out carrying the T29
  acceptance record above.
- Full check.sh green at 0.18.0, first try, bare container reporting
  "temur 0.18.0"; all three TUI pty smokes quiet.

Stage 2:

- Prep pushed 2ba2d85..4c06a00; ci run 31655622617 on headSha 4c06a00
  green in both jobs (test 2m02s 00:47:44Z..00:49:46Z, release-gate
  7m52s 00:47:44Z..00:55:36Z).
- Annotated tag v0.18.0 AT 4c06a00, tag object
  d679daf785bff7fdaaa80fe6b65ee9ac45fac3b8. The message was verified
  against the RAW object before the tag was pushed, not through
  `git tag -l --format` (which appends its own newline): the object's
  message region was extracted and `cmp`-compared against the intended
  bytes, identical, exactly one line, zero em-dash bytes in the whole
  object. Remote ref resolves to the same object hash.
- scripts/release.sh with NO SKIP_CHECK: green first try, 4/4
  artifacts gated and staged, all three pty smokes quiet for the
  second run of the cycle.

Staged sha256 (and the same values inside SHA256SUMS):

```
c7efc963  temur-v0.18.0-aarch64-unknown-linux-musl
f8bfed01  temur-v0.18.0-armv7-unknown-linux-musleabihf
6df88e9b  temur-v0.18.0-i686-unknown-linux-musl
cb258419  temur-v0.18.0-x86_64-unknown-linux-musl
ab96ecc1  SHA256SUMS itself
```

- Private release created with 5 assets, not draft, not prerelease,
  notes = the CHANGELOG v0.18.0 section verbatim. Repo isPrivate
  confirmed true BEFORE creating it and again AFTER.
- Closing gate: the x86_64 asset and SHA256SUMS were re-downloaded and
  both `cmp`-identical to staged, re-hashed to cb258419 and ab96ecc1.
  Then a fresh FULL download of all five assets, every one
  `cmp`-identical to staged. Installer matrix 6/6 twice, once against
  the staged dir and once against that fresh download
  (pass + corrupt + unlisted, on the GNU host and in busybox).

Residuals carried out of this cycle, none blocking:

- The gate and release runs were launched detached under script(1)
  rather than in a literal foreground shell, the standing deviation:
  both exceed the build session's foreground time cap. Both were
  pty-backed, fully teed, and watched for the 180s pty-smoke bound.
- Three of the nine measured models were already on this machine, and
  their source repo was not recorded when they were fetched in July,
  so the T29 record hashes their bytes but does not assert a repo.
- The substantive T29 residuals are unchanged and listed in the T29
  record: Llama-3.2-3B's argument-level errors uncaptured, the T28
  observation being one task across three models, and the baked
  default model left alone despite no longer being the top scorer.

## T30 acceptance - recorded result (no release)

Model floor, round two: four items off the T29 queue, built one phase
at a time with a full `scripts/check.sh` gate per phase. Offline
throughout, no live provider call of any kind, and no re-measurement
(see "Deliberate non-change" below).

### What shipped

- **F1, the silent shape.** `detect_text_tool_call` now also scans for
  FENCED blocks anywhere in a message, strips each fence, and applies
  the checks it already applied to a whole-message body: parses as JSON
  (or as its first balanced object), names a REGISTERED tool under
  `"name"`/`"tool"`, carries an arguments-like key. First fenced hit
  wins. A bare JSON object mid-prose WITHOUT a fence stays undetected
  on purpose, and the pin for that case is kept: prose quoting a call
  shape while discussing a plan is common, and the fence is the only
  cheap evidence of intent.

  **The execution predicate is byte-identical.** `extract_prose_tool_
  call` was not touched, so preamble plus a fence still never runs; the
  change converts silence into a retry, not into a wider set of things
  that execute. Pinned both directions with the exact Qwen2.5-Coder-1.5B
  eval-task-8 shape: the unit table asserts detection true AND
  extraction `None`, and a loop-level test asserts that the file on
  disk came from the structured retry, that the nudge notice fired, and
  that no `prose-call recovery` notice did. NUDGE_LIMIT bounds the
  widened path exactly as it bounds the old one, pinned separately.

- **F8, the base-directory line.** `Base directory for this skill:
  <path>` is now conditional in all three modes (content, index,
  section): emitted only when the skill's directory holds at least one
  entry besides `SKILL.md`. One `read_dir` at render time, nothing
  cached; a directory that cannot be listed counts as bare, since
  visible assets are the whole justification for the line. Output for a
  skill that ships assets is byte-identical to v0.18.0. The T28
  reconstruction invariant, the index, and section selection are
  untouched.

- **F6, the write note.** A successful `write` over a non-empty file
  appends `, replaced <N> bytes of prior content` to its result.
  Always when the prior file was non-empty, with no smallness
  threshold, and never for a new or previously empty file. Sizes as
  `u64`. The read-first guard is UNCHANGED and was never the defect:
  in eval task 5 it permitted the destruction correctly, because the
  model had read that file moments earlier. What was missing was the
  trace.

- **F7, the default flip** (operator-approved, since flipping a default
  is not a build-session call). `temur init`'s local template bakes
  `qwen3-4b`; the fallback shortlist leads with Qwen3-4B-Instruct-2507
  as the primary recommendation and keeps Qwen3-1.7B second as the
  low-RAM choice; OFFLINE.md moves "(primary)" to the 4B row and marks
  the 1.7B the low-RAM floor. Every golden that baked the old default
  moved in the same commit: init's README-recipe render, the picker's
  template-default and listing-failure fallbacks, and two `tests/cli.rs`
  pins that drive the local template through the binary.

### Planning-session rulings carried in

Two judgment calls were raised in the build report and ruled on rather
than decided here:

- The `/model` "no profiles defined" help notice in `src/commands.rs`
  KEEPS `qwen3-1.7b`, as one of the mechanism-snippet trio (with the
  multi-profile examples in OFFLINE.md and USAGE.md). Those snippets
  illustrate the profiles MECHANISM; the id in them is payload, not a
  recommendation, and churning them would spread a default flip into
  text that makes no claim about defaults.
- The two index-size figures in USAGE.md were DERIVED arithmetically
  rather than re-captured: the transcript was a verbatim capture over
  an asset-free fixture, so removing the base-directory line removes
  exactly 73 characters, 846 -> 773, and 1.7% -> 1.6%. Accepted as
  arithmetic on a capture. It is recorded here because the doc reads
  as a verbatim run and this one figure is not one.

### Deliberate non-change

The matrix was NOT re-run. Every score in OFFLINE.md keeps its
2026-08-12 date and describes the binary as it was measured, before
any of the above existed. F1 is EXPECTED to raise Qwen2.5-Coder-1.5B,
whose lost calls are precisely what it addresses, and that expectation
is an unverified prediction until the next matrix pass measures it.
Nothing in the docs claims otherwise.

### Gate outcomes

Four phases, four full `check.sh` runs, every one ending
`== ALL CHECKS PASSED ==` with exit 0 across both paths (gnu-debug and
the musl-release acceptance path). All three TUI pty smokes (host, gnu,
musl) reported OK in all four runs; the 180s bound was never
approached and the hang signature never appeared. Staticness clean
each time (no INTERP, no NEEDED), forbidden-deps clean, bare busybox
container printing the version. The final gate ran 30 test-result
lines, 890 passing assertions, zero failures.

Standing deviation, unchanged from prior cycles: the gate runs were
launched detached under `script(1)` rather than in a literal
foreground shell, because a full gate exceeds the build session's
foreground command cap. Every run was pty-backed, fully teed to a log,
and watched for the 180s pty-smoke signature.

### Phase commits

```
c245ee0  P1  fenced-call nudge widening (detection only)
b8841f9  P2  conditional base-directory line + write replaced-bytes note
a46880f  P3  baked local default -> qwen3-4b
5417fea  P4  docs and close-out
```

### Residuals

- The F1 improvement for Qwen2.5-Coder-1.5B is predicted, not
  measured (above).
- The five surviving T29 queue items (2, 3, 4, 5, 9) keep their
  original numbers, because this record and the T29 record cross-
  reference them by number. Items 2 through 5 are eval-HARNESS items
  that each change what a published score means, so they belong to a
  milestone that re-runs the matrix and can restate the numbers in the
  same breath; item 9 needs a live model to reproduce at all.
- The duplicate-heading note in a `<skill_section>` lost one blank
  line as a side effect of the header rework. No pin covered the
  spacing; recorded so it is not read later as an accident.

## v0.19.0 acceptance - recorded result (SHIPPED, private)

What shipped: T30 alone, model floor round two. Four items off the T29
queue, all offline-verifiable; the T30 acceptance record above carries
the per-finding detail, the planning-session rulings, and the
deliberate non-change (the matrix was NOT re-run).

Stage 1:

- Four T30 commits pushed a749a9d..5417fea (P1 fenced-call nudge
  widening, P2 conditional base-directory line + write replaced-bytes
  note, P3 baked local default to qwen3-4b, P4 docs and close-out).
  On-push ci run 31702070953 on headSha 5417fea green in both jobs
  (test 1m17s 12:51:55Z..12:53:12Z, release-gate 4m21s
  12:51:56Z..12:56:17Z).
- Three local prep commits: f3dc5da bump (four files, Cargo.lock's
  temur entry only, untrusted still 0.9.0, five README tag pins, no
  v0.18.0 left outside history), 68b6bee CHANGELOG cut to
  "## v0.19.0 - 2026-08-13" with a fresh empty Unreleased, c20a128
  close-out carrying the T30 acceptance record above.
- Full check.sh green at 0.19.0, first try, bare container reporting
  "temur 0.19.0"; all three TUI pty smokes quiet.

Stage 2:

- Prep pushed 5417fea..c20a128; ci run 31706292904 on headSha c20a128
  green in both jobs (test 2m14s 13:42:32Z..13:44:46Z, release-gate
  8m29s 13:42:33Z..13:51:02Z).
- Annotated tag v0.19.0 AT c20a128, tag object
  c902af8a26c50d0bdba1cc1c7b88c2b9bc431ca8. The message was verified
  against the RAW object before the tag was pushed, not through
  `git tag -l --format` (which appends its own newline): the object's
  message region was extracted and `cmp`-compared against the intended
  bytes, identical, `od -c` showing it ends in exactly one \n, one
  line, zero em-dash bytes in the whole object. The remote ref
  resolves to the same object hash.
- scripts/release.sh with NO SKIP_CHECK: green first try, 4/4
  artifacts gated and staged, leak grep clean, install.sh/README skew
  gate clean, all three pty smokes quiet for the second run of the
  cycle.

Staged sha256 (and the same values inside SHA256SUMS):

```
d0724dbc  temur-v0.19.0-aarch64-unknown-linux-musl
63118695  temur-v0.19.0-armv7-unknown-linux-musleabihf
622e7e69  temur-v0.19.0-i686-unknown-linux-musl
ce70d46e  temur-v0.19.0-x86_64-unknown-linux-musl
02c7180f  SHA256SUMS itself
```

- Private release created with 5 assets, not draft, not prerelease,
  notes = the CHANGELOG v0.19.0 section verbatim. Repo isPrivate
  confirmed true BEFORE creating it and again AFTER uploading.
- Closing gate: the x86_64 asset and SHA256SUMS were re-downloaded and
  both `cmp`-identical to staged, re-hashing to ce70d46e and 02c7180f,
  with `sha256sum -c` OK inside the download dir. Then a fresh FULL
  download of all five assets, every one `cmp`-identical to staged.
  Installer matrix 6/6 twice, once against the staged dir and once
  against that fresh download (pass + corrupt + unlisted, on the GNU
  host and in busybox).

Residuals carried out of this cycle, none blocking:

- The gate, release and installer runs were launched detached under
  script(1) rather than in a literal foreground shell, the standing
  deviation: they exceed the build session's foreground time cap. All
  were pty-backed, fully teed, and watched for the 180s pty-smoke
  bound, which was never approached.
- The substantive T30 residuals are unchanged and listed in the T30
  record: F1's effect on Qwen2.5-Coder-1.5B is a prediction until the
  next matrix pass measures it, the five surviving T29 queue items
  keep their original numbers, and the USAGE index-size figures are
  arithmetic on an earlier capture rather than a fresh one.

## T31 acceptance - recorded result (no release)

Model floor, round three: seven findings from operator dogfood day 1
plus a Qwen2.5-Coder-1.5B eval re-run, all dated 2026-08-14, built one
phase at a time with a full `scripts/check.sh` gate per phase. Offline
throughout except one live `serve.sh` check against a stub container
(no model was loaded and no provider was called). The matrix was NOT
re-run.

### What shipped

- **H1, the unbounded recovery loop.** Prose-call recovery executed a
  byte-identical resend as a fresh call every time. Eval task 8 has the
  model resending ONE fenced `write` about sixty consecutive times, each
  a fresh SUCCESS, until the context window overflowed: `NUDGE_LIMIT`
  bounds nudges and FAILED executions, and successes were uncapped.
  `ProseRepeatGuard` remembers the last DISPATCHED call (executed or
  failed, since the failure text is fed back either way). A resend equal
  in tool name and argument VALUE is answered with a short honest notice
  rather than run, and the notice counts against the same cap, so the
  sequence is execute, notice, notice, turn end. Any change of name or
  argument resets the guard; key ORDER is not a change, because
  serde_json runs with `preserve_order` and IndexMap equality is
  order-independent. Structured `tool_use` repetition keeps the M2
  doom-loop guard and is deliberately out of scope: no evidence of harm
  there.

- **H3, the unknown tool.** Eval task 7 died in three seconds at 31
  output tokens: a fenced `{"name": "delete", "arguments": {...}}`
  matched neither the executor nor the nudge, both of which require a
  REGISTERED name. That requirement is the false-positive killer and
  stays. `detect_unknown_tool_call` is a SIBLING of the detector rather
  than a widening of it, so every existing pin in
  `detect_text_tool_call` is literally unchanged; the loop names the
  bogus tool and lists the registry, never a hardcoded set. It never
  executes and is capped like every other nudge. It requires a FENCE and
  an arguments-like key, so the unfenced whole-message pin and the
  `{"name": ...}` package.json-fragment pin both hold, and the
  registered paths keep priority.

- **H2, the empty workdir.** bash treated `""` as a path and failed the
  spawn with `No such file or directory (os error 2)`, after which the
  model parroted that error text into its next call's arguments. Empty
  or whitespace after trim now means absent and falls back to
  `ctx.cwd`; a workdir naming a real but missing path still errors. The
  schema description is unchanged.

- **D3, the binary refusal hint.** The refusal worked live (qwen3-4b
  stopped trying to read a PDF as text) but pointed every type at one
  generic hint, sending a PDF toward `unzip -l`. Known types now get a
  remedy they can run: `pdftotext`, `unzip -l`, `zcat`, `tar -tf`, and
  "ask the user to describe it" for images. Unknown types keep the
  pre-T31 sentence byte-identically, and the test pins that with
  `ends_with`.

- **The doctor tools-drop probe.** llama.cpp `--jinja` drops the tools
  array for templates without tool support at HTTP 200, with no log
  line and no response signal. Re-confirmed on `b10423-a94d563ed` on
  2026-08-14 (gemma-3-4b 10/10, Phi-4-mini 4/4, SmolLM2 31/31 prompt
  tokens with and without tools, against a Qwen3-4B control that
  moved), so this is CURRENT behavior, not a fixed historical quirk.
  Reported upstream 2026-08-14. Doctor sends one tiny completion twice,
  bare and with a single probe tool, and compares
  `usage.prompt_tokens`: identical WARNs naming both counts and the
  consequence, differing PASSes naming both, anything unusable is a
  NOTE and never a FAIL. `probe_prompt_tokens` is the THIRD and last
  keyless request doctor may make and takes a base URL and a model id
  and nothing else, so it cannot attach an auth header or touch a key
  file by construction. Active selection only, openai-compat only,
  keyless only, absent under `--no-network`, one generated token each.

- **D1, the prompt sentence.** Asked conversationally ("can you find it
  in the folder?"), qwen3-4b denied having file access while holding
  file tools; the same request phrased as an instruction used them at
  once. Both prompt profiles gain one sentence saying the filesystem is
  reachable through these tools and to list or read a path before
  claiming it cannot be accessed.

- **D4, the serve.sh model mismatch.** With a server already up,
  `serve.sh start <name>` printed `OK: already running` and kept
  serving the OLD model, which silently poisoned a measurement. A model
  REQUESTED by name argument or `MODEL_GGUF` is now compared against
  the running container's mounted source, and a mismatch FAILs naming
  both models and the stop-then-restart sequence. Whether a model was
  requested is recorded BEFORE any defaulting, because by the time the
  already-running check runs `MODEL_GGUF` is always set; the
  no-model-requested path is unchanged.

### Design choices worth recording

- **H3 as a sibling, not a widening.** The kickoff described widening
  `detect_text_tool_call`. Returning a variant from that function would
  have rewritten its whole pin table, which is the neighbouring
  contract T30 went out of its way to keep byte-identical. A separate
  `detect_unknown_tool_call`, consulted only when the registered paths
  find nothing, gets the same behavior with every existing pin
  untouched.

- **H1 reuses `NUDGE_LIMIT`.** The kickoff allowed a sibling constant.
  Reusing the existing counter means the repeat notice, the failed
  prose execution, and the plain-text nudge all draw on one per-turn
  budget, which is the property that matters (a stubborn model ends its
  turn) without a second number to keep consistent.

### Test changed by a behavior change

`prose_call_failures_count_toward_nudge_cap_and_terminate` scripted
three IDENTICAL failing prose calls. Under H1 the second one takes the
repeat-guard path instead of a second execution, so the test would have
measured the guard rather than the failure cap. It now uses three
distinct targets and still pins exactly two failed executions per turn.
Recorded because a changed test is a claim about intent, not a fix.

### Deviations

- **The evidence archive did not exist at the stated path.** The
  kickoff pointed at `~/temur-eval-archive/coder15b-2026-08-14/`, which
  was absent. The transcripts were found at `/tmp/temur-weak-eval/`
  (task1 through task9, mtimes 2026-08-14 15:54 to 16:00) and every
  finding matched the kickoff description, so they are the same
  evidence. All nine were copied to the archive path the kickoff named,
  since `/tmp` does not survive a reboot. The build session CREATED
  that directory rather than reading it.

- **The kickoff text arrived truncated.** Several lines ended
  mid-sentence (the H1, H2 and H3 bullets, the P1 H3 contract, P2, P3,
  P4, and the closing paragraph). The contracts were read from the
  surviving text plus the three transcripts. The two places where a
  choice had to be made are recorded under "Design choices" above.

- **The gate runs were launched detached under `script(1)`**, the
  standing deviation: a full gate exceeds the build session's
  foreground command cap. Every run was pty-backed, fully teed to a
  log, and watched for the 180s pty-smoke signature.

### Live check (D4 only)

Verified against a stub container (`busybox`, `sleep 300`, a model file
bind-mounted so `podman inspect Mounts` reports it) standing in for a
running server, so nothing loaded a model:

```
start Qwen3-1.7B      -> FAIL naming both models, exit 1
start Qwen3-0.6B      -> OK: already running (the mounted model)
MODEL_GGUF=SmolLM2... -> FAIL naming both models, exit 1
no model requested    -> OK: already running (previous behavior)
```

The stub container was removed afterward.

### Deliberate non-changes

The matrix was NOT re-run: every score in OFFLINE.md keeps its
2026-08-12 date. Native `tool_use` repetition stays unguarded beyond
the doom loop. D2 (a turn that announces future action with zero tool
calls) and a `serve.sh` `SERVER_ARGS` knob are queued in ROADMAP with
their reasons, not built.

### Gate outcomes

Four phases, four full `check.sh` runs, every one ending
`== ALL CHECKS PASSED ==` with exit 0 across both paths (gnu-debug and
the musl-release acceptance path), each green on the FIRST try. All
three TUI pty smokes reported OK in all four runs; the 180s bound was
never approached and the hang signature never appeared. Staticness
clean each time (no INTERP, no NEEDED), forbidden-deps clean, bare
busybox container printing the version.

### Phase commits

```
d43c64a  P1  prose repeat guard (H1) + unknown-tool feedback (H3)
015bad1  P2  empty workdir means absent (H2) + typed binary read hints (D3)
88c7a99  P3  doctor probe for silently dropped tool definitions
a21b9a7  P4  prompt sentence (D1), serve.sh model mismatch (D4), docs
```

### Residuals

- No upstream issue NUMBER was available when this shipped, so the
  doctor doc comment and the OFFLINE paragraph carry the dated
  "Reported upstream 2026-08-14" sentence instead. Folding a number in
  later is a one-line doc edit in two places.
- The doctor probe has no live leg in this cycle: it is verified
  against canned servers only. The upstream behavior it detects was
  measured live on 2026-08-14, but temur's own WARN has not yet fired
  against a real llama.cpp server in a recorded run.
- The Coder-1.5B re-run scored 5/9 and neither confirms nor refutes
  T30's prediction that the preamble-then-fence fix would raise that
  model. It was a dogfood pass, not a matrix pass. Recorded on the T30
  ROADMAP row as an UPDATE and in the T31 queue section; the formal
  check stays with the next matrix pass.
- `src/tools/edit/matchers.rs:122` carries a pre-existing `dead_code`
  warning on `line_trimmed`, untouched by T31 and noted so it is not
  read later as introduced here.

## v0.20.0 acceptance - recorded result (SHIPPED, private)

What shipped: T31 alone, model floor round three. Seven findings from
operator dogfood day 1, all offline-verifiable; the T31 acceptance
record above carries the per-finding detail, the design choices, the
deviations, and the deliberate non-changes.

Stage 1:

- Four T31 commits pushed d43c64a..a21b9a7 (P1 prose repeat guard (H1)
  and unknown-tool feedback (H3), P2 empty workdir means absent (H2)
  and typed binary read hints (D3), P3 doctor probe for servers that
  silently drop tool definitions, P4 prompt sentence (D1), serve.sh
  model mismatch (D4), docs). On-push ci run 31841482324 on headSha
  a21b9a7 green in both jobs (test 1m06s 21:13:33Z..21:14:39Z,
  release-gate 6m39s 21:13:33Z..21:20:12Z).
- Three local prep commits: 64e6590 bump (four files, Cargo.lock's
  temur entry only, Cargo.toml, five README tag pins, scripts/install.sh),
  d584974 CHANGELOG cut to "## v0.20.0 - 2026-08-14", d3d7fe7 close-out
  carrying the T31 acceptance record above.
- Stage 1 verified by the planning session before stage 2 opened.

Stage 2:

- Prep pushed a21b9a7..d3d7fe7; ci run 31854690993 on headSha d3d7fe7
  green in both jobs (test 2m06s 00:47:56Z..00:50:02Z, release-gate
  7m47s 00:47:56Z..00:55:43Z).
- Annotated tag v0.20.0 AT d3d7fe7, tag object
  241deabf026242591a3e8dd8d66cbdc452785416. The message was verified
  against the RAW object before the tag was pushed, not through
  `git tag -l --format` (which appends its own newline): the object's
  message region under `od -c` is exactly
  "temur v0.20.0 - model floor round three (T31)" followed by one \n,
  45 message bytes, one line, ASCII hyphen, zero em-dash bytes. The
  remote ref resolves to the same object hash.
- scripts/release.sh with NO SKIP_CHECK: green first try, 4/4
  artifacts gated and staged, leak grep clean, install.sh/README skew
  gate clean, all three TUI pty smokes quiet, bare busybox container
  reporting "temur 0.20.0".

Staged sha256 (and the same values inside SHA256SUMS):

```
06ede7a8  temur-v0.20.0-aarch64-unknown-linux-musl
347b82e0  temur-v0.20.0-armv7-unknown-linux-musleabihf
f09a3897  temur-v0.20.0-i686-unknown-linux-musl
e60ec10f  temur-v0.20.0-x86_64-unknown-linux-musl
1ae6b3b2  SHA256SUMS itself
```

- Private release created with 5 assets, not draft, not prerelease,
  notes = the CHANGELOG v0.20.0 section verbatim, title "temur v0.20.0
  - model floor round three (T31)". Repo isPrivate confirmed true
  BEFORE creating it and again AFTER uploading.
- Closing gate: the x86_64 asset and SHA256SUMS were re-downloaded and
  both `cmp`-identical to staged, re-hashing to e60ec10f and 1ae6b3b2,
  with `sha256sum -c` OK inside the download dir. Then a fresh FULL
  download of all five assets, every one `cmp`-identical to staged and
  `sha256sum -c` 4/4 OK. Installer matrix 6/6 twice, once against the
  staged dir and once against that fresh download (pass + corrupt +
  unlisted, on the GNU host and in busybox).

Residuals carried out of this cycle, none blocking:

- The gate, release and installer runs were launched detached under
  script(1) rather than in a literal foreground shell, the standing
  deviation: they exceed the build session's foreground time cap. All
  were pty-backed, fully teed, and watched for the 180s pty-smoke
  bound, which was never approached. Second consecutive cycle with
  every pty smoke quiet and zero reruns.
- The substantive T31 residuals are unchanged and listed in the T31
  record: the doctor probe has no live leg, the upstream llama.cpp
  report carries no issue number yet, the Coder-1.5B dogfood score
  does not settle T30's prediction, and the pre-existing `dead_code`
  warning at `src/tools/edit/matchers.rs:122` is untouched.

## T32 acceptance - recorded result (no release)

Measurement milestone with one harness change ahead of it: the five
surviving T29 queue items cleared, then the whole local-model matrix
re-measured against the SHIPPED v0.20.0 binary. No product behavior
changed. The only Rust touched is one doc comment.

### Conditions

Binary: the i686 musl-static release, sha256
`f09a38978643efcf063a2434e2336b77bd0970bff7273c4feee7993091b38f0e`,
verified byte-identical to the shipped v0.20.0 i686 asset before any
measurement ran, reporting `temur 0.20.0`.

Server: `ghcr.io/ggml-org/llama.cpp:server-b10438`, manifest digest
`sha256:190813e82f33a82f506e66826f367004a3159f8b8139b11d07566437aecdac93`,
self-reported `version: 0.1.0-dev (build 10438, commit 9d57ce456)`,
`--jinja`, ctx 8192. The pin was chosen by paginating the GHCR tag list
(11 pages, 10,614 tags, 478 matching `server-b<digits>`) and taking the
newest; the registry's first page returns a stale b5xxx-era window and
cannot be trusted for this.

Harness: `scripts/weak_model_eval.sh` at 10a7787, compact prompt
profile, `EVAL_MAX_TOKENS` 3072, each task in a fresh work dir with a
fresh process and a mounted state dir, inside a `--network none` pod.
Keyless throughout; no Anthropic call was made in any phase.

### P0: the bridge run

The unchanged harness at 59b7878 was run once on Qwen2.5-Coder-1.5B
against the v0.20.0 binary, to check T31's three fixes live and to
settle T30's F1 prediction. `SCORE: 5/9`, failed tasks 5, 7, 8, 9,
about 140s total.

The archived 0.19.0 run also scored 5/9 but failed 2, 6, 7, 8: same
number, four of nine tasks moved. That run's failed-task NUMBERS were
never recorded anywhere in the tree (RUNBOOK and ROADMAP give the score
only), so they were RECONSTRUCTED from the archived transcripts rather
than assumed: four transcripts show a failing end state and five show
the assertion's target being produced, which is the recorded 5/9.

- **H1 CONFIRMED, and wider than T31 described.** The unbounded resend
  also ran on eval tasks 1 and 4 (62 and 60 consecutive executions of
  one identical call, each ending in an HTTP 400 context overflow).
  Both tasks PASSED anyway, because the first execution already did the
  work, so the defect was invisible in the score and visible only as
  cost: task 8 alone billed 321,207 input tokens. In the bridge run all
  three are bounded to execute-then-notice-then-notice, task 8's input
  falls to 11,530 (a 96.4% reduction), and there are zero context
  overflows against three in the archive. The guard fired on SEVEN of
  the nine tasks.
- **H3 CONFIRMED.** Where the archived transcript had three seconds and
  31 output tokens of silence after a fenced `{"name": "delete", ...}`,
  the bridge run names the tool that does not exist and lists the real
  ones. The model then tried `write` and `read` and never reached
  `bash`, so the task still FAILS. Firing is the fix; recovery was
  never claimed.
- **H2 NOT EXERCISED.** The prediction was that task 6's empty
  `workdir` would now fall back to cwd. The model sent no `workdir` at
  all this time, so the code path was never entered; task 6 moved FAIL
  to PASS on a model-side difference. H2 still has no live leg.
- **T30 F1 NOT CONFIRMED.** Across 0.18.0-era 7/9, 0.19.0 5/9 and
  0.20.0 5/9 the score never rose. What T31 changed is the SHAPE of the
  failures, not the count. The matrix pass then measured this model at
  4/9 twice with six of nine tasks moving between runs, so the noise is
  larger than the effect being looked for. Closed as unprovable by this
  instrument rather than left open.

### P2: the doctor tools-drop probe, first live leg

Every model on disk, one at a time: `serve.sh start`, `temur doctor`
against it with a keyless local config, tools-drop line recorded
verbatim, `serve.sh stop`. Ten models, zero servers left running.

```
model                          verdict  prompt_tokens (without / with tools)
Qwen3-4B-Instruct-2507          PASS      9 / 147
Qwen3-4B-Thinking-2507          PASS     11 / 149
Qwen2.5-Coder-3B-Instruct       PASS     30 / 163
Qwen2.5-Coder-1.5B-Instruct     PASS     30 / 163
Qwen3-1.7B                      PASS      9 / 147
Qwen3-0.6B                      PASS      9 / 147
Llama-3.2-3B-Instruct           PASS     36 / 170
gemma-3-4b-it                   WARN     10 / 10
Phi-4-mini-instruct             WARN      4 / 4
SmolLM2-1.7B-Instruct           WARN     31 / 31
```

T31 measured those same three templates BY HAND on a different server
build (`b10423-a94d563ed`) and got gemma-3-4b 10/10, Phi-4-mini 4/4,
SmolLM2 31/31. Two independent methods, two server builds, identical
counts. That closes the T31 residual recording that the probe had never
fired against a real server in a recorded run.

Llama-3.2-3B PASSes the probe while scoring 2/9, which is the probe
working correctly rather than contradicting itself: it receives the
tools and fails later, so the probe separates the template-drop family
from every other failure mode. A model can pass the probe and still be
unusable; the probe never claimed otherwise.

### P3: the matrix, measured 2026-08-15

Two runs per model; the three WARN rows ran once under the probe-gated
economy, and all three scored 0, so none was promoted.

```
model                          run 1  run 2   failed tasks
Qwen3-4B-Instruct-2507          9/9    9/9    none / none
Qwen3-4B-Thinking-2507          7/9    9/9    6,8 / none
Qwen2.5-Coder-3B-Instruct       6/9    9/9    5,6,8 / none
Qwen3-1.7B                      7/9    7/9    8,9 / 5,8
Qwen3-0.6B                      5/9    5/9    2,5,8,9 / 2,5,8,9
Qwen2.5-Coder-1.5B-Instruct     4/9    4/9    1,3,5,8,9 / 2,5,7,8,9
Llama-3.2-3B-Instruct           2/9    2/9    2,3,4,6,7,8,9 (both)
gemma-3-4b-it                   0/9     -     all nine
Phi-4-mini-instruct             0/9     -     all nine
SmolLM2-1.7B-Instruct           0/9     -     all nine
```

NOT comparable to the 2026-08-12 table: the server build, `max_tokens`
and two task wordings all changed at once.

Variance is the headline. Two models changed score between consecutive
runs under fixed conditions (Coder-3B by 3 tasks, Thinking by 2). Two
more held their score while the task set moved: Coder-1.5B scored 4/9
twice with only tasks 4 and 6 passing both times, six of nine moving,
and Qwen3-1.7B scored 7/9 twice failing a different pair each run. Only
Qwen3-0.6B repeated its exact task set. This independently reproduces
P0's finding on a different axis.

Task difficulty across the 7 tool-capable models (14 runs): task 8
failed 10 times, task 9 seven, task 5 six, task 2 five, task 6 four,
tasks 3 and 7 three each, task 4 twice, task 1 once. Task 8 is the
discriminator and only the two 4B models ever pass it.

### Third runs, run 2026-08-16

The milestone's ban covered HARNESS auto-logic, not the policy itself.
The operator invoked the third-run rule (spread >= 2, SCORE reading) for
both qualifying models, one `EVAL_RUNS=1` run each, same binary, server
and settings as the pass.

```
model                        run 1  run 2  run 3   failed in run 3
Qwen3-4B-Thinking-2507        7/9    9/9    9/9    none
Qwen2.5-Coder-3B-Instruct     6/9    9/9    7/9    6, 8
```

Qwen3-4B-Thinking resolves: two consecutive sweeps after the 7/9, and
run 3 passed both tasks run 1 failed. 9/9 is the model's level.

Qwen2.5-Coder-3B does NOT resolve. Three runs, three different scores,
spanning 3 tasks, with the third landing between the first two. The
policy call was right and the answer it returned is "still unresolved",
which is a fact about the instrument rather than about the model: at
this spread no small number of runs pins the row down. The table shows
the triple rather than a representative score, because there is no
honest single number to show.

The rule's ambiguity survives and is worth recording: on a TASK-SET
reading rather than a score reading, Qwen2.5-Coder-1.5B is the
strongest candidate in the matrix (six of nine tasks moved at an
identical 4/9) and Qwen3-1.7B also qualifies. Neither was run; that
reading stays optional and operator-invoked.

Wall clock, sum of task durations: Thinking 6172s, Qwen3-1.7B 2309s,
Instruct 514s, Coder-1.5B 366s, Coder-3B 358s, Llama 304s, gemma 68s,
Phi-4-mini 55s, SmolLM2 32s, Qwen3-0.6B 729s. Thinking alone is more
than half the pass: same size as Instruct, same 9/9 ceiling, twelve
times the wall clock.

### F1. VERIFIED: Llama-3.2-3B has a second, undocumented failure mode

The published table explained this row entirely by llama.cpp's
peg-native grammar rejection. That is still live (nine provider errors
across tasks 2, 4, 6, 7, 8, 9), but it is not the only cause. The new
state-dir archiving shows the model emitting structurally perfect tool
calls whose scalar arguments are stringified:

```json
{"type": "tool_use", "name": "edit",
 "input": {"filePath": "/work/config.ini",
           "oldString": "mode = development",
           "newString": "mode = production",
           "replaceAll": "false"}}
```

`replaceAll` is the string `"false"`, not the boolean. temur answers
`invalid type: string "false", expected a boolean`, the model resends
the identical call twice more, and the repeat guard stops it at three.
Every other argument was correct.

This closes T29 queue item 9 with the argument capture it asked for,
and it proves the sibling state mount was necessary to see any of it.

Within this model the shape is systematic, sixteen rejections across
five archived tasks: six `"false"` for a boolean, and ten numeric
strings for `u64` (`"600000"` five times, `"120000"` twice,
`"1200000"`, `"null"`, `"0"`). Only booleans and `u64` counts are
affected, which is the entire set of non-string scalars the tool
schemas use. Queued in ROADMAP as a tolerant-parsing item, NOT fixed
here: changing argument handling mid-pass would have made rows
incomparable.

The finding is confined to Llama-3.2-3B: no other model in the matrix
produced a single `invalid type: string` rejection. Qwen2.5-Coder-1.5B
has exactly one invalid-argument event in the whole archive
(`task2.run2`), and it is a different class, `offset must be greater
than or equal to 1`, a RANGE check that runs after the type parsed
successfully. Tolerant coercion would not have changed it.

### F2. VERIFIED: `EVAL_TASK_TIMEOUT` is advertised and not enforced

The knob documents itself as "seconds allowed per task" and defaults to
300. Ten task runs exceeded it on 2026-08-15, worst 994s at 3.3x the
cap (Thinking task 8), then 807s (Qwen3-1.7B task 8) and 742s. The
`timeout` call wraps `podman run`, but the podman client keeps waiting
after the signal fires, so the bound never binds. The line is
byte-identical to its pre-T32 form, so this is long-standing and not
introduced by P1. Deliberately not fixed mid-pass, for comparability.
Queued. The OFFLINE conditions caption states no per-task bound.

### Deliberate non-changes

The task count stays 9 and the seeds stay fixed, so round two remains
comparable to round three. No auto-third-run logic: the 2-task rule is
the operator's to invoke. The three template-limited families stay in
the table with their scores, because the fix is upstream at
ggml-org/llama.cpp#27129, not in temur.

The T31 acceptance record above keeps its "Reported upstream
2026-08-14" wording: it is a historical record of what was true when
T31 shipped. The issue number was folded into the two LIVE claim sites
that T31's own residual named, the `src/doctor.rs` doc comment and the
OFFLINE paragraph.

### Phase commits

```
b7c35cf  P1  eval harness knobs and artifact retention
10a7787  P3  bump the pinned llama.cpp server build to b10438
(this commit)  P4  matrix restatement and queue dequeue
```

P0 and P2 produced no commits by design.

### Residuals

- Granite-3.3-2B-Instruct and Hermes-3-Llama-3.2-3B were not on disk
  when the pass ran, so the matrix is TEN rows rather than the twelve
  planned. Adding them later is a run plus two table rows; nothing
  about the existing rows changes.
- H2 (empty `workdir` means absent) still has no live leg. Two matrix
  passes have now failed to make a model send an empty `workdir`.
- `README.md` prints a 9/9 task table for Qwen3-4B-Instruct-2507 under
  `server-b10068` attributed to T19 acceptance. It is true as recorded
  and outside this milestone's stated scope, but it now sits beside a
  table measured on a different build and reads as current.
- The eval driver scripts for P2 and P3 live in the session scratchpad,
  not the repo, on the same reasoning as T29's: these phases ask for
  measurements, not new committed tools. The scratchpad was cleared
  partway through and the P3 driver was rebuilt from the archive
  ledger, which is what made the pass resumable in practice.
- The pass was interrupted once by an operator pause and resumed.
  Qwen2.5-Coder-3B was in flight and was re-measured from scratch, so
  no row mixes data from before and after the pause.
- The pre-existing `dead_code` warning at
  `src/tools/edit/matchers.rs:122` is untouched.

## v0.21.0 acceptance - recorded result (stage 1 only, NOT released)

What shipped: T32 alone, eval harness round two plus a full matrix
refresh. The T32 acceptance record above carries the per-phase detail,
the third runs, the two new findings, the deviations and the residuals.

Stage 1:

- Four T32 commits pushed 59b7878..048c8d9 (P1 eval harness knobs and
  artifact retention, P3 the llama.cpp server pin bumped to b10438, P4
  the matrix restatement and T29 queue dequeue, and the rider carrying
  the two third runs and the corrected stringified-scalar attribution).
  On-push ci run 31954598928 on headSha 048c8d9 green in both jobs
  (test 1m10s 15:05:14Z..15:06:24Z, release-gate 4m27s
  15:05:15Z..15:09:42Z).
- Three local prep commits: 292332a bump (four files, Cargo.lock's
  temur entry only with `untrusted` still pinned 0.9.0, Cargo.toml,
  scripts/install.sh VERSION, five README tag pins, zero 0.20.0
  residual outside history sections), cf45f57 CHANGELOG cut to
  "## v0.21.0 - 2026-08-16" with a fresh empty Unreleased above it and
  no entry text touched, and this close-out.
- Full `scripts/check.sh` on the prep head: ALL CHECKS PASSED, exit 0,
  all three TUI pty smokes OK, bare busybox container printing
  "temur 0.21.0". Log kept beside the T32 gate logs in the evaluation
  archive.
- Em-dash differential across the three prep commits: 0 added lines,
  0 in any of the three messages.
- No tag and no release in this stage. Version 0.20.0 is now 0.21.0 in
  the tree only.

Stage 2: not yet run.
