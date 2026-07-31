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

**Weak-model floor, measured.** `scripts/weak_model_eval.sh` runs nine fixed
agent tasks (write, read-and-extract, targeted edit, bash, multi-file
search, edit-then-bash chain, indirect delete, gzip binary nudge,
large-output tail), each scored only by host-verified filesystem
assertions. Recorded scores: **5/6** on the original six tasks with
Qwen3-1.7B Q4_K_M (a 1.1 GB model), and the current nine-task score in
docs/RUNBOOK.md "T19 acceptance"; both through the compact prompt profile,
llama.cpp `server-b10068`, 8192-token context, in a `--network none` pod.

The harness floor itself (T19, active on every provider): tool output
over the per-result cap keeps its true head AND tail around a narrowing
marker instead of losing the end (the cap scales to `context_window`,
clamped 4,000..30,000 chars); `write` refuses to overwrite a file the
session has not read (the prompt's long-standing promise, now enforced);
prompts steer binary formats to scripted `bash` runs instead of corrupt
raw writes; and a tool call written as plain text executes when it is
one unambiguous, losslessly parsed call to a real tool (config
`prose_tool_calls`, default true; `false` restores nudge-only).

## Install

Prebuilt static binaries ship for `x86_64`, `aarch64`, `armv7` (hard-float,
Raspberry Pi 2/3+ and other 32-bit ARM userlands), and `i686` (SSE2
required). Because they are musl-static they run on any Linux distro,
including Alpine and other musl systems, no glibc needed.

One-liner (detects your arch, downloads, verifies the checksum, installs to
`~/.local/bin`; refuses to install anything unverified):

```sh
curl -fsSL https://raw.githubusercontent.com/thekeoni1/Temur/v0.9.0/scripts/install.sh | sh
```

Piping to `sh` is a trust decision: [read the script
first](https://github.com/thekeoni1/Temur/blob/v0.9.0/scripts/install.sh) if
you prefer. The checksum step defends against transport corruption and a
mismatched artifact; it is not a substitute for trusting the release source,
since the sums come from the same place as the binaries.

### Update

To update an existing install, re-run the install one-liner taken from
the latest release page; it overwrites `~/.local/bin/temur` in place.
The one-liner is tag-pinned, so a copy saved from an old release keeps
installing that old version; always copy it fresh from the latest
release page. `temur --version` shows what is currently installed.

Manual install (example: x86_64; substitute your triple):

```sh
curl -fsSLO https://github.com/thekeoni1/Temur/releases/download/v0.9.0/temur-v0.9.0-x86_64-unknown-linux-musl
curl -fsSLO https://github.com/thekeoni1/Temur/releases/download/v0.9.0/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
install -m 755 temur-v0.9.0-x86_64-unknown-linux-musl ~/.local/bin/temur
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

`temur init` offers five templates: local llama.cpp / Ollama / LM Studio
(keyless), Anthropic, OpenAI, Gemini, and xAI Grok (the hosted three
through their OpenAI-compatible endpoints). The local template asks where the server
lives (default `http://127.0.0.1:8080/v1`) and, when one answers, lists
the models it actually serves so you pick by number instead of typing an
id blind; with no server reachable it falls back to the free-text
question plus a short baked list of known-good small models
([docs/OFFLINE.md](docs/OFFLINE.md), section "Recommended small models",
stays the full table). The Anthropic template writes a four-profile
set (fable, haiku, opus, sonnet over the current model tiers) sharing
one key file, and asks which profile to start on (number or name,
default sonnet). For keyed templates it asks for a key file
path (default `~/.secrets/temur-<provider>-key`), creates that file
EMPTY with mode 600, then offers to take the key at a hidden prompt
(input never echoed; Enter skips) and otherwise tells you to paste it
in with your editor. That one wizard prompt is a narrow, documented
amendment (docs/RUNBOOK.md, T17 amendment record): outside it, temur
never accepts key material, and it never reads back, echoes, or stores
it, in any direction. `temur doctor` then verifies the setup: config parse and
validation, the key file by metadata only (present, non-empty by size,
mode 600, WARN on group/other bits), sessions dir writability, one
TCP-connect/TLS-handshake reachability probe per endpoint, and, for
keyless local endpoints only, whether each configured model is in the
server's own listing (an unauthenticated GET; a mismatch is a WARN
naming what the server serves, since servers alias ids). `--no-network`
skips the probes and the model checks. Running `temur` with no config
at all prints these pointers instead of a raw credential error.

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
of the recipes below for you. The hosted OpenAI, Gemini, and xAI
templates are written to their published compat specs but not yet
live-verified against those endpoints (that verification is a parked
milestone awaiting keys). The minimal keyless setup
against a local llama.cpp server (`base_url` defaults to
`http://127.0.0.1:8080/v1`):

```json
{
  "provider": "openai-compat",
  "max_tokens": 4096,
  "openai_compat": { "model": "qwen3-1.7b", "context_window": 8192 }
}
```

Server setup for llama.cpp, Ollama, and LM Studio (including reaching a
Windows-host server from WSL2), LAN topology, recommended small
models, and the compact prompt profile: [docs/OFFLINE.md](docs/OFFLINE.md).
From a checkout, `scripts/serve.sh start|stop|status` runs the containerized
llama.cpp server detached in the same terminal (details in OFFLINE.md).

The Anthropic template writes a curated profile set over the current
model tiers, every profile reading the same key file, and asks which
profile to start on (default `sonnet`, keeping `claude-sonnet-5` as the
effective default model):

```json
{
  "profiles": {
    "fable":  { "provider": "anthropic", "model": "claude-fable-5",
                "api_key_file": "/home/you/.secrets/temur-anthropic-key" },
    "haiku":  { "provider": "anthropic", "model": "claude-haiku-4-5",
                "api_key_file": "/home/you/.secrets/temur-anthropic-key" },
    "opus":   { "provider": "anthropic", "model": "claude-opus-5",
                "api_key_file": "/home/you/.secrets/temur-anthropic-key" },
    "sonnet": { "provider": "anthropic", "model": "claude-sonnet-5",
                "api_key_file": "/home/you/.secrets/temur-anthropic-key" }
  },
  "profile": "sonnet"
}
```

The hosted OpenAI-compatible templates share one shape and differ only
in endpoint and default model; the xAI one, for instance (OpenAI:
`https://api.openai.com/v1` / `gpt-4o-mini`; Gemini:
`https://generativelanguage.googleapis.com/v1beta/openai` /
`gemini-2.5-flash`):

```json
{
  "provider": "openai-compat",
  "openai_compat": { "base_url": "https://api.x.ai/v1",
                     "model": "grok-4",
                     "api_key_file": "/home/you/.secrets/temur-xai-key" }
}
```

The default provider is `anthropic` (model `claude-sonnet-5`); any API key
is read from a file path at startup, never from env or argv.

Two more optional keys: `sessions_dir` overrides where saved sessions live
(default: the state dir, see below), and `session_max_bytes` caps the saved
session file's size (default 4 MiB, minimum 64 KiB).

### Adding a provider

`temur init --add <local|anthropic|openai|gemini|xai>` merges a
template into your EXISTING config as named profiles, leaving every
other setting, the startup `profile` key included, untouched:
`anthropic` adds the four-profile set above sharing one key file;
`openai`, `gemini`, and `xai` each add one profile named after the
template; `local` adds a keyless `local` profile through the same
base-URL question and model picker as the fresh wizard. A name
collision with any existing profile aborts the whole merge with the
file untouched. Afterwards `/model <name>` switches to the new
profile; set `"profile": "<name>"` in config.json to make it the
startup default.

For keyed templates the wizard (fresh or `--add`) creates the key
file empty (mode 600), then offers a hidden paste prompt: input is
never echoed, Enter skips, and a pasted key is written only to the
key file. A non-empty existing key file is never prompted for or
touched. As a rotation reminder, `temur doctor` WARNs when a key
file has not changed in `key_rotate_warn_days` days (optional config
field; default 90, `0` disables); re-running `temur init --add`
re-prompts after you rotate the key at the provider.

### Named profiles and in-session switching

Define named profiles (nicknames bundling provider + model + endpoint +
key file + limits) and switch between them from inside a session with
`/model <name>`, no quit-and-edit-JSON round trip:

```json
{
  "profiles": {
    "local":  { "provider": "openai-compat", "model": "qwen3-1.7b",
                "max_tokens": 4096, "context_window": 8192 },
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
- `/model` - list profiles, then two hint lines saying what a
  non-profile argument does · `/model <name>` - switch profiles
  mid-session · `/model <model-id>` - switch the model WITHIN the
  active provider (profile names win on collision; endpoint,
  credentials, limits, and prompt profile stay; a bad id surfaces as
  the provider's error on the next turn; if the id is absent from the
  last `/models` listing an advisory notice says so, without blocking).
  Exception - the cross-provider hop: a `claude-*` id on a
  non-anthropic provider with an anthropic profile configured switches
  to that profile instead (the exact-model match, else the first
  anthropic profile by name), then applies the id on top when it is
  not the profile's own model; the notice names the profile. An id the
  active provider actually listed in `/models` always switches
  literally, and with no anthropic profile a hint notice explains the
  hop. · `/model <model-id> --save` - the same switch, persisted to
  config.json on success (a surgical edit: your key order and unknown
  fields survive; when a profile is active - including one a hop just
  activated - the save site is that profile's `model` and the notice
  names it) · `/model --save` - persist the currently active model;
  `--save` with a profile name is an error (the startup profile stays
  the hand-edited `profile` key)
- `/models` - list model ids from the active provider (live GET; ids
  feed `/model` Tab completion in the TUI)
- `/clear` - wipe the session; the empty state is persisted immediately,
  so quitting and `--continue` resumes empty
- `/compact` - one model call summarizes the conversation, then the
  session continues from that summary plus the last user-initiated
  exchange kept verbatim (fail-closed: any error, interrupt, or empty
  summary leaves history untouched; the compacted state is persisted
  immediately, like `/clear`)
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

## Context lifecycle

With a `context_window` configured, temur tracks an advisory estimate
of context use (the last response's reported input+output tokens) and
warns once per session when the conversation gets tight: at 80% of the
window, or when the remaining room is smaller than `max_tokens`,
whichever comes first. The advisory names both remedies: `/compact`
summarizes the conversation and continues in a fraction of the window;
a new session starts clean. The same advisory also fires immediately at
`--continue`/`--resume`/`/resume` when the restored session is already
past the threshold, and resume is in fact the cheapest moment to
compact: no provider cache or local KV state is warm yet, so the
summarization throws away nothing. Requests are append-only by design
(pinned by a prefix-stability test suite), which is what makes provider
prompt caching effective: the anthropic provider marks cache
breakpoints (system+tools, plus a moving one at the end of history),
and against local llama.cpp the same append-only shape makes prefix KV
reuse work for free (start the server with `--cache-reuse 256` to keep
prompt processing incremental across turns). `/compact` deliberately
invalidates that warm prefix once, in exchange for a small history from
then on; per-turn trimming, which would invalidate it on every turn, is
deliberately absent.

## Key isolation

Tools run in the same process, as the same user, as temur itself, so
file modes alone cannot keep the model away from API keys: anything the
key-owning user can read, a shell command could too. Three layers close
that hole, on by default whenever any key file is configured:

- **File guard** (read, write, edit, glob, grep). Every configured
  `api_key_file` (the active selection and every named profile) plus the
  `APP_SECRET_FILE` path is protected. A tool path is denied when it
  resolves to a protected file (symlinks and not-yet-existing write
  targets are canonicalized first), when it lies under a protected
  file's parent directory (a secrets directory holds sibling keys), or
  when it shares the file's device and inode identity (hardlinks,
  renames). grep never reads a protected file, glob never lists one,
  and writes are denied too: overwriting a key is destruction and a
  poisoning vector.
- **bash sandbox.** With keys configured, every bash command runs in an
  unprivileged user namespace plus a private mount namespace where each
  existing key file is bind-masked with `/dev/null`: inside the shell
  the key path reads as empty and writes to it are discarded, while the
  host file stays untouched. On kernels without unprivileged user
  namespaces, an interactive session (the TUI, or the plain REPL on a
  real terminal) asks you to approve each bash command before running
  it unsandboxed, showing the exact command; the default answer is no,
  and nothing is remembered between commands. Non-interactive runs
  (one-shot `-p`, piped stdin) refuse to run bash instead. Setting
  `allow_bash_without_key_sandbox` to `true` in `config.json` accepts
  running bash unsandboxed WITHOUT asking, for non-interactive use;
  that is a real risk (an unsandboxed shell can read anything you can),
  the other layers still apply, and a working sandbox is always used
  when available, silencing both the ask and the override.
- **Redaction.** The ACTIVE provider's key, the one credential temur
  has actually read, is scrubbed from every tool result (successes and
  errors, before output truncation), so even an unexpected leak path
  cannot echo it back to the model.

The invariant: a keyless config behaves byte-identically to earlier
releases. No guard, no namespace, no probe, no redaction.

Honest limits: the identity check knows a key's identity only while the
file exists at its configured path, so a hardlink made beforehand
escapes it if the key file itself is later removed; redaction covers
the active key only (inactive profiles' keys are never read, so there
is nothing to redact them with); a masked write inside the bash sandbox
is discarded silently rather than reported; and the parent-directory
rule means a key file placed in a broad directory (a home directory, a
project root) blocks tool access to that entire directory. Keep key
files in their own directory, as `temur init` sets up.

`temur doctor` reports the guard count and the sandbox availability,
and warns when bash would need approval or refuse.

## Untrusted hosts

Ephemeral playgrounds, throwaway VMs, and shared machines deserve more
suspicion than your own workstation: anything that reaches the host
root user, a snapshotting hypervisor, or another user with your file
access can read whatever key you place there, and temur's key isolation
only guards against the MODEL, not against the host.

- **Never place a primary key on a host you do not control.** Use a
  dedicated key with a spend cap, rotate it on a schedule, and revoke
  it when the machine goes away. `temur doctor` warns when a key file
  has not been rotated in `key_rotate_warn_days` (default 90).
- **The durable pattern is a relay you control.** Run a small
  OpenAI-compatible proxy (LiteLLM is the common choice) on a machine
  you trust, holding the real provider key. Point the playground
  profile's `base_url` at the relay and give the playground only a
  revocable virtual key with its own budget. The existing
  `openai-compat` provider and per-profile `base_url` support this
  unchanged; the untrusted host never sees the real credential, and
  killing the virtual key ends its access without touching anything
  else.
- **Locked-down kernels.** Playground containers often deny
  unprivileged user namespaces, so the bash key sandbox cannot start.
  Interactive sessions then ask per-command approval (see Key
  isolation above); for non-interactive use on such a host, either
  accept `allow_bash_without_key_sandbox` (with a throwaway key only)
  or leave bash refusing and rely on the other tools.
- **Paste carefully.** `temur init` never accepts a key at the file
  PATH question; a key-shaped answer there is dropped with a warning
  to rotate, because the value reached the terminal. Keys go in only
  at the hidden prompt, or into the key file with your editor.

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
