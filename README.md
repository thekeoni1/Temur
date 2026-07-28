# temur

A dependency-free single static binary coding agent for any Linux system,
down to 32-bit and embedded, that runs fully offline against local models.

Mainstream Bun- and Node-based coding agents publish no 32-bit x86 or armv7 builds, and
their "single executable" bundles embed a runtime on the order of 90 MB;
temur's release binary is a ~5 MB statically linked ELF with no interpreter
and no shared-library dependencies, so it loads on old x86 machines, armv7
industrial controllers, OpenWrt-class devices, `FROM scratch` containers,
and rescue/initramfs environments. Offline is a first-class mode, not a
degraded one: the OpenAI-compatible provider runs keyless against llama.cpp,
Ollama, or LM Studio, and quirky-local-server behavior (absent usage, missing tool-call
IDs, malformed argument JSON) is defined, tested degradation. The agent loop
is hardened for small models: schema-error feedback with bounded retries,
detection of tool calls emitted as prose, a compact prompt profile sized for
small context windows, and how well that works is measured by a scripted
eval rather than asserted. The honest topology: no useful LLM runs *on* a
32-bit box; temur runs on the constrained device where the code lives, and
the model serves from a capable machine on the same LAN or the same host.

## Proven, not claimed

**Static binary.** `scripts/check.sh` gates every change on the
`i686-unknown-linux-musl` release build being static, runs the test suites
and REPL/TUI smokes against that binary in a container, and repeats the
smoke in a bare `busybox` container where a dynamic binary could not load:

```
$ readelf -l temur | grep -c INTERP
0
$ readelf -d temur
There is no dynamic section in this file.
$ file temur
ELF 32-bit LSB executable, Intel 80386, version 1 (GNU/Linux),
statically linked, stripped
```

**Zero-internet operation.** `scripts/offline_demo.sh` creates a podman pod
with `--network none`, asserts the negative first (a TLS probe to the
internet MUST fail inside the pod), then requires the model to drive a real
`bash` tool call whose output file is verified from the host: model prose
is never accepted as evidence. Recorded pass: llama.cpp `server-b10068`
serving Qwen3-1.7B Q4_K_M, first attempt.

**Weak-model floor, measured.** `scripts/weak_model_eval.sh` runs six fixed
agent tasks (write, read-and-extract, targeted edit, bash, multi-file
search, edit-then-bash chain), each scored only by host-verified filesystem
assertions. Recorded score: **5/6** with Qwen3-1.7B Q4_K_M (a 1.1 GB model)
through the compact prompt profile, llama.cpp `server-b10068`, 8192-token
context, in a `--network none` pod. The one failure is documented as a model
capability floor (it batched a read and a dependent write in one parallel
call), not excluded from the score.

## Install

Prebuilt static binaries ship for `x86_64`, `aarch64`, `armv7` (hard-float,
Raspberry Pi 2/3+ and other 32-bit ARM userlands), and `i686` (SSE2
required). Because they are musl-static they run on any Linux distro,
including Alpine and other musl systems, no glibc needed.

One-liner (detects your arch, downloads, verifies the checksum, installs to
`~/.local/bin`; refuses to install anything unverified):

```sh
curl -fsSL https://raw.githubusercontent.com/thekeoni1/Temur/v0.4.0/scripts/install.sh | sh
```

Piping to `sh` is a trust decision: [read the script
first](https://github.com/thekeoni1/Temur/blob/v0.4.0/scripts/install.sh) if
you prefer. The checksum step defends against transport corruption and a
mismatched artifact; it is not a substitute for trusting the release source,
since the sums come from the same place as the binaries.

Manual install (example: x86_64; substitute your triple):

```sh
curl -fsSLO https://github.com/thekeoni1/Temur/releases/download/v0.4.0/temur-v0.4.0-x86_64-unknown-linux-musl
curl -fsSLO https://github.com/thekeoni1/Temur/releases/download/v0.4.0/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
install -m 755 temur-v0.4.0-x86_64-unknown-linux-musl ~/.local/bin/temur
```

Build from source (any Rust-supported target, e.g. pre-armv7 ARM): the
musl-static recipe is checked into `.cargo/config.toml`: rust-lld links
against the toolchain's bundled musl, and the host or cross `gcc` compiles
ring's C, no musl-gcc or musl-tools package needed.

```sh
rustup target add i686-unknown-linux-musl
cargo build --release --target i686-unknown-linux-musl
```

## Quickstart

From installed to a first conversation:

```sh
temur init      # guided starter config (answers can be piped)
temur doctor    # read-only check of the config and environment
temur           # TUI on a terminal; plain line REPL when piped
```

`temur init` offers four templates: local llama.cpp / Ollama / LM Studio
(keyless), Anthropic, OpenAI, and Gemini (the latter two through their
OpenAI-compatible endpoints). For keyed templates it asks for a key file
path (default `~/.secrets/temur-<provider>-key`), creates that file
EMPTY with mode 600, and tells you to paste the key in with your editor:
temur never accepts, reads back, echoes, or stores key material, in any
direction. `temur doctor` then verifies the setup: config parse and
validation, the key file by metadata only (present, non-empty by size,
mode 600, WARN on group/other bits), sessions dir writability, and one
TCP-connect/TLS-handshake reachability probe per endpoint, without
sending any API request (`--no-network` skips the probes). Running
`temur` with no config at all prints these pointers instead of a raw
credential error.

One-shot mode runs exactly one full agentic turn (tool calls included)
and exits: assistant prose on stdout, tool and status chrome on stderr,
exit code by outcome (0 completed turn, 1 provider or startup error,
130 interrupted with Ctrl+C, the shell convention for SIGINT), so it
composes in shell pipelines:

```sh
temur -p "Summarize what this repo does"
temur --continue -p "Now list the main risks"   # chained: same session
```

Live one-shots save the session like interactive runs, which is what
makes `--continue -p` chains work.

A fuller tour of day-to-day use (a worked interactive session, one-shot
scripting recipes, skills): [docs/USAGE.md](docs/USAGE.md).

## Configure

Config lives at `~/.config/temur/config.json`; `temur init` writes any
of the recipes below for you. The hosted OpenAI and Gemini templates
are written to their published compat specs but not yet live-verified
against those endpoints (that verification is a parked milestone
awaiting keys). The minimal keyless setup
against a local llama.cpp server (`base_url` defaults to
`http://127.0.0.1:8080/v1`):

```json
{
  "provider": "openai-compat",
  "max_tokens": 1024,
  "openai_compat": { "model": "qwen3-1.7b", "context_window": 8192 }
}
```

Server setup for llama.cpp, Ollama, and LM Studio (including reaching a
Windows-host server from WSL2), LAN topology, recommended small
models, and the compact prompt profile: [docs/OFFLINE.md](docs/OFFLINE.md).
From a checkout, `scripts/serve.sh start|stop|status` runs the containerized
llama.cpp server detached in the same terminal (details in OFFLINE.md).

The default provider is `anthropic` (model `claude-sonnet-5`); any API key
is read from a file path at startup, never from env or argv.

Two more optional keys: `sessions_dir` overrides where saved sessions live
(default: the state dir, see below), and `session_max_bytes` caps the saved
session file's size (default 4 MiB, minimum 64 KiB).

### Named profiles and in-session switching

Define named profiles (nicknames bundling provider + model + endpoint +
key file + limits) and switch between them from inside a session with
`/model <name>`, no quit-and-edit-JSON round trip:

```json
{
  "profiles": {
    "local":  { "provider": "openai-compat", "model": "qwen3-1.7b",
                "max_tokens": 1024, "context_window": 8192 },
    "sonnet": { "provider": "anthropic", "model": "claude-sonnet-5",
                "max_tokens": 32000 }
  },
  "profile": "local"
}
```

Optional `profile` picks the startup profile; omit it and the base
provider/model fields apply exactly as before profiles existed. Profile
fields: `provider` (`"anthropic"` or `"openai-compat"`), `model`
(required), and optional `base_url` (default: the provider's own default
endpoint), `api_key_file` (path to a key file: openai-compat profiles
without one are keyless, anthropic profiles without one fall back to
`APP_SECRET_FILE`), `max_tokens` (default: the global value),
`context_window`, and `prompt_profile` (`"full"` or `"compact"` for
THIS profile; default: the global `prompt_profile` - switching between
profiles swaps the system prompt and tool descriptions accordingly, and
an explicit `system_prompt` still wins in either profile). Every
profile is validated at startup, so `/model` can
only fail on a credential/IO problem, and a failed switch leaves the
session untouched. History continues across a switch (it is stored
provider-neutrally), and each save records whichever provider/model is
active at that moment.

## Commands

Inside a session, any input line starting with `/` is a command: it
never reaches the model or the history (which also means a literal
message starting with `/` cannot be sent):

- `/help` - list commands
- `/status` - profile, provider, model, thinking, prompt profile,
  context use, session file
- `/model` - list profiles · `/model <name>` - switch profiles
  mid-session · `/model <model-id>` - switch the model WITHIN the
  active provider (profile names win on collision; endpoint,
  credentials, limits, and prompt profile stay; a bad id surfaces as
  the provider's error on the next turn)
- `/models` - list model ids from the active provider (live GET; ids
  feed `/model` Tab completion in the TUI)
- `/clear` - wipe the session; the empty state is persisted immediately,
  so quitting and `--continue` resumes empty
- `/sessions` - list every saved session, all projects: name (or
  `(default)`), the directory it was recorded in, message count, file
  name, and a title derived from its first prompt; the active session
  is starred
- `/resume <session>` - switch to a saved session by name or file-name
  prefix; the saved history renders into the transcript as backscroll
- `/new <name>` - start a fresh named session for this project (the
  file is created on the first turn)
- `/thinking` · `/thinking on|off` - show or flip adaptive thinking for
  this session (only the anthropic provider uses it)

Under `--mock`/`--capture-sse` the state-mutating commands, and
`/models`, which is a live network GET, report themselves unavailable
to keep replays deterministic.

In the TUI (the default on a terminal; design notes and key bindings in
[docs/TUI.md](docs/TUI.md)), assistant replies render as
markdown (headings, emphasis, lists, quotes, links, and code blocks
behind a dim gutter) in the same monochrome, default-terminal-color
style; the plain REPL prints raw text unchanged. TUI command
ergonomics: `/`-input renders in the cyan accent, the status row shows
a live hint for the command being typed, and Tab cycles completions
in place (command names; profile names and `/models`-cached ids after
`/model`; `/sessions`-cached session keys after `/resume`; `on|off`
after `/thinking`) with BackTab reversing.

## Sessions

Every live run saves the conversation after each turn, under
`$XDG_STATE_HOME/temur/sessions/` (fallback
`~/.local/state/temur/sessions/`; state, not config, because transcripts
carry tool output and grow to megabytes). Each working directory has a
**default session**, plus any number of **named sessions** created with
`/new <name>` (names keep `[A-Za-z0-9._-]` and cap at 32 chars). A plain
start uses the default session; `temur --continue` resumes it.

`/sessions` lists everything saved, across all projects, newest first.
`/resume <key>`, or `temur --resume <key>` at startup, switches to a
saved session: a key is a session name (a name in the current project
wins; a globally-unique name works from anywhere; a duplicated one is an
error listing the candidates) or a file-name prefix, which is how
default sessions are addressed. Resuming renders the saved history into
the transcript as backscroll (prompts, replies, and tool names - tool
output and arguments are not replayed) and redirects saving to the
resumed file. Resuming another project's session warns that tools still
run in the current directory. A failed `/resume` (unknown key,
ambiguous key, unreadable file) changes nothing.

The saved history is provider-neutral, so a session recorded against
one provider resumes against another. Saves are atomic (write, fsync,
rename) and the FORMAT contains no timestamps, so a power cut at any
instant leaves the previous complete file, resumable on a clock-less
device. The `/sessions` listing order (newest first) comes from
filesystem mtimes, which is display-only metadata read at list time: on
a clock-less device every file sorts equal and the listing falls back
to name order, and nothing else depends on it. Past the size cap the
file drops its oldest exchanges, always cutting at a message boundary
that keeps the remainder replayable; the in-memory conversation is
never trimmed. Two processes in one directory don't corrupt anything:
last complete writer wins. To start over, `/new` a fresh name or delete
the file from the sessions dir.

## Scope

temur deliberately does not do LSP, MCP, IDE plugins, web UI,
server/multi-client mode, or a plugin ecosystem: each adds dependency and
maintenance surface (several would threaten the static-musl constraint) and
none serves constrained, offline, or weak-model use. Small surface is a
feature.

## Attribution

The tool prompt texts are ported near-verbatim from
[sst/opencode](https://github.com/sst/opencode) v1.2.25 (MIT).

License: MIT
