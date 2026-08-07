# Changelog

Newest first. Dates are release dates; "Unreleased" ships next.

## Unreleased

## v0.12.0 - 2026-08-07

Hosted-provider verification (T13): the openai-compat provider run
against the real OpenAI and Gemini endpoints for the first time, with
every fix below found by that run rather than by review.

- **Tool calls now dispatch on assembled calls, whatever
  `finish_reason` says.** Gemini's streaming responses report "stop"
  while attaching real tool calls (its non-streaming responses report
  "tool_calls" for the identical request). temur streams, believed the
  wire, and silently discarded well-formed calls: no tool ran, nothing
  printed, and the saved session was left holding a `tool_use` with no
  `tool_result`. Assembled calls now mean tool use regardless, with
  one exception: a completion the provider filtered still refuses
  rather than dispatching. `finish_reason` "length" continues to
  report truncation, now even when calls were assembled too.
- **Gemini's thought signatures round-trip.** Its tool calls carry
  opaque state that must come back on the next request, or the request
  is rejected, which broke the agent loop on its first round trip
  while single-shot calls worked. Tool calls now carry optional opaque
  provider state through the neutral types, saved sessions, and back
  onto the wire it came from. Absent for every other provider, and
  absent means absent: request bodies and session files are unchanged
  wherever it does not apply.
- **Error bodies wrapped in a JSON array are unwrapped.** Google sends
  one, so a 404 printed `api error (HTTP 404) api_error:` with no
  message at all, hiding the sentence that explained the failure.
- **Hosted template defaults repaired from live evidence.** The OpenAI
  template defaults to `gpt-4o` and bakes its 16384 completion cap,
  because the previous default was absent from a real account listing
  and because a fresh profile inheriting the global 32000 was rejected
  on every call. The Gemini template defaults to `gemini-3.6-flash`;
  the previous default is retired for new accounts.
- **Per-model context windows in the Anthropic template**, read off
  the authenticated models API rather than assumed: haiku serves 200k
  where the other three serve 1M, so one shared constant was wrong for
  whichever tier it did not match.
- `temur init` now says the key file path question out loud, so the
  key cannot be pasted where the path belongs.

## v0.11.0 - 2026-08-01

Launch-readiness documentation pass (T23; prose and layout only, no
code or behavior change):

- README rebuilt for a first-time reader: badge row, a short pitch,
  the nine-task weak-model eval as a table near the top
  (Qwen3-4B-Instruct-2507 Q4_K_M, 9/9, RUNBOOK pointer), Install and
  Quickstart within the first two screens, a minimal Configure, a
  short Untrusted hosts, and a "How this was built" section naming
  the AI-directed, gate-everything workflow plainly. The tag-pinned
  install lines are byte-identical. Deep reference material (full
  command list, session model, context lifecycle, configuration
  recipes, key isolation, untrusted-host practice) merged into
  docs/USAGE.md, deduplicated against what was already there.
- Root tidy: the machine-setup and v1-plan documents moved from the
  root into docs/ (now docs/SETUP.md and docs/IMPLEMENTATION_PLAN.md)
  with every live reference updated; the root now holds README,
  CHANGELOG, ROADMAP, LICENSE, CLAUDE.md, and the cargo manifests.
- Milestone codes moved out of user-facing lead lines: earlier
  release sections in this changelog now lead with the feature and
  keep the code parenthetical, and README and Cargo.toml prose drop
  bare codes; RUNBOOK record titles stay verbatim.
- New scripts/bump_version.sh: a stage-1 helper that rewrites the
  four-file version map (Cargo.toml, Cargo.lock, scripts/install.sh
  VERSION, the README tag pins) on a clean tree, prints the
  resulting diff, and never commits. release.sh gate 3 remains the
  authority on version skew.

## v0.10.0 - 2026-07-31

Context-window detection and discoverability (T22):

- `temur doctor` now checks `context_window` per profile. On keyless
  openai-compat profiles with network allowed it reads the server's
  real context allocation from llama.cpp's `/props` endpoint
  (`default_generation_settings.n_ctx`, the server's `-c` flag): a
  matching configured value PASSes; a mismatch WARNs naming both
  values and the consequence direction (configured larger than the
  allocation means the context advisory fires too late and requests
  can fail at the real limit; smaller is safe but early); a missing
  value WARNs with the exact config line to add. Non-llama.cpp servers
  answer nothing useful there and stay silent. Independently, any
  profile without a `context_window` gets a one-line NOTE that the
  context advisory and the context-scaled tool-output caps are off for
  it. Keyed profiles and `--no-network` are never probed; the probe
  takes only a base URL, so it is unauthenticated by construction, the
  second and last request under the keyless-GET amendment (RUNBOOK
  record).
- `temur init` (local template, fresh and `--add`): with the server
  up, the wizard now writes the detected allocation as
  `context_window` instead of the baked 8192, with a notice naming the
  value and its source; server down or not llama.cpp keeps the baked
  value, byte-identical to before. The anthropic template's four
  profiles now carry `"context_window": 200000` (knowledge-based, not
  detected; the `/models` enrichment below reads the real value off
  the wire). Existing configs are never rewritten.
- `/models` on an anthropic profile now uses the per-model
  `max_input_tokens` the listing response already carries: a
  configured `context_window` larger than the reported value draws a
  warning naming both, a missing one draws a hint naming the exact
  config line to add, equal or smaller stays silent. No new network
  calls, and the cached listing keeps the reported windows (still
  cleared on a provider change).

## v0.9.0 - 2026-07-30

Bash approval mode and untrusted-host riders (T21):

- With key files configured and no working bash key sandbox (kernels
  that deny unprivileged user namespaces), an interactive session (TUI,
  or plain REPL on a real terminal) now asks per-command approval
  instead of refusing bash outright: the prompt shows the exact
  command, `y` runs that one command unsandboxed, anything else denies
  it, and nothing is remembered between commands. A denial goes back to
  the model as an ordinary tool error, so the turn continues. A working
  sandbox is never preempted, keyless configs never ask, one-shot `-p`
  and piped runs never ask (they still refuse), and
  `allow_bash_without_key_sandbox` still runs plain and now silences
  the ask entirely. The refusal wording leads with the interactive ask
  and keeps the config override as the non-interactive answer.
- `temur init` now catches a key-shaped answer at the key file PATH
  question (no `/`, 20+ chars, all in `[A-Za-z0-9_-]`): the value is
  dropped, never used or stored, with a warning that keys are only
  accepted at the hidden prompt and that the pasted value reached the
  terminal and should be rotated. Interactive runs re-ask; piped runs
  fail closed.
- `temur doctor`'s sandbox-unavailable line now names all three
  outcomes (interactive ask, non-interactive refusal, config override)
  and points at the new README "Untrusted hosts" section, which covers
  spend-capped throwaway keys and the LiteLLM-style relay pattern over
  the existing per-profile `base_url`.
- Test-harness only: the headless TUI key pump is now readiness-gated
  (a scripted line starts only once the app is idle), fixing a
  pre-existing flake where a zero-delay Enter could race the deliberate
  busy-Enter drop; `App` key semantics are unchanged. New one-way test
  seam `TEMUR_TEST_SANDBOX_UNAVAILABLE` forces the sandbox probe to
  FAIL (never to succeed) so the approval arms are testable on hosts
  whose kernel supports the sandbox.

## v0.8.0 - 2026-07-30

Context lifecycle: living with small context windows (T20):

- New `/compact` command: one model call (tools omitted, the session's
  own model and system prompt) summarizes the conversation under
  structured headings, then the history becomes that summary plus a
  verbatim tail, the last user-initiated exchange, merged
  alternation-safe (the summary rides inside the tail's first user
  message as a leading text block). Fail-closed: any provider error,
  interrupt, or empty summary leaves history untouched. On success the
  context estimate resets, the advisory re-arms, session usage totals
  keep accumulating (including the summary call itself), todos stay,
  and the compacted state is persisted immediately, like `/clear`. The
  notice is honest that the next request rebuilds the provider's
  cached prefix once.
- The once-per-session context warning is now a unified advisory with
  two arms: it fires at 80% of `context_window` OR when the remaining
  window is smaller than `max_tokens`, whichever comes first, and its
  wording names both remedies (`/compact`, new session). A second
  trigger fires it immediately at `--continue`/`--resume`/`/resume`
  when the restored estimate already crosses the threshold, because
  resume is the zero-waste moment to compact.
- Prefix-stability invariant tests on both providers pin that requests
  are append-only (growing the history never rewrites earlier bytes,
  modulo the one moving Anthropic cache breakpoint), the property that
  makes provider prompt caching and llama.cpp `--cache-reuse` prefix
  KV reuse effective.

Model floor: raising the harness floor for weak local models (T19):

- Tool-output truncation now scales to the model's context window
  and keeps both ends: the per-result cap is `context_window`
  clamped to 4,000..30,000 chars (no configured window keeps the
  30,000 cap exactly as before), and truncation keeps the true head
  and true tail around a one-line marker that says how to narrow
  the command, so build errors at the end of long output survive.
  Key redaction still runs before truncation.
- `write` now enforces its prompt's read-first promise: overwriting
  an existing file this session has not seen (via read, edit, or a
  previous successful write) fails with a pointer to read or edit.
  New files are unaffected. `--continue`/`--resume` start with an
  empty read set deliberately: the file may have changed on disk.
- Binary-format nudge: the write prompt steers models to produce
  binary formats (xlsx, zip, png, gz, ...) with a small script run
  via bash instead of raw-writing corrupt bytes, and read's binary
  denial now names bash inspection tools (file, unzip -l, strings).
- Prose tool-call execution, a recorded narrow amendment of T4's
  "prose is never executed" policy: an end-of-turn message with no
  structured tool calls that IS one unambiguous tool call (exactly
  one candidate in a known shape, losslessly parsed JSON, registered
  tool, object arguments) executes through the same guarded registry
  path as a structured call; the result returns as plain user text
  and a notice announces it. Failed prose executions count toward
  the per-turn nudge cap. New config `prose_tool_calls` (default
  true); false restores detect+nudge exactly.
- `scripts/weak_model_eval.sh` grows task 8 (gzip binary nudge:
  gunzip validity proves the file was scripted, not raw-written)
  and task 9 (a needle on the LAST line of over-cap output, the
  live proof of the head+tail keep). The score line is now /9.

## v0.7.0 - 2026-07-29

Key isolation: guaranteeing tools cannot reach configured keys (T18):

- File guard for read/write/edit/glob/grep: every configured
  `api_key_file` (active selection and all named profiles) plus the
  `APP_SECRET_FILE` path is denied to tools, by canonical path
  (symlinks, unborn write targets), by parent-directory prefix
  (sibling keys in a secrets dir), and by device+inode identity
  (hardlinks, renames). grep never reads a protected file, glob never
  lists one, writes and creates under a secrets dir are refused.
  Denials are ordinary tool errors naming the policy, never key
  material.
- bash sandbox: with keys configured, bash runs in an unprivileged
  user namespace + private mount namespace with every existing key
  file bind-masked by /dev/null (reads empty, writes discarded, host
  file untouched). Kernels without unprivileged user namespaces make
  bash refuse instead, naming the new
  `allow_bash_without_key_sandbox` config override (default false;
  it never disables a working sandbox). Keyless configs spawn bash
  byte-identically to before: no namespace, no probe.
- Redaction: the active provider's key (the one credential actually
  read) is scrubbed from every tool result, successes and errors,
  before output truncation so a key cannot leak split across the
  30k cut; re-registered on `/model` switches, cleared on a switch
  to keyless. Keys shorter than 8 chars are never matched.
- `temur doctor` adds two offline lines: the key-isolation guard
  count (or a keyless note) and bash sandbox availability, WARNing
  when keys exist but no sandbox is possible (naming the refusal or
  the override), never affecting the exit code.

Provider onboarding (T17):

- `temur init --add <template>` merges a template into an EXISTING
  config as named profiles instead of overwriting it: `anthropic` adds
  the curated four-profile set (fable/haiku/opus/sonnet) sharing one
  key file, `openai`/`gemini`/`xai` add one profile named after the
  template, `local` adds a keyless `local` profile reusing the
  base-URL question and model picker. Surgical config edit: key order
  and unknown fields survive, the startup `profile` key is never
  touched, and ANY profile-name collision aborts the whole merge with
  the file untouched (every collision named). The cross-provider hop
  hint now names the command (`temur init --add anthropic sets one
  up`).
- New `xai` starter template: xAI Grok API over its OpenAI-compatible
  endpoint (`https://api.x.ai/v1`, default model `grok-4`), in both
  the fresh wizard and `--add`. Spec-written; live verification stays
  parked with T13 until keys exist.
- The init wizard (fresh and `--add`) can now take the API key at a
  hidden prompt right after creating, or finding, an EMPTY key file:
  input is never echoed (termios echo off behind an RAII restore,
  SIGINT held off for the read), Enter or EOF skips, and a pasted key
  lands only in the key file (mode 600) with a best-effort wipe of the
  in-memory buffer. This is a deliberate NARROW amendment of the T14
  "init never accepts key material" rule, recorded in the RUNBOOK T17
  amendment record; a non-empty key file is never touched, no other
  surface accepts key material, and there is no --key flag, env, or
  argv path.
- `temur doctor` adds a key-rotation reminder: a present, non-empty
  key file whose mtime is at least `key_rotate_warn_days` days old
  (new optional config field; default 90, 0 disables) gets a WARN
  suggesting a rotation and naming `temur init --add` as the
  re-prompt path. Metadata only, advisory only, never affects the
  exit code.

## v0.6.0 - 2026-07-28

Model-access footgun fixes (T16):

- Cross-provider hop: `/model <claude-* id>` on a non-anthropic
  provider with an anthropic profile configured now switches to that
  profile (the exact-model match, else the first anthropic profile by
  name) instead of setting an anthropic id on the wrong provider; when
  the id is not the profile's own model it is applied on top, and the
  notice names the mechanism and the profile. Escape hatches: an id
  the active provider itself listed in `/models` always switches
  literally, and with no anthropic profile the raw switch happens as
  before plus a hint that an anthropic profile enables the hop. The
  hop makes no network request. `--save` composes: the save site is
  the hop profile's `model`, and the persist notice now names the site
  profile whenever one is active.
- `temur init`, Anthropic template: writes a curated four-profile set
  (fable, haiku, opus, sonnet over the current Anthropic model tiers),
  every profile sharing the one key file the wizard asks for; the
  model question becomes a startup-profile question (number or name,
  default sonnet, anything else re-asks). The effective default model
  stays claude-sonnet-5.
- `/model` with no argument appends two hint lines after the profile
  list (what a non-profile argument does, where `/models` fits, that
  `--save` persists). A raw-id switch whose id is absent from the last
  `/models` listing gets an advisory notice; the switch stands. Cached
  listing ids are dropped when a switch changes the provider, so one
  provider's listing never completes or judges another's ids.
- init local template writes `max_tokens` 4096 (1024 truncated first
  real tasks); README and OFFLINE recipes updated in lockstep. The
  plain truncation notice now names the limit and its source
  (`max_tokens (4096, from profile "local")`, or `from config`) and
  says the fix. init's closing text and the first-run quickstart note
  that conversations are saved automatically per working directory and
  `temur --continue` resumes the last one.

Model-selection onboarding polish (T15):

- `temur init`, local template: a Base URL question (default
  `http://127.0.0.1:8080/v1`) now precedes the model question, and when
  the server answers, its own model listing prints as a numbered picker
  (capped at 20 shown; a number or a free-text id both work; default is
  the template default when listed, else the first listed id). With no
  server reachable: a one-line note, the old free-text question, and a
  short baked shortlist of known-good small models pointing at
  docs/OFFLINE.md "Recommended small models" (which stays canonical).
  Keyed templates are unchanged. A non-default base URL is written into
  the config; the default render stays byte-identical.
- `/model <model-id> --save` persists a raw-id switch to config.json
  after the switch succeeded; `/model --save` persists the currently
  active model. The write is a surgical serde_json::Value edit (never a
  round trip through the config struct): unknown fields and the user's
  key order survive, the file is written atomically (temp + rename),
  and the site is the active profile's `model`, `openai_compat.model`,
  or the top-level `model` key as appropriate. `--save` with a profile
  name is a clean error (the startup profile stays the hand-edited
  `profile` key). Persistence failure after a successful switch keeps
  the switch and says why the save failed. Replay-guarded like the
  other mutators.
- `temur doctor` now checks each keyless openai-compat selection's
  configured model against the server's listing: PASS when listed, WARN
  naming the model and up to 10 served ids when not (advisory, never a
  FAIL, since servers alias ids), a plain NOTE when the listing itself
  fails, and a SKIP line for keyed selections (that check would need an
  authenticated request). `--no-network` skips model checks like the
  probes.
- The single network capability all of this rides on is one new
  provider fn, `list_models_keyless(base_url, timeout)`: an
  unauthenticated GET of `{base}/models` with a 3s timeout that cannot
  attach auth headers or touch key files by construction. init and
  doctor call only this, never the authenticated listing path.
- First-run quickstart gains a pointer line to docs/OFFLINE.md
  "Recommended small models".
- Internal: serde_json's preserve_order feature is enabled so saves
  keep config key order; request bodies now serialize through a
  sorted-key step pinning the wire byte-identical to every release
  since T1 (the request_golden suite enforces it).

## v0.5.0 - 2026-07-28

Onboarding + one-shot mode (T14; built before T13, which awaits keys):

- First-run quickstart: running with no config file, no `--mock`, and no
  usable credential now prints guidance (the config path looked for,
  `temur init` / `temur doctor` pointers, a README pointer) and exits
  FAILURE, instead of the raw "secret: APP_SECRET_FILE is not set"
  error. Any existing config, `--mock` run, or launcher-style
  `APP_SECRET_FILE` run behaves byte-identically to before.
- One-shot mode: `-p <text>` / `--prompt <text>` runs exactly one full
  agentic turn (all tool rounds) on the plain path with no banner;
  assistant prose to stdout, tool/status chrome (and `--continue`/
  `--resume` backscroll) to stderr; exit SUCCESS on a completed turn,
  FAILURE on a provider or startup error. Composes with `--continue`,
  `--resume`, and `--mock` (persistence stays off there); mutually
  exclusive with `--tui`. Live one-shots save the session, so
  `temur -p` chains with `temur --continue -p`.
- `temur init`: line-based wizard (pipeable answers) with four
  templates: local llama.cpp/Ollama/LM Studio (keyless), Anthropic,
  OpenAI, Gemini (hosted pair via their OpenAI-compat endpoints);
  per-template model defaults; keyed templates get a key file path
  (default `~/.secrets/temur-<provider>-key`) created EMPTY, mode 600
  (parent dir 700 if created), paste-with-your-editor instruction.
  Refuses to overwrite an existing config unless `--force`; an existing
  key file is never touched. No key material ever passes through temur.
- `temur doctor`: read-only diagnosis, one PASS/WARN/FAIL line per
  check, exit SUCCESS iff no FAIL: config parse + the same eager
  validation as startup, active selection, key files by metadata only
  (missing/empty FAIL for the active selection, WARN for inactive
  profiles; group/other mode bits WARN), sessions dir writability, and
  one TCP-connect(+TLS-handshake for https) probe per distinct
  base_url, never an HTTP request. `--no-network` skips probes. With no
  config, FAILs with the quickstart pointer.
- tests/cli.rs: new black-box suite spawning the real binary with
  isolated XDG dirs (exit codes, stdout/stderr split, wizard piping,
  key-file metadata); check.sh mounts the bin dir so the suite also
  runs in both containers.
- One-shot exit codes completed: an interrupted one-shot (Ctrl+C)
  now exits 130 (128+SIGINT, the shell convention), and interruption
  wins over a raced error. The full contract: 0 completed turn, 1
  provider or startup error, 130 interrupted; verified end-to-end by
  an event-driven SIGINT test in tests/cli.rs.
- Usage docs: new docs/USAGE.md (a worked interactive session,
  one-shot scripting recipes with the exit-code contract, the skills
  contract with a minimal working example; all transcripts from real
  local-model runs), an audience note atop SETUP.md separating the
  dev-machine recipe from installing/using temur, and README links to
  USAGE.md and TUI.md.

CI (T12):

- Two-tier GitHub Actions CI (first-party actions only). Tier 1 on
  every push to main: a hermetic test job (cargo build + full suite +
  forbidden-dep scan) and a release-gate job running the real
  release.sh (generic leak scan over files and full history, skew
  gate, 4-target static build with per-target asserts, SHA256SUMS)
  with staged artifacts uploaded for 7 days. Tier 2 on manual
  dispatch: the full check.sh under rootless podman, verified green
  in a live run.
- check.sh: target dir and TUI smoke log dir are now env-overridable
  (`TEMUR_TARGET_DIR`, `TEMUR_CHECK_TMP`); defaults are
  behavior-identical, no other script changes.
- SETUP.md: recorded the T7-era cross-toolchain additions (ARM cross
  compilers, qemu-user-static, the three extra rustup targets) that
  the original stages predated.

## v0.4.0 - 2026-07-27

Multi-model ergonomics (T11):

- `scripts/serve.sh start [model]` selects a `.gguf` from `MODELS_DIR`
  by name: exact basename beats unique substring, case-insensitive;
  zero or several matches fail and list every candidate with its size.
  The no-argument lone-gguf auto-default stays; its failure now lists
  candidates too. `MODEL_GGUF` remains an explicit override and
  conflicts with a name argument.
- serve.sh RAM fit warning (advisory only): model file size plus a
  generous context allowance (128 KiB per context token) checked
  against `MemAvailable` before start; a single WARN line, then the
  start proceeds. `MEMINFO` knob makes the check testable.
- Compact bash prompt gained one sentence steering models to bash for
  file operations no dedicated tool covers (delete, move, copy, chmod):
  closes the observed qwen3-1.7b gap where "delete the file" was
  refused for lack of a delete tool.
- weak_model_eval.sh task 7, indirect-delete: "delete the file" naming
  no tool; PASS requires the file gone AND a bash rm call in the
  transcript. SCORE is now N/7.
- docs: expanded Ollama recipe (profile example, /models note), new
  LM Studio recipe including WSL2-to-Windows-host networking, serve.sh
  selection and RAM warn docs, and the small-model shortlist table
  (file size, est. RAM at 8k ctx, tool calls, indirect selection,
  verification status).

## v0.3.0 - 2026-07-26

Command ergonomics (T9):

- Per-profile prompt profiles: a profile's `prompt_profile` overrides the
  global one (own > global > full), validated at startup; `/model`
  switches swap the system prompt and tool descriptions atomically;
  `/status` gained the `prompt: full|compact` field.
- `/models` lists model ids from the active provider (live GET, both
  wire families); `/model <model-id>` switches to a raw id WITHIN the
  active provider (profile names win on collision; endpoint,
  credentials, limits, and prompt profile stay).
- TUI command styling and completion: `/`-input renders in the cyan
  accent, the status row live-hints the command being typed from the
  COMMANDS table, and Tab/BackTab cycle completions in place.
- `scripts/serve.sh start` defaults `MODEL_GGUF` when `MODELS_DIR`
  (default `$HOME/models`) holds exactly one `.gguf`; zero or several
  files fail with the searched dir and count.

Session management (T10):

- Named multi-session per project: the default session keeps the exact
  pre-T10 filename; `/new <name>` creates named sibling sessions
  (names keep `[A-Za-z0-9._-]`, cap 32; the file appears on the first
  turn's save).
- `/sessions` lists every saved session across projects (name, recorded
  cwd, message count, file name, derived title, active marker, newest
  first); `/resume <key>` switches by session name or file-name prefix
  with load-first atomicity (a failed resume changes nothing) and a
  cross-project advisory; `--resume <key>` does the same at startup.
- Resuming renders the saved history into the transcript as backscroll
  (prompts, replies, tool names) in both UIs; `--continue` now shows
  the same backscroll.
- Session format unchanged: FORMAT_VERSION stays 1, filenames and the
  FNV digest are untouched, compatibility holds in both directions
  (pre-T10 files load as the default session; default-session files
  stay byte-identical to the pre-T10 shape).

## v0.2.0 - 2026-07-26

Daily-driver UX (T8):

- Slash commands (`/help`, `/status`, `/model`, `/clear`, `/thinking`)
  and named config profiles with atomic in-session `/model` switching.
- Markdown rendering for assistant prose in the TUI (pulldown-cmark,
  terminal renderer) plus the formalized monochrome style contract
  (DIM/BOLD/ITALIC/UNDERLINED and exactly three accents).
- `scripts/serve.sh start|stop|status`: background llama.cpp server
  launcher, loopback-only publish, pinned image, never auto-pulls.
- check.sh hygiene: host-side invocations run with isolated XDG dirs;
  `tests/sigint.rs` joined the container suites on both paths.

## v0.1.1 - 2026-07-23

Ten review fixes from the post-v0.1.0 code review:

- Edit correctness: block-anchor matching now requires the expected
  offset or a similarity floor (silent mis-splice fixed), and fuzzy
  splices keep the FILE's indentation, not the model's.
- Portable installer verify: the GNU-only `sha256sum` flags that broke
  busybox/Alpine replaced with a portable check; `install_test.sh`
  matrix added.
- Interruption correctness: plain-REPL SIGINT bridges into the cancel
  token (first Ctrl+C interrupts, second force-quits), bash kills its
  whole process group, real provider errors are no longer swallowed by
  an interrupt race, thinking-only landings persist nothing, and the
  cancel token clears at submission (not at turn entry).
- Cleanups: matcher precompute, one Session constructor, one builder
  for the synthesized interrupt marker.

## v0.1.0 - 2026-07-23

Initial release:

- Agent loop with seven tools plus the skill tool, doom-loop and
  weak-model guards, session persistence with `--continue`, turn
  interruption (Esc / Ctrl+C).
- Providers: Anthropic and OpenAI-compatible (keyless local servers
  first-class), provider-neutral history, pure-Rust TLS (rustls/ring).
- UIs: ratatui TUI and plain line REPL.
- Packaging: static musl release builds for i686, x86_64, aarch64, and
  armv7 (hard-float), with checksum-verified installer.
