# temur

A dependency-free single static binary coding agent for any Linux system —
down to 32-bit and embedded — that runs fully offline against local models.

Mainstream Bun- and Node-based coding agents publish no 32-bit x86 or armv7 builds, and
their "single executable" bundles embed a runtime on the order of 90 MB;
temur's release binary is a ~5 MB statically linked ELF with no interpreter
and no shared-library dependencies, so it loads on old x86 machines, armv7
industrial controllers, OpenWrt-class devices, `FROM scratch` containers,
and rescue/initramfs environments. Offline is a first-class mode, not a
degraded one: the OpenAI-compatible provider runs keyless against llama.cpp
or Ollama, and quirky-local-server behavior (absent usage, missing tool-call
IDs, malformed argument JSON) is defined, tested degradation. The agent loop
is hardened for small models — schema-error feedback with bounded retries,
detection of tool calls emitted as prose, a compact prompt profile sized for
small context windows — and how well that works is measured by a scripted
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
`bash` tool call whose output file is verified from the host — model prose
is never accepted as evidence. Recorded pass: llama.cpp `server-b10068`
serving Qwen3-1.7B Q4_K_M, first attempt.

**Weak-model floor, measured.** `scripts/weak_model_eval.sh` runs six fixed
agent tasks (write, read-and-extract, targeted edit, bash, multi-file
search, edit-then-bash chain), each scored only by host-verified filesystem
assertions. Recorded score: **5/6** with Qwen3-1.7B Q4_K_M — a 1.1 GB model
— through the compact prompt profile, llama.cpp `server-b10068`, 8192-token
context, in a `--network none` pod. The one failure is documented as a model
capability floor (it batched a read and a dependent write in one parallel
call), not excluded from the score.

## Install

Prebuilt static binaries ship for `x86_64`, `aarch64`, `armv7` (hard-float —
Raspberry Pi 2/3+ and other 32-bit ARM userlands), and `i686` (SSE2
required). Because they are musl-static they run on any Linux distro,
including Alpine and other musl systems — no glibc needed.

One-liner (detects your arch, downloads, verifies the checksum, installs to
`~/.local/bin`; refuses to install anything unverified):

```sh
curl -fsSL https://raw.githubusercontent.com/thekeoni1/Temur/v0.1.1/scripts/install.sh | sh
```

Piping to `sh` is a trust decision — [read the script
first](https://github.com/thekeoni1/Temur/blob/v0.1.1/scripts/install.sh) if
you prefer. The checksum step defends against transport corruption and a
mismatched artifact; it is not a substitute for trusting the release source,
since the sums come from the same place as the binaries.

Manual install (example: x86_64; substitute your triple):

```sh
curl -fsSLO https://github.com/thekeoni1/Temur/releases/download/v0.1.1/temur-v0.1.1-x86_64-unknown-linux-musl
curl -fsSLO https://github.com/thekeoni1/Temur/releases/download/v0.1.1/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
install -m 755 temur-v0.1.1-x86_64-unknown-linux-musl ~/.local/bin/temur
```

Build from source (any Rust-supported target, e.g. pre-armv7 ARM): the
musl-static recipe is checked into `.cargo/config.toml` — rust-lld links
against the toolchain's bundled musl, and the host or cross `gcc` compiles
ring's C — no musl-gcc or musl-tools package needed.

```sh
rustup target add i686-unknown-linux-musl
cargo build --release --target i686-unknown-linux-musl
```

## Configure

Config lives at `~/.config/temur/config.json`. The minimal keyless setup
against a local llama.cpp server (`base_url` defaults to
`http://127.0.0.1:8080/v1`):

```json
{
  "provider": "openai-compat",
  "max_tokens": 1024,
  "openai_compat": { "model": "qwen3-1.7b", "context_window": 8192 }
}
```

Server setup for llama.cpp and Ollama, LAN topology, recommended small
models, and the compact prompt profile: [docs/OFFLINE.md](docs/OFFLINE.md).

The default provider is `anthropic` (model `claude-sonnet-5`); any API key
is read from a file path at startup, never from env or argv.

Two more optional keys: `sessions_dir` overrides where saved sessions live
(default: the state dir, see below), and `session_max_bytes` caps the saved
session file's size (default 4 MiB, minimum 64 KiB).

### Named profiles and in-session switching

Define named profiles — nicknames bundling provider + model + endpoint +
key file + limits — and switch between them from inside a session with
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
endpoint), `api_key_file` (path to a key file — openai-compat profiles
without one are keyless, anthropic profiles without one fall back to
`APP_SECRET_FILE`), `max_tokens` (default: the global value), and
`context_window`. Every profile is validated at startup, so `/model` can
only fail on a credential/IO problem — and a failed switch leaves the
session untouched. History continues across a switch (it is stored
provider-neutrally), and each save records whichever provider/model is
active at that moment.

## Commands

Inside a session, any input line starting with `/` is a command — it
never reaches the model or the history (which also means a literal
message starting with `/` cannot be sent):

- `/help` — list commands
- `/status` — profile, provider, model, thinking, context use, session file
- `/model` — list profiles · `/model <name>` — switch profiles mid-session
- `/clear` — wipe the session; the empty state is persisted immediately,
  so quitting and `--continue` resumes empty
- `/thinking` · `/thinking on|off` — show or flip adaptive thinking for
  this session (only the anthropic provider uses it)

Under `--mock`/`--capture-sse` the state-mutating commands report
themselves unavailable to keep replays deterministic.

In the TUI (the default on a terminal), assistant replies render as
markdown — headings, emphasis, lists, quotes, links, and code blocks
behind a dim gutter — in the same monochrome, default-terminal-color
style; the plain REPL prints raw text unchanged.

## Sessions

Every live run saves the conversation after each turn — one file per working
directory, under `$XDG_STATE_HOME/temur/sessions/` (fallback
`~/.local/state/temur/sessions/`; state, not config, because transcripts
carry tool output and grow to megabytes). `temur --continue` resumes the
current directory's session; the saved history is provider-neutral, so a
session recorded against one provider resumes against another. Saves are
atomic (write, fsync, rename) and the format contains no timestamps, so a
power cut at any instant leaves the previous complete file — resumable on a
clock-less device. Past the size cap the file drops its oldest exchanges,
always cutting at a message boundary that keeps the remainder replayable;
the in-memory conversation is never trimmed. Two processes in one directory
don't corrupt anything: last complete writer wins. To start over, delete the
directory's file from the sessions dir.

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
