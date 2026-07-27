# Changelog

Newest first. Dates are release dates; "Unreleased" ships next.

## Unreleased - v0.4.0

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
