# Changelog

Newest first. Dates are release dates; "Unreleased" ships next.

## Unreleased

T20 context lifecycle (living with small context windows):

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

T19 model floor (raising the harness floor for weak local models):

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

T18 key isolation (guaranteeing tools cannot reach configured keys):

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

T17 provider onboarding:

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

T16 model-access footgun fixes:

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

T15 model-selection onboarding polish:

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

T14 onboarding + one-shot mode (built before T13, which awaits keys):

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

T12 CI:

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

T11 multi-model ergonomics:

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

T9 command ergonomics:

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

T10 session management:

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

T8 daily-driver UX:

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
