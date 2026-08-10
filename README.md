# temur

[![ci](https://github.com/thekeoni1/Temur/actions/workflows/ci.yml/badge.svg)](https://github.com/thekeoni1/Temur/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/thekeoni1/Temur)](https://github.com/thekeoni1/Temur/releases/latest)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A zero-runtime-dependency single static binary AI agent for any Linux
system, down to 32-bit and embedded. Bring your own model: hosted or
fully offline against local models.

Mainstream Bun- and Node-based agents publish no 32-bit x86 or armv7
builds, and their "single executable" bundles embed a runtime on the
order of 90 MB; temur's release binary is a ~5 MB statically linked
ELF with no interpreter and no shared-library dependencies, so it
loads on old x86 machines, armv7 industrial controllers, OpenWrt-class
devices, `FROM scratch` containers, and rescue/initramfs environments.

Offline is a first-class mode, not a degraded one: the
OpenAI-compatible provider runs keyless against llama.cpp, Ollama, or
LM Studio, and quirky-local-server behavior (absent usage, missing
tool-call IDs, malformed argument JSON) is defined, tested
degradation. The agent loop is hardened for small models, and how well
that works is measured by a scripted eval rather than asserted.

The honest topology: no useful LLM runs *on* a 32-bit box; temur runs
on the constrained device where the code lives, and the model serves
from a capable machine on the same LAN or the same host.

<!-- Demo GIF placeholder: to be recorded from scripts/offline_demo.sh
     plus a short TUI session before the public launch. -->

## Proven, not claimed

Three claims, each with a scripted check behind it:

**Static binary.** `scripts/check.sh` gates every change on the
`i686-unknown-linux-musl` release build being static (`readelf` shows
no INTERP header and no dynamic section), runs the test suites and
REPL/TUI smokes against that binary in an `i386/debian` container, and
repeats the smoke in a bare `busybox` container where a dynamic binary
could not load.

**Zero-internet operation.** `scripts/offline_demo.sh` creates a
podman pod with `--network none`, asserts the negative first (a TLS
probe to the internet must fail inside the pod), then requires the
model to drive a real `bash` tool call whose output file is verified
from the host: model prose is never accepted as evidence. Recorded
pass: llama.cpp `server-b10068` serving Qwen3-1.7B Q4_K_M, first
attempt.

**Weak-model floor, measured.** `scripts/weak_model_eval.sh` runs nine
fixed agent tasks, each scored only by host-verified filesystem
assertions. The recorded run, with Qwen3-4B-Instruct-2507 Q4_K_M
through the compact prompt profile (llama.cpp `server-b10068`,
8192-token context, in a `--network none` pod):

| Task | Result |
|---|---|
| write a file | pass |
| read and extract | pass |
| targeted edit | pass |
| bash | pass |
| multi-file search | pass |
| edit-then-bash chain | pass |
| indirect delete | pass |
| gzip binary nudge | pass |
| large-output tail | pass |

Score: **9/9**. The recorded transcript lives in
[docs/RUNBOOK.md](docs/RUNBOOK.md), record "T19 acceptance".

The same harness floor is active on every provider, hosted
included: tool output
over the per-result cap keeps its true head and tail around a
narrowing marker instead of losing the end; `write` refuses to
overwrite a file the session has not read; prompts steer binary
formats to scripted `bash` runs instead of corrupt raw writes; and a
tool call written as plain text executes when it is one unambiguous,
losslessly parsed call to a real tool. Details and transcripts:
[docs/USAGE.md](docs/USAGE.md).

## Install

Prebuilt static binaries ship for `x86_64`, `aarch64`, `armv7` (hard-float,
Raspberry Pi 2/3+ and other 32-bit ARM userlands), and `i686` (SSE2
required). Because they are musl-static they run on any Linux distro,
including Alpine and other musl systems, no glibc needed. Honesty note:
the `armv7` and `aarch64` binaries are built and version-asserted under
qemu emulation and have not yet been exercised on ARM hardware.

One-liner (detects your arch, downloads, verifies the checksum, installs to
`~/.local/bin`; refuses to install anything unverified):

```sh
curl -fsSL https://raw.githubusercontent.com/thekeoni1/Temur/v0.13.0/scripts/install.sh | sh
```

Piping to `sh` is a trust decision: [read the script
first](https://github.com/thekeoni1/Temur/blob/v0.13.0/scripts/install.sh) if
you prefer. The checksum step defends against transport corruption and a
mismatched artifact; it is not a substitute for trusting the release source,
since the sums come from the same place as the binaries.

To update an existing install, re-run the one-liner from the latest
release page; it overwrites `~/.local/bin/temur` in place. The
one-liner is tag-pinned, so copy it fresh rather than from a saved
copy; `temur --version` shows what is installed.

Manual install (example: x86_64; substitute your triple):

```sh
curl -fsSLO https://github.com/thekeoni1/Temur/releases/download/v0.13.0/temur-v0.13.0-x86_64-unknown-linux-musl
curl -fsSLO https://github.com/thekeoni1/Temur/releases/download/v0.13.0/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
install -m 755 temur-v0.13.0-x86_64-unknown-linux-musl ~/.local/bin/temur
```

Build from source (any Rust-supported target): the musl-static recipe
is checked into `.cargo/config.toml`; no musl-gcc or musl-tools
package needed.

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

`temur init` offers five templates: local llama.cpp / Ollama / LM
Studio (keyless), Anthropic, OpenAI, Gemini, and xAI Grok. Against a
running local server it lists the models actually served and fills in
the server's real context allocation; for keyed templates it creates
the key file empty (mode 600) and offers a hidden paste prompt whose
input is never echoed or stored anywhere but the key file (a narrow,
documented amendment; the record "T17 - init hidden key entry" in
docs/RUNBOOK.md; no other surface accepts key material). `temur doctor` then checks the setup
read-only: config, key-file metadata, endpoint reachability, and
whether each configured model and context window matches what the
server reports.

One-shot mode runs exactly one full agentic turn (tool calls included)
and exits: assistant prose on stdout, tool and status chrome on
stderr, exit code by outcome (0 completed turn, 1 provider or startup
error, 130 interrupted), so it composes in shell pipelines:

```sh
temur -p "Summarize what this repo does"
temur --continue -p "Now list the main risks"   # chained: same session
```

Inside a session, any input line starting with `/` is a command;
`/help` lists them all. Every live run saves the conversation per
working directory, and `temur --continue` resumes it. The full command
reference, the session model, and a worked interactive session:
[docs/USAGE.md](docs/USAGE.md).

## Configure

Config lives at `~/.config/temur/config.json`; `temur init` writes any
of the documented recipes for you. The minimal keyless setup against a
local llama.cpp server (`base_url` defaults to
`http://127.0.0.1:8080/v1`):

```json
{
  "provider": "openai-compat",
  "max_tokens": 4096,
  "openai_compat": { "model": "qwen3-1.7b", "context_window": 8192 }
}
```

The default provider is `anthropic` (model `claude-sonnet-5`); any API
key is read from a file path at startup, never from env or argv.
`context_window` is advisory-only and checked, not guessed: `temur
init` fills it from a running llama.cpp server's real allocation,
`temur doctor` compares a configured value against the same source,
and `/models` on an anthropic profile compares it against the limit
the API itself reports. That last check is one-directional on purpose
and worth knowing exactly: it warns when your value is larger than the
API reports, hints the exact config line when you have set none, and
stays silent when your value is smaller, since under-configuring only
makes the advisory fire early.

The rest of the configuration surface is in
[docs/USAGE.md](docs/USAGE.md): the Anthropic multi-profile recipe,
the hosted OpenAI / Gemini / xAI templates (live-verified with
caveats, per provider: see below), named profiles, `temur init --add`,
and the context lifecycle (`/compact`, the context advisory, prompt
caching).

Hosted providers, verified against the real endpoints on 2026-08-05
and again on 2026-08-10, with the caveats named rather than averaged
away:

- **Anthropic**: live-verified, including the four-profile template
  and per-model context windows read off the API.
- **OpenAI**: live-verified on `gpt-4o`, which the template now
  defaults to and whose 16384 completion cap it bakes. The gpt-5 era
  ids reject `max_tokens` and want `max_completion_tokens`; set
  `"max_tokens_parameter": "max_completion_tokens"` on the profile
  and temur sends that name instead. Live-verified on `gpt-5` on
  2026-08-10, tool call included: without the field the turn fails
  with an HTTP 400 saying `max_tokens` is not supported and naming
  `max_completion_tokens` as the replacement; with it, the same
  prompt completes, the server accepts the cap instead of silently
  dropping it, and no other field is objected to.
- **Gemini**: live-verified, tool calls included, after two fixes the
  verification itself found (its streaming responses report
  `finish_reason` "stop" while attaching real tool calls, and it
  requires its opaque thought signatures echoed back or it rejects
  the next request). It also bills thinking tokens while naming them
  in no usage field, which used to leave `/status` reading a floor;
  temur now recovers them from the `total_tokens` it does report.
  Live-verified on the streaming path on 2026-08-10: a turn reporting
  6498 prompt and 1 completion token against a total of 6526 recorded
  28 output tokens, the 27-token gap folded in where it is billed. A
  wire that omits usage altogether is still a floor, since nothing can
  recover what was never sent.
- **xAI**: unverified. No key was available; the template is written
  to the published spec. Server setup for llama.cpp, Ollama, and LM Studio,
plus recommended small models: [docs/OFFLINE.md](docs/OFFLINE.md).

## Untrusted hosts

temur's key isolation (a file guard over every tool, a bash sandbox
that masks key files, and redaction of the active key from tool
results) guards against the MODEL, not against the host. Anything that
reaches the host root user, a snapshotting hypervisor, or another user
with your file access can read whatever key you place there. Never
place a primary key on a host you do not control: use a dedicated key
with a spend cap, rotate it on a schedule, and revoke it when the
machine goes away. The durable pattern is a relay you control (LiteLLM
is the common choice) holding the real provider key, with the
untrusted host given only a revocable virtual key. The full isolation
rules, their honest limits, and the worked patterns:
[docs/USAGE.md](docs/USAGE.md).

## Scope

temur deliberately does not do LSP, MCP, IDE plugins, web UI,
server/multi-client mode, or a plugin ecosystem: each adds dependency and
maintenance surface (several would threaten the static-musl constraint) and
none serves constrained, offline, or weak-model use. Small surface is a
feature.

## How this was built

temur is built by directing Claude Code, an AI coding agent, under a
fixed set of working rules checked into this repo as
[CLAUDE.md](CLAUDE.md); the build machine and its security boundary
are reproduced step by step in [docs/SETUP.md](docs/SETUP.md). Every
change passes `scripts/check.sh` (static musl build, container test
suites, REPL and TUI smokes, a bare-busybox run) before it lands, and
agent-facing behavior is scored by the scripted weak-model eval rather
than judged by eye. The transparency is deliberate: the working rules,
the acceptance records in docs/RUNBOOK.md, and the self-analysis in
ROADMAP.md are part of the project, not internal scaffolding.

## Attribution

The tool prompt texts are ported near-verbatim from
[sst/opencode](https://github.com/sst/opencode) v1.2.25 (MIT).

License: MIT
