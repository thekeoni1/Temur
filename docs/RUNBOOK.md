# RUNBOOK — Tier-1 live smoke (operator steps)

Everything here is a **human/operator** procedure. The build agent (`dev`) cannot and
must not perform these steps: it can't read the credential, and it deliberately can't
write the binary that `appsvc` executes (a builder-writable binary would nullify the
secret boundary).

## What the builder staged

- Binary: `/home/dev/dist/temur`
  (i686 musl-static ELF, release, stripped — the `i686-unknown-linux-musl`
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

Optional config (model, max_tokens, thinking) — appsvc's HOME is
`/srv/rustcode-runtime`, so the file is
`/srv/rustcode-runtime/.config/temur/config.json`, e.g.
`{"model": "claude-sonnet-5"}`. Defaults: claude-sonnet-5, 32000 max_tokens,
thinking off.

Provider selection (T2): `{"provider": "openai-compat", "openai_compat":
{"base_url": "http://127.0.0.1:8080/v1", "model": "<model-id>"}}` targets any
OpenAI-compatible endpoint (llama.cpp, Ollama, vLLM, LM Studio, or a hosted
compat API). Local endpoints need no credential — omit `api_key_file` and no
auth header is sent. A keyed endpoint reads its key from `api_key_file`, a
file path with the same isolation rule as `APP_SECRET_FILE` (never env, never
argv). The default provider remains `anthropic`; selecting the compat
provider leaves the Anthropic fields untouched.

## 3. One-time SSE capture (golden fixtures), then the smoke

Run the first turn with capture enabled so the raw wire streams get frozen into the
test suite (SSE bodies contain no credentials — the key travels only in a request
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
on **stdout** (previously stderr) — adjust any stderr-filtering scripts.

At the `>` prompt (line REPL) or in the TUI input line, run the Tier-1 smoke
script (one prompt per line):

1. `Create a file named smoke.txt containing the single line "tier1", then read it back to me.`
   — expect: `write` then `read` tool activity; the reply quotes `tier1`.
2. `Run the shell command "uname -m" and tell me the output.`
   — expect: `bash` tool activity; the reply contains `i686` (via linux32 personality)
   or `x86_64` (host kernel) — either proves live execution; note which.
3. `Change smoke.txt so it says "tier1 passed" instead, and confirm by reading it.`
   — expect: `edit` tool activity; the reply confirms the new content.
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

## T3 offline acceptance — recorded result

2026-07-19: `scripts/offline_demo.sh` passed end-to-end on the operator
machine (rootless podman, WSL2), first attempt:

- server image: `ghcr.io/ggml-org/llama.cpp:server-b10068` (854 MB on
  disk), ctx 8192, `--jinja`
- model: `/home/dev/models/Qwen3-1.7B-Q4_K_M.gguf` (from
  `unsloth/Qwen3-1.7B-GGUF`, 1.11 GB) — the docs/OFFLINE.md primary
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
`MODEL_GGUF=/home/dev/models/Qwen3-1.7B-Q4_K_M.gguf scripts/offline_demo.sh`
— the script never pulls; if an image is missing it prints the exact pull
command and exits.

## T4 weak-model acceptance — recorded result

2026-07-20: **T4 acceptance met — `scripts/weak_model_eval.sh` scored 5/6
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

Iterations (disclosed, per the pre-agreed protocol — prompts/wording only,
no model swap): the first run scored 4/6 with the eval config's
`max_tokens: 1024` — tasks 2 and 5 both truncated at max_tokens because
the model's streamed reasoning counts against the completion budget, with
zero tool calls completing. The generated config was raised to
`max_tokens: 2048` (comment recorded in the script) and task 2's wording
made stepwise after the original phrasing ("JUST the value… nothing
else") provably sent the model into a reasoning spiral. Task 5 then
passed. Task 2's remaining failure is an honest capability floor, not a
harness artifact: the model issued its read and write calls in ONE
parallel batch, writing token.txt before the read result existed, then
claimed success in prose (the host-verified assertion correctly failed —
model prose is never evidence).

Live captures, frozen the same day: 14 raw SSE streams in
`tests/fixtures/live-openai/` (11 with `--jinja`, 3 without), covering
plain text, a single tool call, a live repeated call, arguments
fragmented across many chunks, a tool-error round-trip, a live PARALLEL
three-call response, and post-result texts.
`tests/live_conformance_openai.rs` walks them strictly (per-chunk key
allowlists derived from these captures — including llama.cpp's
`reasoning_content` deltas and `timings` extension — sequence invariants,
and a zero-tolerance assembler pass) and is green.

No-jinja finding: llama.cpp `server-b10068` emitted fully STRUCTURED
`tool_calls` for Qwen3-1.7B even without `--jinja` — the wire shape was
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
each temur launch — nothing is left to the operator.

## T5 acceptance — recorded result

2026-07-21: **live resume smoke PASSED, first attempt** — the full cycle
live turn → save → SIGKILL → `--continue` → seeded history on the wire,
end-to-end through main.rs, reusing the T3/T4 infrastructure unchanged
(llama.cpp `server-b10068`, Qwen3-1.7B Q4_K_M, ctx 8192, `--jinja`,
`--network none` pod; in-pod `tls-probe` FAILED as required before any
turn; musl-static binary readelf-verified in preflight; compact profile,
max_tokens 2048 — the T4 eval settings).

- **Run 1** (two turns): the model drove a real `write` creating
  `milestone.txt` (content `t5-resume-proof`, host-verified) and a real
  `todowrite`. The session file appeared after turn 1 — first-save
  behavior confirmed live. `kill -9` of the temur process mid-idle.
- **Survival (host-verified):** the file survived SIGKILL intact — parsed
  as JSON, version 1, 8 messages, the todo present, 1661 bytes. No
  `.tmp` litter (the kill was mid-idle; any would be harmless per below).
- **Run 2** (`--continue`): notice rendered exactly as
  `[!] resumed session: 8 messages, ~10769 tokens in / 699 out` —
  matching the file's contents — with zero mismatch lines (same
  provider/model/cwd, as expected). Asked *without tools* what file it
  had created earlier and with what text, the model answered
  `/work/milestone.txt` / `t5-resume-proof` from the seeded history
  alone; a live `todoread` returned the seeded todo. Session usage
  continued from the saved totals (10769+2850 in on the first resumed
  turn) rather than restarting from zero.
- **Post-exit re-validation:** the grown file parses, version still 1,
  8 → 14 messages, 1661 → 2627 bytes — save-after-resume produces a
  valid file, not just a larger one.
- Incidental: `cache_creation_input_tokens` (never reported by
  llama.cpp) round-tripped save → SIGKILL → load as JSON `null`, and both
  runs displayed it as `—` — absent-vs-zero preserved across persistence.

Artifacts (transcripts, the session file pre- and post-resume) kept at
`/home/dev/temur-t5-resume-smoke-2026-07-21/`.

## T5 sessions (operator notes)

Live runs persist the conversation after every turn (mock runs never do).
Location: `$XDG_STATE_HOME/temur/sessions/`, falling back to
`~/.local/state/temur/sessions/` — for the appsvc runtime identity (HOME
`/srv/rustcode-runtime`) that is
`/srv/rustcode-runtime/.local/state/temur/sessions/`. One file per working
directory (`<basename>-<hash>.json`); `--continue` from the same directory
resumes it. Config: `sessions_dir` relocates the directory,
`session_max_bytes` caps the file (default 4 MiB).

- **Power-cut semantics:** saves are write→fsync→rename atomic. The previous
  complete file is always intact; after a crash you resume the last fully
  saved turn. A leftover `*.json.tmp.<pid>` file is harmless litter from an
  interrupted save — safe to delete, never loaded.
- **Two processes, same directory:** safe against corruption — each writes
  its own pid-suffixed temp file and renames a complete file into place.
  Last writer wins; the sessions aren't merged.
- **Reset:** `rm` the directory's file from the sessions dir (the startup
  error for a corrupt or version-mismatched file names the exact path).
- **`--continue` fails fast** on a missing, corrupt, or wrong-version file
  rather than silently starting fresh; run without `--continue` to start a
  new session.

- `secret: APP_SECRET_FILE is not set` — run via `run-app.sh` or pass the env as above.
- `secret: cannot read credential file` — check ownership/mode (`appsvc:appsvc` 600)
  and that you're running as `appsvc`.
- `api error (HTTP 401)` — the credential file's contents are wrong (whole file is
  used, trimmed, as the API key).
- The build environment never performs this procedure; per project rules the live API
  is only ever touched here, by the operator, as `appsvc`.

## T6 interruption (operator notes)

Esc during a TUI turn interrupts it cooperatively (status row shows
`interrupting…`, then a `turn interrupted` notice). Effect on the session
file: the turn lands on a wire-valid boundary and the normal after-turn
save runs, so the file ends in one of three shapes — partial assistant
text; an assistant message whose tool calls are answered by synthesized
`[interrupted by user]` error results (kept on `--continue`); or, when
the interrupt landed before any content, the bare user prompt (dropped on
`--continue` with the usual notice). All three resume cleanly.

A running `bash` is killed with its whole process group within ~200 ms —
no orphaned children. Esc cannot reach a FULLY stalled stream (no frames
arriving); double-Ctrl+C force-quit (exit 130) remains the escape hatch
there, and the session file then simply holds everything up to the last
completed turn (the in-flight turn was never saved). The plain line REPL
has no interruption — documented T6 exclusion.

## T6 acceptance — recorded result

2026-07-22: **live interrupt + fuzzy-edit smoke PASSED — all seven gated
checks (2a–2g)**, on the T3/T4/T5 infrastructure unchanged (llama.cpp
`server-b10068`, Qwen3-1.7B Q4_K_M, ctx 8192, `--jinja`, `--network none`
pod; in-pod `tls-probe` FAILED as required before any turn; compact
profile, max_tokens 2048). The musl binary was installed to
`/srv/rustcode-runtime/bin/app` from the Windows-side root path (RUNBOOK
§1) and sha256-verified identical to the gated staged copy
(`20c98142…c3888`); the pod executed that same hash-verified staged
binary (T5 precedent — the runtime copy is not mountable from dev's
rootless podman, and the hash tie is the point). TUI driven by scripted
keystrokes over the podman pty; sessions on a host-mounted state dir.

- **2a interrupt mid-stream:** Esc sent 10 s into streaming (mid-stream
  proven by transcript growth +21 KB and cross-checked against server
  timing). `interrupting…` state, `turn interrupted` notice, partial
  lighthouse-story tail and the ▣ turn tail all rendered; the prompt was
  back within **≤2 s** of Esc (a Ctrl+D landing-probe quit the app).
  Server side, llama logged `srv stop: cancel task` **114 ms** after the
  Esc stamp — this build notices the dropped connection immediately, so
  the "post-interrupt slot hangover" watch item did not materialize.
- **2b/2g session validity:** checked with a python3 script (jq is not
  installed on the host and installing needs elevation — same check
  content, disclosed substitution). Pre-resume file: parsed, wire-valid.
  Final file after all runs: 17 messages, **3 tool_use ids, every one
  answered by a tool_result in the immediately following message**; the
  file ends with run 5's bare user prompt — the documented empty-landing
  shape that `prepare_seed` drops on the next `--continue`.
- **2c resume:** `--continue` rendered the resume notice; the server
  accepted the seeded history including the interrupted partial text (no
  400, clean turn). The model answered from pre-interrupt session content
  (Elaris story details recalled without tools). Honest nuance: asked
  *which* story was interrupted, it recounted the completed first story
  rather than the interrupted lighthouse one — a 1.7B comprehension slip;
  the wire-level acceptance and cross-interrupt recall are what the step
  gates, and both held.
- **2d bash interrupt:** the model ran `sleep 60` via bash; /proc scans
  inside the app container show the forked pair `sh -c sleep 60` +
  `sleep 60` BOTH running pre-Esc and BOTH GONE at Esc+4 s with temur
  still alive — the I3 process-group kill proven live, no orphaned
  children. History landed exactly the designed shape: assistant
  tool_use answered by `(interrupted by user)` `is_error` tool_result;
  no post-Esc server request (the pre-POST token check held). The
  transcript shows `interrupting…` → ▣ ~30 bytes apart (landing within
  ~one render frame).
- **2e pre-first-byte (ran LAST as its own `--continue` session —
  disclosed sequencing deviation):** Esc 0.3 s after Enter, during
  prompt processing. Server logged `cancel task` **446 ms** after the
  Esc — b10068 emits an early chunk before prompt eval completes, so the
  blocked-read residual barely bites on this server. The residual stands
  as documented for servers that send nothing until the first token.
  Notice + tail rendered; app quit by Esc+6 s (probe granularity).
- **2f live fuzzy edit:** tab-indented `app.py` seeded; the model called
  edit with an UNINDENTED `oldString` (`value = 1`) — a legitimate exact
  substring match, so the fuzzy fallback was correctly NOT consulted
  (output marker absent, `Edited /work/app.py (1 replacement(s))`
  byte-identical v1 shape). Host-verified: `value = 2` with tab
  indentation preserved. Recorded as the plan anticipated: the binding
  fuzzy proof is the E3 scripted e2e plus the builder mock smokes (which
  exercised the fallback in both the gnu and musl binaries).
- **Watch item (synthesized-result reaction):** in run 4 the model had
  the `(interrupted by user)` bash result in context and simply
  proceeded with the new task — no confusion, no spontaneous retry of
  the aborted command. First live data point: benign.
- **Harness observation (not a product signal):** scripted keystrokes
  through the podman-attach pty occasionally coalesce, and crossterm
  clears its whole parse buffer per event — a batched `EscEsc+Ctrl+D`
  yields the Esc and silently drops the Ctrl+D, so some landing-probes
  registered one probe-interval late (also seen once with no Esc
  involved at all). Characterized with three offline probes (lone
  late Ctrl+D at idle: quits in <1 s); app-side landing is evidenced
  frame-adjacent in every interrupted run. Real-terminal input does not
  batch this way; noted as a follow-up curiosity, not a defect.
- **Run 1a baseline (driver retry, disclosed):** the first 2a attempt's
  Esc fired after the turn had already completed (a wrong server-log
  grep plus the lone-ESC pty ambiguity — fixed by transcript-growth
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
