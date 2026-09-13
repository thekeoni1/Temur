# temur

[![ci](https://github.com/thekeoni1/Temur/actions/workflows/ci.yml/badge.svg)](https://github.com/thekeoni1/Temur/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/thekeoni1/Temur)](https://github.com/thekeoni1/Temur/releases/latest)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A zero-runtime-dependency single static binary AI agent for any Linux
system, down to 32-bit and embedded. Bring your own model: hosted
(Anthropic, OpenAI, Gemini, xAI) or fully offline against a local
llama.cpp, Ollama, or LM Studio server.

If you have an old or low-powered Linux machine and want an AI
assistant on it that runs against a modest local model, with nothing
to install, this is built for that. No useful LLM runs on a 32-bit
box: temur runs on the constrained device where the code lives, and
the model serves from a capable machine on the same LAN or the same
host.

Try it in your browser at https://play.temur.live before you download
anything. The demo page runs the real released 32-bit binary inside an
emulated Linux, offline with no key or with a key you supply, and you
can hand it a PDF or spreadsheet of your own to read.

Most of what makes a small model unusable is the harness's job to
absorb, malformed tool arguments and mid-turn context overflow
included, and that is what temur's agent loop is for: the bullets below
and [docs/COMPARISON.md](docs/COMPARISON.md).

- A single static ELF, under 10 MB on 32-bit, zero dependencies.
  Mainstream Bun- and Node-based agents publish no 32-bit x86 or armv7
  builds, and their "single executable" bundles embed a runtime on the
  order of 90 MB. temur's release binary has no interpreter and no
  shared libraries, so it loads on old x86 machines, armv7 industrial
  controllers, OpenWrt-class devices, `FROM scratch` containers, and
  rescue/initramfs environments. The v0.35.0 binaries measure 9.87 MB
  on i686 and 11.71 MB on x86_64: the PDF, spreadsheet and document
  parsers are compiled in, the price of reading documents with nothing
  installed.
- Offline is a first-class mode. The OpenAI-compatible provider runs
  keyless against local servers, and quirky-local-server behavior
  (absent usage, missing tool-call IDs, malformed argument JSON) is
  defined, tested degradation.
- Small models are the design target. The agent loop is hardened
  for weak local models: prose-call recovery, tolerant argument
  parsing, self-healing tool errors, context-scaled output caps, and
  automatic compaction with overflow recovery.
- Reads PDFs and office documents and writes spreadsheets, charts
  included, and documents, with nothing installed on the machine:
  `read` takes `.pdf`, `.xlsx`, `.xlsm`, `.xls`, `.ods` and `.docx`,
  CSV written to an `.xlsx` path becomes a workbook, Markdown written
  to a `.docx` or `.pdf` path becomes the document, and the
  `spreadsheet` tool adds
  charts and multiple sheets.
- A `TEMUR.md` (or `AGENTS.md`) in the repository joins the system
  prompt at startup, so a project states its build commands and
  conventions once.
- Measured, with the records published. A scripted nine-task eval scores the
  agent loop against real small models, and a comparison against
  OpenCode and Codex CLI publishes every cell, losses included. The
  records: the next section, and the results-at-a-glance table that
  opens [docs/COMPARISON.md](docs/COMPARISON.md).
- Built in the open by directing an AI agent under working rules
  checked into this repo, with every milestone's acceptance record
  kept. See "How this was built" below.

## Demo

A real context overflow on a local 4B model: the server rejects the
request, temur truncates the largest tool result, retries, and the
turn finishes.

![temur recovering from a context overflow by truncating the largest tool result and retrying](docs/demo/overflow-recovery.gif)

A small real task end to end on the same model: find the file, read
it, edit it, run it to confirm.

![temur finding, reading, editing, and running a file](docs/demo/edit-and-run.gif)

Both captured live against a keyless local llama.cpp server
(Qwen3-4B-Instruct-2507, the default local model) on the shipped
0.33.0 binary. Nothing is spliced or re-typed; pauses longer than
2 seconds are shortened in the rendered GIFs.

## Where it fits

Concrete situations this design serves:

- An isolated sandbox. A throwaway container or `--network none`
  pod: copy one binary in, point it at a model server, and you have
  a full agent with no package manager, no runtime, and no network
  path except the one you chose. `scripts/offline_demo.sh` runs this
  shape.
- Air-gapped and regulated environments. Labs, ships, field
  sites, networks where nothing calls out: temur plus a local
  llama.cpp server is a complete agent with no internet anywhere in
  the loop.
- Old or small hardware. 32-bit x86 machines, armv7 boards,
  OpenWrt-class devices, rescue shells and initramfs environments,
  places a 90 MB runtime bundle will never load.
- Code that stays home. Against a keyless local server, prompts,
  code, and tool output never leave your machine or LAN.
- Shell pipelines. `temur -p` runs one full agentic turn and
  exits with a meaningful code, prose on stdout and chrome on
  stderr, so it composes with everything else in a script.
- Weak-model realism. If the model you can run is a 4B, the loop is
  hardened and measured for that, and the eval records say which
  models hold up.

## What is proven

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
from the host. Model prose is never accepted as evidence. Recorded
pass: llama.cpp `server-b10068` serving Qwen3-1.7B Q4_K_M, first
attempt.

**Weak-model floor.** `scripts/weak_model_eval.sh` runs nine
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

Score: **9/9**. The transcript is in
[docs/RUNBOOK.md](docs/RUNBOOK.md), record "T19 acceptance". The
table is that run, kept as a record and not re-measured per release.
The current per-release matrix, every model tested and dated to the
pass that produced it, is in [docs/OFFLINE.md](docs/OFFLINE.md).

How temur compares against released builds of OpenCode and Codex CLI,
driving the same local model on the same machine, is in
[docs/COMPARISON.md](docs/COMPARISON.md). It was built by temur's side
against tasks from temur's own eval suite, it says so up front, and it
publishes the cells temur loses. The same page carries a
Terminal-Bench 2 row, a suite temur did not write, on a subset fixed
by rule before any score was seen, and a GPU desktop row running the
same subset on a second machine.

The same harness floor is active on every provider, hosted
included. Tool output over the per-result cap keeps its head and
tail around a narrowing marker instead of losing the end. `write`
refuses to overwrite a file the session has not read. Prompts steer
binary formats to scripted `bash` runs instead of corrupt raw writes.
A tool call written as plain text executes when it is one unambiguous,
losslessly parsed call to a real tool. Details and transcripts:
[docs/USAGE.md](docs/USAGE.md).

## Install

Prebuilt static binaries ship for `x86_64`, `aarch64`, `armv7` (hard-float,
Raspberry Pi 2/3+ and other 32-bit ARM userlands), and `i686` (SSE2
required). Because they are musl-static they run on any Linux distro,
Alpine included, with no glibc. The `armv7` and `aarch64` binaries are
built and version-asserted under qemu and have not been exercised on
ARM hardware.

One-liner (detects your arch, downloads, verifies the checksum, installs to
`~/.local/bin`; refuses to install anything unverified):

```sh
curl -fsSL https://raw.githubusercontent.com/thekeoni1/Temur/v0.35.0/scripts/install.sh | sh
```

Piping to `sh` is a trust decision: [read the script
first](https://github.com/thekeoni1/Temur/blob/v0.35.0/scripts/install.sh) if
you prefer. The checksum step defends against transport corruption and a
mismatched artifact. It is not a substitute for trusting the release source,
since the sums come from the same place as the binaries.

To update, re-run the one-liner from the latest release page; it
overwrites `~/.local/bin/temur` in place. The one-liner is tag-pinned,
so copy it fresh each time; `temur --version` shows what is installed.

Manual install (example: x86_64; substitute your triple):

```sh
curl -fsSLO https://github.com/thekeoni1/Temur/releases/download/v0.35.0/temur-v0.35.0-x86_64-unknown-linux-musl
curl -fsSLO https://github.com/thekeoni1/Temur/releases/download/v0.35.0/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
install -m 755 temur-v0.35.0-x86_64-unknown-linux-musl ~/.local/bin/temur
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

`temur init` says where the config will go, then offers five
templates: local llama.cpp / Ollama / LM Studio (keyless), Anthropic,
OpenAI, Gemini, and xAI Grok. Against a running local server it lists
the models served and fills in the server's context allocation. The
hosted templates point at `/models` for ids other than the default.
For keyed templates it asks where to save the key, creates the file
empty (mode 600), and offers a hidden paste prompt whose input is
never echoed or stored anywhere but the key file (a documented
amendment, record "T17 - init hidden key entry" in docs/RUNBOOK.md;
no other surface accepts key material). A wrong answer is
recoverable: an existing key file is replaced only after a question
that defaults to no, an unusable key path is named before anything is
written, and a key step that fails takes the config back with it. The
closing line says what to do next: start temur, or start your server
first. `temur doctor` then checks the setup
read-only: config, key-file metadata, endpoint reachability, and
whether each configured model and context window matches what the
server reports, one authenticated listing per keyed endpoint.

When a tool wants to change your system, temur asks. At that prompt `a`
allows every later call of that same tool for the rest of the session, where
`y` allows just the one: useful once you have seen what a run is doing, and
scoped to the tool that asked, not to everything. It covers `write` and `edit`
always, and `bash` only where a key sandbox is present, because the keyless
bash prompt offers `y/N` alone by design.

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
of the documented recipes. The minimal keyless setup against a
local llama.cpp server (`base_url` defaults to
`http://127.0.0.1:8080/v1`):

```json
{
  "provider": "openai-compat",
  "max_tokens": 4096,
  "openai_compat": { "model": "qwen3-4b", "context_window": 8192 }
}
```

The default provider is `anthropic` (model `claude-sonnet-5`); any API
key is read from a file path at startup, never from env or argv.
`context_window` is advisory-only and always checked: `temur init`
fills it from a running llama.cpp server's allocation, `temur doctor`
compares a configured value against the same source, and `/models` on
an anthropic profile compares it against the limit the API reports.
That last check warns when your value is larger than the API reports,
since the advisory then fires too late and requests can fail at the
real limit. It hints when it is smaller, which is safe but fires the
advisory early. It hints the exact config line when you have set
none, and stays silent when the two agree. The API lists dated model
ids only, so a profile on a bare
alias is matched against dated entries of that alias, and only when
they agree on one window.

The rest of the configuration surface is in
[docs/USAGE.md](docs/USAGE.md): the Anthropic multi-profile recipe,
the hosted OpenAI / Gemini / xAI templates (live-verified, caveats
below), named profiles, `temur init --add`,
and the context lifecycle (`/compact`, the context advisory, prompt
caching).

Hosted providers, verified against the real endpoints on 2026-08-05
and 2026-08-10:

- **Anthropic**: live-verified, including the four-profile template
  and per-model context windows read off the API.
- **OpenAI**: live-verified on `gpt-4o`, which the template defaults
  to and whose 16384 completion cap it bakes. The gpt-5 era
  and o-series ids reject `max_tokens` and want
  `max_completion_tokens`. When a server rejects the name, temur
  retries once with the name it asked for, keeps that name for the
  session, and prints one line saying so. On `api.openai.com` it
  picks the right name up front. Setting
  `"max_tokens_parameter": "max_completion_tokens"` on the profile
  still sends that name from the start. Live-verified on `gpt-5` on
  2026-08-10, tool call included.
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
  wire that omits usage altogether is still a floor.
- **xAI**: unverified. No key was available; the template is written
  to the published spec.

Server setup for llama.cpp, Ollama, and LM Studio, plus recommended
small models: [docs/OFFLINE.md](docs/OFFLINE.md).

## Untrusted hosts

temur's key isolation (a file guard over every tool, a bash sandbox
that masks key files, and redaction of the active key from tool
results) guards against the model. It does not guard against the
host. Anything that reaches the host root user, a snapshotting
hypervisor, or another user with your file access can read whatever
key you place there. Never place a primary key on a host you do not
control: use a dedicated key with a spend cap, rotate it on a
schedule, and revoke it when the machine goes away. The durable
pattern is a relay you control (LiteLLM is the common choice) holding
the real provider key, with the untrusted host given only a revocable
virtual key. The full isolation rules, their limits, and the worked
patterns: [docs/USAGE.md](docs/USAGE.md).

## Scope

temur does not do LSP, MCP, IDE plugins, web UI,
server/multi-client mode, or a plugin ecosystem: each adds dependency and
maintenance surface (several would threaten the static-musl constraint) and
none serves constrained, offline, or weak-model use.

## Documentation

| Document | What it holds |
| --- | --- |
| [docs/USAGE.md](docs/USAGE.md) | Full command reference, the whole configuration surface, the session model, key isolation rules and their limits |
| [docs/OFFLINE.md](docs/OFFLINE.md) | Local server setup (llama.cpp, Ollama, LM Studio) and the dated per-model eval matrix |
| [docs/COMPARISON.md](docs/COMPARISON.md) | temur against OpenCode and Codex CLI on the same local models, plus a Terminal-Bench 2 subset; opens with a results-at-a-glance table |
| [docs/SETUP.md](docs/SETUP.md) | The build machine and its security boundary, reproducible step by step |
| [docs/RUNBOOK.md](docs/RUNBOOK.md) | The acceptance record and ship procedure of every milestone |
| [docs/IMPLEMENTATION_PLAN.md](docs/IMPLEMENTATION_PLAN.md) | The v1 build plan, kept as the record |
| [ROADMAP.md](ROADMAP.md) | Milestone history, the findings queue, and the project's self-analysis |
| [CHANGELOG.md](CHANGELOG.md) | Per-release changes, newest first |

## How this was built

temur is built by directing Claude Code, an AI coding agent, under a
fixed set of working rules checked into this repo as
[CLAUDE.md](CLAUDE.md); the build machine and its security boundary
are reproduced step by step in [docs/SETUP.md](docs/SETUP.md). Every
change passes `scripts/check.sh` (static musl build, container test
suites, REPL and TUI smokes, a bare-busybox run) before it is merged,
and agent-facing behavior is scored by the scripted weak-model eval.
The transparency is deliberate: the working rules, the acceptance
records in docs/RUNBOOK.md, and the self-analysis in ROADMAP.md are
part of the project.

## Attribution

The tool prompt texts are ported near-verbatim from
[sst/opencode](https://github.com/sst/opencode) v1.2.25 (MIT).

License: MIT
