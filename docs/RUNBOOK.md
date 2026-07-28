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
