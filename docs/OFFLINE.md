# Offline operation

Offline is not a degraded mode of temur. It is the point. One static
binary with zero runtime dependencies, pointed at a local inference
server, is a complete coding agent with no internet anywhere in the loop:
air-gapped labs, regulated networks, ships, field sites, or just a laptop
on a plane.

The honest topology: no useful LLM runs *on* a 32-bit or embedded box.
temur runs where the code lives (the constrained device) and the model
serves from a capable machine, either the same host (a modern workstation
with no internet) or elsewhere on the LAN. Everything below assumes that
shape.

## Quickstart: llama.cpp

llama.cpp's `llama-server` speaks the OpenAI-compatible API temur's
`openai-compat` provider targets.

**Native:**

```sh
llama-server -m /path/to/model.gguf -c 8192 --jinja --port 8080
```

**Container** (the repo pins `server-b10068` - the tag scheme is
`server-b<build>`; update the pin deliberately, never track `latest`):

```sh
podman run --rm -p 127.0.0.1:8080:8080 \
    -v /path/to/model.gguf:/model.gguf:ro \
    ghcr.io/ggml-org/llama.cpp:server-b10068 \
    -m /model.gguf -c 8192 --jinja --host 0.0.0.0 --port 8080
```

**One window** (checkout only): `scripts/serve.sh` wraps the container
form as a detached background server, so the server and temur share one
terminal, no second WSL window:

```sh
scripts/serve.sh start           # lone .gguf in MODELS_DIR auto-selected
scripts/serve.sh start qwen3-4b  # pick by name from MODELS_DIR
scripts/serve.sh status          # health + summary (shows the mounted model)
scripts/serve.sh stop            # idempotent teardown
```

Model selection: the optional `start` argument matches case-insensitively
against the basenames of `$MODELS_DIR/*.gguf` (default `$HOME/models`).
An exact basename match (`name` or `name.gguf`) wins; otherwise a unique
substring match selects; zero or several matches fail and list every
candidate with its size (matches marked when ambiguous). With no
argument, a lone `.gguf` in the dir is auto-selected; zero or several
fail and list the candidates: nothing is ever guessed between models.
`MODEL_GGUF=/path/to/model.gguf` still works as an explicit override,
but combining it with a name argument is an error (choose one). A
running server keeps its current model: `start` against a running
container just reports it, so switching models is `stop` then
`start <name>`.

RAM fit warning: before starting, the script compares the model file
size plus a deliberately generous context allowance (128 KiB per
context token, covering f16 KV cache and compute buffers at these
defaults) against `MemAvailable` and prints a single WARN line when it
does not fit. It is advisory only: the start proceeds, because mmap'd
weights can still limp along (expect thrashing).

Knobs (env overrides): `MODEL_GGUF` (explicit path, see above),
`MODELS_DIR` (the search dir; serve.sh only - the demo and eval scripts
stay explicit),
`LLAMA_IMAGE` (the pin above; a missing image prints the exact
`podman pull` command and stops, nothing is pulled for you), `CTX`,
`PORT`/`BIND` (published host side only; the container-internal port is
always 8080 - a non-default `PORT` prints the `base_url` to set),
`CONTAINER_NAME` (default `temur-llama`), `MEMINFO` (the meminfo file
the RAM warning reads; default `/proc/meminfo`).

> **`--jinja` is STRONGLY RECOMMENDED for tool calls.** Many model chat
> templates need it before llama-server presents tool definitions
> properly: without it those models answer in prose instead of calling
> tools. Some combinations (e.g. Qwen3 on recent llama.cpp builds) emit
> structured tool calls even without the flag, so it is not an absolute
> requirement, but if temur connects and never executes a tool, check
> this flag first.

`-c` sets the server-side context size in tokens; mirror the same number
into temur's `context_window` (below) so temur's advisory warnings match
reality.

## Quickstart: Ollama

```sh
ollama pull qwen3:1.7b
ollama list    # confirm the model and its size
ollama serve   # if not already running as a service
```

Ollama exposes the OpenAI-compatible API at `http://127.0.0.1:11434/v1`.
A keyless temur profile only needs the `base_url` and the model name as
`ollama list` prints it:

```json
{
  "provider": "openai-compat",
  "max_tokens": 4096,
  "openai_compat": {
    "base_url": "http://127.0.0.1:11434/v1",
    "model": "qwen3:1.7b",
    "context_window": 8192
  }
}
```

temur's `/models` command works against Ollama (it serves
`GET /v1/models`), so a typo'd model name is easy to spot from inside
the session.

Mind the context size: Ollama defaults to a small `num_ctx` regardless of
what the model supports. Raise it (e.g. `OLLAMA_CONTEXT_LENGTH=8192`, or
`num_ctx` in a Modelfile) and set temur's `context_window` to the same
value: an agent conversation with tool definitions overflows a default
window quickly, and Ollama silently truncates rather than erroring.

## Quickstart: LM Studio

LM Studio's local server speaks the same OpenAI-compatible API, default
port 1234. Load a model in the GUI first, then enable the server (the
Developer tab); the server serves whatever is loaded. A keyless profile:

```json
{
  "provider": "openai-compat",
  "max_tokens": 4096,
  "openai_compat": {
    "base_url": "http://127.0.0.1:1234/v1",
    "model": "loaded-model-id",
    "context_window": 8192
  }
}
```

`/models` works here too (`GET /v1/models` lists the loaded and
downloaded models), which is the quickest way to find the exact model
id to put in the profile.

**Reaching a Windows-host LM Studio from WSL2:** this varies by setup;
what follows is orientation, not automation, and nothing in the repo
scripts it.

- Mirrored networking (Windows 11: `networkingMode=mirrored` in
  `%USERPROFILE%\.wslconfig`, then `wsl --shutdown`): WSL2 shares the
  host's interfaces, so `http://127.0.0.1:1234/v1` works as-is.
- Classic NAT (the default on Windows 10 and unconfigured Win11): WSL2
  is a separate network. Use the Windows host's IP as seen from WSL2,
  usually the default-gateway address (`ip route show default`). Note
  `/etc/resolv.conf`'s nameserver is a common suggestion but lies
  whenever DNS is overridden, so prefer the route. Two host-side
  requirements: LM Studio must listen on all interfaces (serve on
  network / bind `0.0.0.0`, not just localhost), and Windows Defender
  Firewall must allow inbound connections to it on port 1234.

## temur configuration

Config lives at `~/.config/temur/config.json` (or
`$XDG_CONFIG_HOME/temur/config.json`).

**Keyless local server (minimal)**: `base_url` defaults to
`http://127.0.0.1:8080/v1` (llama.cpp's default port); no key, no auth
header:

```json
{
  "provider": "openai-compat",
  "max_tokens": 4096,
  "openai_compat": { "model": "qwen3-1.7b", "context_window": 8192 }
}
```

**Server elsewhere on the LAN:**

```json
{
  "provider": "openai-compat",
  "max_tokens": 4096,
  "openai_compat": {
    "base_url": "http://192.168.1.10:8080/v1",
    "model": "qwen3-1.7b",
    "context_window": 8192
  }
}
```

**Keyed remote compat endpoint**: the key is read from a file path
(never env, never argv), same isolation rule as the Anthropic provider:

```json
{
  "provider": "openai-compat",
  "openai_compat": {
    "base_url": "https://api.example.com/v1",
    "model": "provider-model-id",
    "api_key_file": "/path/to/key-file"
  }
}
```

Set `max_tokens` well below the model's context window for local use.
temur's default (32000) suits large cloud contexts; against an 8192-token
window it means a single response is allowed to (try to) outgrow the
whole context, and temur's advisory warning will (correctly) fire
immediately. 1024–4096 is a sensible local range.

## Compact prompt profile

The stock tool descriptions are the OpenCode-ported prompts, sized for
Claude-class context windows (~24 KB of tool text). On a small local
window that is a real tax before the conversation even starts. Setting

```json
{ "prompt_profile": "compact" }
```

(top-level, next to `provider`) swaps in hand-trimmed descriptions for
the largest tools (bash, todowrite, edit) and a shorter default system
prompt, bringing total tool text under 8 KB. Tool set, order, and input
schemas are identical in both profiles (only the description text
varies), and an explicit `system_prompt` in config always wins over
either default.

The profile is **explicit-only**: absent or `"full"` means the stock
prompts (byte-identical to pre-profile behavior), `"compact"` opts in,
anything else is a startup config error. temur never auto-selects a
profile from `context_window` or the model name.

Named profiles can each carry their own `prompt_profile` (same values,
same explicit-only rule; absent = the global setting above). That is
the natural pairing for mixed setups: compact on the small local
profile, full on a hosted one:

```json
{
  "profiles": {
    "local":  { "provider": "openai-compat", "model": "qwen3-1.7b",
                "prompt_profile": "compact", "context_window": 8192 },
    "sonnet": { "provider": "anthropic", "model": "claude-sonnet-5" }
  }
}
```

`/model local` ⇄ `/model sonnet` swaps the tool descriptions and the
default system prompt together with the provider (`/status` shows the
live value as `prompt: full|compact`); an explicit `system_prompt`
override still wins in both profiles, and a raw-id switch
(`/model <model-id>`) never changes the prompt profile.

## LAN topology

```
constrained box (i686/ARM/router/…)          capable machine (x86_64, GPU…)
┌──────────────────────────────┐             ┌─────────────────────────────┐
│ temur (musl-static, ~5 MB)   │  HTTP LAN   │ llama-server -c 8192 --jinja │
│ + your code                  │────────────▶│ + model.gguf                │
└──────────────────────────────┘   :8080     └─────────────────────────────┘
```

No internet is required on either side. The constrained box needs only
the temur binary and your working tree; the model machine needs only
llama.cpp (or Ollama) and a `.gguf` file.

## Recommended small models

Small-model tool-calling is a real floor, not a marketing detail; these
are the smallest models observed to drive temur's tools with acceptable
reliability, smallest-first is NOT best-first. "Tool calls" means the
model reliably emits structured tool calls when told which tool to use;
"indirect selection" means it picks the right tool on its own when the
task does not name one (the weak-model eval's task 7). "Verified" rows
ran the full eval harness on the stated date; "reported" rows carry
earlier observations not re-run through the current harness.

Since T15, temur can also read a running server's own model listing:
`temur init`'s local template offers the served models as a numbered
pick (a two-row summary of this table prints only as the fallback when
no server answers), and `temur doctor` warns when a configured model is
not in the listing. Both use a single unauthenticated GET, against
keyless endpoints only.

| Model | Quant | File size | Est. RAM at 8k ctx | Tool calls | Indirect selection | Status |
|---|---|---|---|---|---|---|
| **Qwen3-1.7B** (primary) | Q4_K_M | ~1.1 GB | ~2.1 GB | yes | yes | verified 2026-07-26 (eval 7/7) |
| Qwen3-4B-Instruct-2507 | Q4_K_M | ~2.4 GB | ~3.4 GB | yes | yes | verified 2026-07-26 (eval 7/7) |
| Qwen2.5-Coder-3B-Instruct | Q4_K_M | ~1.9 GB | ~2.9 GB | no (prose-only) | n/a | verified 2026-07-26 (eval 0/7) |
| Qwen2.5-Coder-1.5B-Instruct | Q4_K_M | ~1.0 GB | ~2.0 GB | yes | untested | reported (pre-T11) |
| Qwen3-0.6B | Q4_K_M | ~0.5 GB | ~1.5 GB | degraded | untested | reported (pre-T11) |

Est. RAM uses the serve.sh warning's own arithmetic: file size plus
128 KiB per context token of KV and compute allowance at 8192 ctx
(about 1.0 GB). Verified rows ran the full seven-task eval (compact
profile, llama.cpp `server-b10068`, ctx 8192, `--jinja`) on the stated
date. The Qwen2.5-Coder-3B result deserves its honest detail: it
consistently picked the RIGHT tool, including bash with `rm` on the
indirect probe, but emitted every call as a fenced JSON block instead
of a structured tool call on this stack, so temur's prose-tool-call
detection asked for the tool interface, the model repeated the prose,
and all seven tasks failed on wire format, not on reasoning. Notes
carried from earlier observation: Qwen3-1.7B has the best tool-calling
reliability per byte of the small trio and is the default
recommendation; Qwen2.5-Coder-1.5B is a code-tuned alternative with
strong edits but slightly weaker tool discipline; Qwen3-0.6B fits
almost anywhere but degrades to single-tool tasks.

Download source: the Q4_K_M quants above are published in the community
`unsloth/…-GGUF` repositories on Hugging Face (e.g.
`unsloth/Qwen3-1.7B-GGUF`); the official `Qwen/Qwen3-1.7B-GGUF` repo
publishes Q8_0 only (1.83 GB), a fine larger-footprint alternative.

Larger is better whenever the serving machine allows it; anything in the
7B+ class changes the experience qualitatively.

## `context_window`: what it does and does not do

`openai_compat.context_window` tells temur how big the *served* context
is, a property of the server (llama.cpp `-c`, Ollama `num_ctx`) that
the OpenAI-compatible API itself does not expose. llama.cpp does expose
it out of band, at the server root's `/props` endpoint
(`default_generation_settings.n_ctx`), and temur reads it there with
the same unauthenticated keyless GET discipline as the model listing:
`temur init` auto-fills a fresh local config with the detected value
(server down, or any non-llama.cpp server, keeps the baked 8192), and
`temur doctor` checks a configured value against the live allocation,
warning on a mismatch in either direction and suggesting the exact
config line when the value is missing. Ollama's equivalent
(`/api/show`) is deliberately not probed, so for Ollama and LM Studio
you still state the value by hand. However it gets set, temur then:

- advises once per session when the conversation gets tight: at 80% of
  the window, or when the remaining room drops below `max_tokens` (the
  next response may not fit), whichever comes first; the advisory names
  `/compact` and a new session as the remedies, and also fires at
  `--continue`/`--resume`/`/resume` when the restored session is
  already past the threshold;
- rewords a `max_tokens` truncation that happens near the window to name
  the likely real cause: context overflow.

**The honest caveat:** temur ships no tokenizer. The estimate is the
input+output token count of the most recent response, as reported by the
server, one round-trip stale, and absent entirely on servers that never
report usage (then the feature stays silent rather than inventing
numbers). That's why every figure is written `~N`. It is an advisory, not
an enforcement: temur never trims, blocks, or compacts a request on its
own; `/compact` exists but only ever runs because you typed it.

## Degradation on quirky servers

Local-server OpenAI compatibility is approximate. temur degrades
politely; none of these are errors:

| Server quirk | temur behavior |
|---|---|
| Usage never reported | Token counts display as `—` (never a fake 0); context advisory stays silent |
| Tool-call IDs absent | IDs synthesized (`call_0`, `call_1`, …); round-trip consistently |
| Whole tool call in one chunk | Assembled normally |
| Malformed tool-call argument JSON | Arguments become `{}`; the tool's schema error feeds back to the model for a retry |
| `role` repeated in every delta | Tolerated |
| `finish_reason` missing after tool calls | Tool use inferred; the calls execute |
| Error as a bare string (`{"error":"…"}`) | Parsed and reported like the object form |

## The offline demo

`scripts/offline_demo.sh` proves the whole story end-to-end with **zero
internet by construction**: one podman pod created with `--network none`
(loopback-only namespace) holding a llama.cpp server and the musl-static
temur binary, which must then drive a real tool call. It is operator-run
and is not part of `check.sh`.

```sh
MODEL_GGUF=/path/to/model.gguf scripts/offline_demo.sh
```

The script never pulls images or models; preflight checks print the exact
`podman pull` command and exit if anything is missing. It asserts the
negative (TLS to the internet MUST fail inside the pod) before the
positive (the model must use the bash tool to write a proof file, which
is verified from the host: model prose is never trusted as evidence).

## The weak-model eval

`scripts/weak_model_eval.sh` measures, instead of claiming, how well a
small model drives temur's tools. Same setup discipline as the demo
(operator-run, not part of `check.sh`; podman pod with `--network none`;
nothing ever pulled; musl binary readelf-checked), then seven fixed
tasks, each in a fresh work directory with a fresh temur process: a
plain file write, a read-and-extract, a targeted edit that must leave
the rest of the file unchanged, a bash mkdir+write, a search across
three files, an edit-then-bash chain where order matters, and an
indirect-tool-selection probe ("delete the file", naming no tool: the
registry has no delete tool, so the model must choose bash by itself).
Every task is scored by a host-verified filesystem assertion (model
prose is never evidence; the indirect probe additionally requires a bash
`rm` call in the transcript), and the run ends with a fixed-width
PASS/FAIL table plus a `SCORE: N/7` line.

```sh
MODEL_GGUF=/path/to/model.gguf scripts/weak_model_eval.sh
```

Knobs: `MUSL_BIN`, `LLAMA_IMAGE`, `CTX` (default 8192), `PROMPT_PROFILE`
(default `compact`, written into the generated keyless config),
`EVAL_TASK_TIMEOUT` (seconds per task, default 300), `EVAL_MIN` (default
0 = informational; a nonzero value makes the script exit 1 below that
score), and `EVAL_TRANSCRIPT_DIR` (per-task transcripts are kept there
for debugging).

## Troubleshooting

1. **Tools never get called; the model answers in prose.** llama.cpp:
   you forgot `--jinja`. Check this before anything else.
2. **Connection refused.** Server not up, wrong port, or `base_url`
   points at the wrong host. For Ollama remember the port is 11434 and
   the path prefix is `/v1`.
3. **Responses cut off mid-thought.** `max_tokens` too small - or, if
   temur's notice mentions the context window, the conversation has
   outgrown `-c`/`num_ctx`: start a new session or serve a bigger
   window.
4. **Token counts show `—`.** The server doesn't report usage. Harmless;
   the context advisory is off in this state.
5. **First response is very slow.** Model load and prompt processing on
   the server; subsequent turns reuse the loaded model.
6. **Small model loops or fumbles tool arguments.** Known floor: see
   the models table; a schema error feeding back gives the model a
   retry, but persistent loops trip temur's doom-loop guard by design.
