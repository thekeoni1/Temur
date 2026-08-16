# Offline operation

Offline is not a degraded mode of temur. It is the point. One static
binary with zero runtime dependencies, pointed at a local inference
server, is a complete AI agent with no internet anywhere in the loop:
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

**Container** (the repo pins `server-b10438` - the tag scheme is
`server-b<build>`; update the pin deliberately, never track `latest`):

```sh
podman run --rm -p 127.0.0.1:8080:8080 \
    -v /path/to/model.gguf:/model.gguf:ro \
    ghcr.io/ggml-org/llama.cpp:server-b10438 \
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
  "openai_compat": { "model": "qwen3-4b", "context_window": 8192 }
}
```

**Server elsewhere on the LAN:**

```json
{
  "provider": "openai-compat",
  "max_tokens": 4096,
  "openai_compat": {
    "base_url": "http://192.168.1.10:8080/v1",
    "model": "qwen3-4b",
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
ran the full eval harness on the stated date. Every row below is a
measurement; nothing here is carried over from earlier observation.

Since T15, temur can also read a running server's own model listing:
`temur init`'s local template offers the served models as a numbered
pick (a two-row summary of this table prints only as the fallback when
no server answers), and `temur doctor` warns when a configured model is
not in the listing. Both use a single unauthenticated GET, against
keyless endpoints only.

| Model | Quant | File size | Est. RAM at 8k ctx | Tool calls | Indirect selection | Status |
|---|---|---|---|---|---|---|
| **Qwen3-4B-Instruct-2507** (primary) | Q4_K_M | ~2.4 GB | ~3.4 GB | yes | yes | verified 2026-08-15 (eval 9/9, 9/9) |
| Qwen3-4B-Thinking-2507 | Q4_K_M | ~2.4 GB | ~3.4 GB | yes | yes | verified 2026-08-15 (eval 7/9, 9/9) |
| Qwen2.5-Coder-3B-Instruct | Q4_K_M | ~1.9 GB | ~2.9 GB | via prose recovery | yes | verified 2026-08-15 (eval 6/9, 9/9) |
| Qwen3-1.7B (low-RAM floor) | Q4_K_M | ~1.1 GB | ~2.1 GB | yes | yes | verified 2026-08-15 (eval 7/9, 7/9) |
| Qwen3-0.6B | Q4_K_M | ~0.4 GB | ~1.4 GB | degraded | yes | verified 2026-08-15 (eval 5/9, 5/9) |
| Qwen2.5-Coder-1.5B-Instruct | Q4_K_M | ~0.9 GB | ~1.9 GB | intermittent | 1 of 2 runs | verified 2026-08-15 (eval 4/9, 4/9) |
| Llama-3.2-3B-Instruct | Q4_K_M | ~1.9 GB | ~2.9 GB | unreliable | no | verified 2026-08-15 (eval 2/9, 2/9) |
| Gemma-3-4B-it | Q4_K_M | ~2.3 GB | ~3.3 GB | no (tools not delivered) | n/a | verified 2026-08-15 (eval 0/9) |
| Phi-4-mini-instruct | Q4_K_M | ~2.3 GB | ~3.3 GB | no (tools not delivered) | n/a | verified 2026-08-15 (eval 0/9) |
| SmolLM2-1.7B-Instruct | Q4_K_M | ~1.0 GB | ~2.0 GB | no (tools not delivered) | n/a | verified 2026-08-15 (eval 0/9) |

Est. RAM uses the serve.sh warning's own arithmetic: file size plus
128 KiB per context token of KV and compute allowance at 8192 ctx
(about 1.0 GB). Every row ran the same nine-task eval on 2026-08-15
under identical conditions: compact profile, llama.cpp
`server-b10438` (digest `sha256:190813e8...`), ctx 8192, `--jinja`,
`EVAL_MAX_TOKENS` 3072, and a pod created with `--network none`. Each
model ran the nine tasks TWICE and both scores are shown; a third run
is taken only where two runs differ by 2 or more tasks. The three
rows that deliver no tools ran once, since a second 0/9 measures the
same template.

These numbers are not comparable to the table published on 2026-08-12.
Three things changed between the two passes: the server build, the
per-turn completion budget (`max_tokens` 2048 to 3072), and the wording
of eval tasks 2 and 9. A row that moved could have moved for any of
those reasons. Round two is a new baseline, not a delta against the old
one.

Read a score as one sample, not a constant, and read the PAIR before
reading either number. Under fixed conditions Qwen2.5-Coder-3B scored
6/9 then 9/9 and Qwen3-4B-Thinking 7/9 then 9/9. Two more models held
their score while the underlying tasks moved: Qwen2.5-Coder-1.5B
scored 4/9 twice with only two of nine tasks passing both times, and
Qwen3-1.7B scored 7/9 twice failing a different pair each run. Only
Qwen3-0.6B repeated its exact task set. A one-task difference between
two rows here is not a real difference, and a single run locates a
model to within roughly two tasks.

Since 2026-08-15 the two tasks that phrased their target as a
placeholder name the value indirectly instead ("the text that follows
`token: ` on the line you just read"), so no literal decoy appears in
either prompt. Earlier scores in this table's 2026-08-12 edition
included models copying that placeholder; these do not.

Three families score 0/9 for a reason that is not about the models:
llama.cpp `--jinja` silently drops the TOOLS array for gemma-3,
Phi-4-mini and SmolLM2, because their bundled chat templates have no
tool-call support. Measured by sending one request three ways and
comparing prompt tokens: with a system message plus one tool schema,
with the system message alone, and with neither. For those three the
first two are byte-identical in token count (gemma-3 28/28,
Phi-4-mini 22/22, SmolLM2 35/35), while Qwen3-1.7B goes 207/30 and
Llama-3.2-3B 240/52. The system message arrives in every case; only
the tools vanish, the server returns HTTP 200, and nothing warns. Those
models are never told tools exist, and they answer accordingly, so they
invent shapes like `{"tool": "file_delete", "path": "obsolete.tmp"}`.
A different chat template would be needed, and the eval harness has no
knob for one.

Llama-3.2-3B is not in that category and its 2/9 is its own story,
with two independent causes. It receives the full tool array, and
llama.cpp's own tool-call grammar then rejects the model's output
server-side with `The model produced output that does not match the
expected peg-native format`, upstream of anything temur parses. That
accounted for nine of its failures on 2026-08-15.

The second cause is visible only with the session store mounted, which
the harness now does for failed tasks. The model also emits well-formed
tool calls whose scalar arguments are stringified: an otherwise perfect
`edit` call carrying `"replaceAll": "false"`, the JSON string rather
than the boolean. temur answers `invalid type: string "false", expected
a boolean`, the model resends the identical call, and the repeat guard
stops it at three. Sixteen such rejections were recorded across the
2026-08-15 pass, all of them booleans or `u64` counts sent as strings,
and Qwen2.5-Coder-1.5B produces them too. This is a temur-side
tolerance question rather than a model verdict, and it is queued.

Qwen2.5-Coder-3B is the row that changed most across milestones, from
`0/7` to 8/9 on 2026-08-12 and to 6/9 and 9/9 on 2026-08-15, and the
reason for the first jump is a temur change rather than a model one
(the spread within round two is sampling noise). It always picked
the right tool and always wrote the call as plain text; T19's
prose-call recovery now EXECUTES such a call when the message is a bare
JSON object or a bare fenced block, and the transcripts show the notice
each time (`prose-call recovery: executed the bash tool call the model
wrote as plain text`). The same feature explains its 1.5B sibling's
lower score: that model writes the identical JSON behind a sentence of
preamble, which the recovery deliberately does not accept, so those
calls neither run nor prompt a retry.

Qwen3-4B-Instruct-2507 is the primary recommendation and the baked
default that `temur init` writes, because it is the only model that
swept 9/9 in both runs and it was several times faster per task than
the 1.7B. Qwen3-4B-Thinking-2507 reaches the same 9/9 ceiling and is
the same size, but took roughly twelve times as long over the same
nine tasks, so it buys nothing here for a large latency cost; prefer
the Instruct variant unless a task actually needs the thinking budget.
Qwen3-1.7B is the low-RAM choice and remains a reasonable floor at
1.1 GB, 1.3 GB less resident than the 4B; take it when the serving
machine cannot hold the larger model, and prefer the 4B whenever it
can. Qwen2.5-Coder-3B reaches 9/9 on a good run but is the least
consistent row in the table; its 1.5B sibling now measures below the
1.7B and is no longer a recommendation at any size.
Qwen3-0.6B fits almost anywhere and degrades roughly as advertised: it
passes the single-call tasks and fails the multi-step ones, though on
2026-08-15 it did pass the indirect-selection probe in both runs,
where the 2026-08-12 pass had it name the correct `bash` command and
then decline to run it.

Download source: every Q4_K_M quant measured above came from the
community `unsloth/…-GGUF` repositories on Hugging Face (e.g.
`unsloth/Qwen3-1.7B-GGUF`, `unsloth/gemma-3-4b-it-GGUF`); the official
`Qwen/Qwen3-1.7B-GGUF` repo publishes Q8_0 only (1.83 GB), a fine
larger-footprint alternative.

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
| Tool definitions silently dropped (`--jinja`, template without tool support) | Nothing on the wire says so; `temur doctor` detects it (see below) and WARNs |

**The tools-drop quirk is the one that looks like a bad model.** When
llama.cpp runs `--jinja` against a chat template with no tool support,
the tools array is dropped: HTTP 200, no log line, no response signal,
and a model that was never told tools exist. It answers in prose, or
invents shapes like `{"name": "delete", "arguments": {...}}`, and the
session reads as a model that cannot follow instructions.

`temur doctor` diagnoses it for the active selection on a keyless
local endpoint: it sends one tiny completion twice, bare and with a
single probe tool, and compares the reported prompt tokens. Identical
counts mean the array went nowhere and doctor WARNs; differing counts
PASS. Re-confirmed on `b10423-a94d563ed` on 2026-08-14 (gemma-3-4b
10/10, Phi-4-mini 4/4, SmolLM2 31/31 prompt tokens with and without
tools, against a Qwen3-4B control that moved), so it is current
behavior, not a fixed historical quirk. Tracked upstream at
ggml-org/llama.cpp#27129. The probe's own WARN was confirmed live on
2026-08-15 across ten served models, reproducing those three counts
exactly on a different server build. The fix is a chat template with
tool support, or a model whose bundled one has it.

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
nothing ever pulled; musl binary readelf-checked), then nine fixed
tasks, each in a fresh work directory with a fresh temur process: a
plain file write, a read-and-extract, a targeted edit that must leave
the rest of the file unchanged, a bash mkdir+write, a search across
three files, an edit-then-bash chain where order matters, an
indirect-tool-selection probe ("delete the file", naming no tool: the
registry has no delete tool, so the model must choose bash by itself),
a gzip binary-format nudge (a valid `.gz` must be produced through a
scripted bash run, proven by host-side `gunzip`, never by writing raw
bytes), and a large-output tail task (a needle on the final line of an
oversized tool output survives only through the head+tail truncation).
Every task is scored by a host-verified filesystem assertion (model
prose is never evidence; the indirect probe additionally requires a bash
`rm` call in the transcript), and the run ends with a fixed-width
PASS/FAIL table plus a `SCORE: N/9` line.

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
