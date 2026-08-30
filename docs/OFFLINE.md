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
the RAM warning reads; default `/proc/meminfo`), `CHAT_TEMPLATE_FILE`
(serve a `.jinja` chat template INSTEAD of the model's bundled one).

> **`CHAT_TEMPLATE_FILE` is a diagnostic knob, not a fix.** A template
> the model was not trained on can produce confident, wrong output: see
> "Substitute chat template (not comparable)" below, where the same
> template that took two models off 0/9 left a third at 0/9 while it
> spent minutes per task inventing tool results that never happened.
> Both scripts print a loud banner the whole time it is set, and a
> running server keeps the template it was started with (asking for a
> different one fails, the same way asking for a different model does).

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
window that is a real tax before the conversation even starts: measured
against a live llama.cpp server, the full profile spends **6,991 prompt
tokens** before the task is read, which is 57% of a 12288-token window.
The compact profile spends **2,763**.

```json
{ "prompt_profile": "compact" }
```

(top-level, next to `provider`) swaps in hand-trimmed descriptions for
the largest tools (bash, todowrite, edit) and a shorter default system
prompt, bringing total tool text under 8 KB. Tool set, order, and input
schemas are identical in both profiles (only the description text
varies), and an explicit `system_prompt` in config always wins over
either default.

### Auto-selection (the default since v0.30.0)

`prompt_profile` takes three values: `"auto"`, `"full"`, `"compact"`.
Absent means `"auto"`, and **auto is the default**.

Auto is one rule, and it reads exactly one thing:

| `context_window` | Profile chosen |
| --- | --- |
| set, below 20480 | `compact` |
| set, 20480 or above | `full` |
| not configured | `full` |

An unconfigured window resolves to `full` on purpose: guessing smaller
would trim the descriptions on a model that never needed it. Note that
this means auto only works where a window is configured, which for a
local server is what `temur init` writes from the server's `/props`
allocation.

When auto picks compact, temur says so once at startup:

```
  [!] prompt profile: compact (context_window 12288 is below 20480; set prompt_profile to "full" to override)
```

Nothing is printed when auto picks full. `/status` distinguishes the
two sources: `prompt: compact (auto)` versus a configured
`prompt: compact`.

An explicit `"full"` or `"compact"` is **never second-guessed** at any
window, and anything but those three spellings is a startup config
error. That was the whole contract before v0.30.0, when the field was
explicit-only and temur never inferred a profile from `context_window`;
what changed is only what an ABSENT field means.

**Upgrading from 0.29.x or earlier:** if your config sets a
`context_window` below 20480 and no `prompt_profile`, you now get the
compact descriptions where you used to get the full ones. Add
`"prompt_profile": "full"` to keep the old behavior.

**The threshold moved in v0.30.1**, from 16384 to 20480. 16384 sat
below temur's own full-profile floor, so a 16384 window (exactly what
`temur init` writes from a 16k llama.cpp server) got `full` from the
rule and a `doctor` WARN against it in the same run. 20480 is the
smallest round window where the full floor stays under that WARN line:
34% of it measured, 35% estimated. Windows from 16384 to 20479 with no
`prompt_profile` move to compact in v0.30.1, which is the better trade
there anyway (compact leaves 13.6k tokens of a 16384 window for the
task where full leaves 9.4k).

Named profiles can each carry their own `prompt_profile` (same three
values; absent = the global setting above), and `"auto"` resolves
against THAT profile's own `context_window`, so one config can hold a
small local server and a large hosted model and get the right answer
for each without naming either. Explicit values still pair the obvious
way:

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
live value as `prompt: full|compact`, with `(auto)` appended when the
window rule chose it, and a switch that lands on an auto-chosen compact
profile prints the same one-line notice startup does); an explicit
`system_prompt` override still wins in both profiles, and a raw-id
switch (`/model <model-id>`) never changes the prompt profile.

`temur doctor` reports what the active profile actually costs; see
USAGE.md, "The prompt floor".

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
| Qwen3-4B-Thinking-2507 | Q4_K_M | ~2.4 GB | ~3.4 GB | yes | yes | verified 2026-08-15 (eval 7/9, 9/9, 9/9) |
| Qwen2.5-Coder-3B-Instruct | Q4_K_M | ~1.9 GB | ~2.9 GB | via prose recovery | yes | verified 2026-08-15 (eval 6/9, 9/9, 7/9) |
| Qwen3-1.7B (low-RAM floor) | Q4_K_M | ~1.1 GB | ~2.1 GB | yes | yes | verified 2026-08-15 (eval 7/9, 7/9) |
| Qwen3-0.6B | Q4_K_M | ~0.4 GB | ~1.4 GB | degraded | yes | verified 2026-08-15 (eval 5/9, 5/9) |
| Qwen2.5-Coder-1.5B-Instruct | Q4_K_M | ~0.9 GB | ~1.9 GB | intermittent | 1 of 2 runs | verified 2026-08-15 (eval 4/9, 4/9) |
| Llama-3.2-3B-Instruct | Q4_K_M | ~1.9 GB | ~2.9 GB | unreliable | no | re-measured 2026-08-16 on v0.22.0, different binary from the rows above (eval 4/9, 3/9; was 2/9, 2/9 on 2026-08-15) |
| Gemma-3-4B-it | Q4_K_M | ~2.3 GB | ~3.3 GB | not delivered by its template | n/a | verified 2026-08-15 (eval 0/9) |
| Phi-4-mini-instruct | Q4_K_M | ~2.3 GB | ~3.3 GB | not delivered by its template | n/a | verified 2026-08-15 (eval 0/9) |
| SmolLM2-1.7B-Instruct | Q4_K_M | ~1.0 GB | ~2.0 GB | not delivered by its template | n/a | verified 2026-08-15 (eval 0/9) |

The last three rows say "not delivered by its template", not "cannot
call tools", and the distinction is not pedantic: **the tools never
reached two of those three models, and when they do, two of the three
score.** See "Substitute chat template" below.

Est. RAM uses the serve.sh warning's own arithmetic: file size plus
128 KiB per context token of KV and compute allowance at 8192 ctx
(about 1.0 GB). Every row ran the same nine-task eval on 2026-08-15
under identical conditions: compact profile, llama.cpp
`server-b10438` (digest `sha256:190813e8...`), ctx 8192, `--jinja`,
`EVAL_MAX_TOKENS` 3072, and a pod created with `--network none`. Each
model ran the nine tasks TWICE and every score is shown; where the two
runs differed by 2 or more tasks a THIRD run was taken (2026-08-16, on
the same binary, server and settings) and it is shown too. The three
rows that deliver no tools ran once, since a second 0/9 measures the
same template.

One row is deliberately outside that sentence. Llama-3.2-3B was
re-measured on 2026-08-16 against a LATER temur binary, the one
carrying T33's tolerant scalar coercion, because that fix was written
for a defect only this model exhibited and the point was to measure it.
Server build, ctx, profile, `max_tokens`, seeds and task wording are
unchanged, so the only difference from its 2026-08-15 pair is the
temur binary and the per-task bound described below; but its numbers
are a two-sample comparison against a two-sample baseline, and the
paragraph on scalar coercion further down says what actually moved and
what did not. Every other row is still the 2026-08-15 measurement.

Since 2026-08-16 `EVAL_TASK_TIMEOUT` is enforced (T33) where it
previously bound nothing, so a task can now be killed at the cap. The
default is 1200s and no task in any published row here has approached
it: the slowest legitimate task ever observed took 994s, which is the
figure the default is set above. No score in this table was truncated
by the bound.

The Llama re-measure's own slowest task, 434s, is deliberately NOT
offered as a second data point (corrected 2026-08-17, having first been
written as one): that task spent its time in an unguarded loop of 77
tool calls that ended at the context window, not in work, so it says
nothing about how long a real task needs.

These numbers are not comparable to the table published on 2026-08-12.
Three things changed between the two passes: the server build, the
per-turn completion budget (`max_tokens` 2048 to 3072), and the wording
of eval tasks 2 and 9. A row that moved could have moved for any of
those reasons. Round two is a new baseline, not a delta against the old
one.

Read a score as one sample, not a constant, and read the whole row
before reading any single number. Under fixed conditions
Qwen2.5-Coder-3B scored 6/9, 9/9 and 7/9 across three runs, three
different values spanning 3 tasks, and the third run landed between
the first two rather than settling them. Qwen3-4B-Thinking went 7/9
then 9/9 twice, which does settle: 9/9 is its level and the 7/9 was
the outlier. Two more models held their score while the underlying
tasks moved: Qwen2.5-Coder-1.5B scored 4/9 twice with only two of nine
tasks passing both times, and Qwen3-1.7B scored 7/9 twice failing a
different pair each run. Only Qwen3-0.6B repeated its exact task set.
A one-task difference between two rows here is not a real difference,
and a single run locates a model to within roughly two tasks.

Since 2026-08-15 the two tasks that phrased their target as a
placeholder name the value indirectly instead ("the text that follows
`token: ` on the line you just read"), so no literal decoy appears in
either prompt. Earlier scores in this table's 2026-08-12 edition
included models copying that placeholder; these do not.

Three families score 0/9 for a reason that is not about the models:
llama.cpp `--jinja` silently drops the TOOLS array for gemma-3,
Phi-4-mini and SmolLM2, because their bundled chat templates do not
expose tool support in the way the standard convention requires.
Measured by sending one request three ways and
comparing prompt tokens: with a system message plus one tool schema,
with the system message alone, and with neither. For those three the
first two are byte-identical in token count (gemma-3 28/28,
Phi-4-mini 22/22, SmolLM2 35/35), while Qwen3-1.7B goes 207/30 and
Llama-3.2-3B 240/52. The system message arrives in every case; only
the tools vanish, the server returns HTTP 200, and nothing warns. Those
models are never told tools exist, and they answer accordingly, so they
invent shapes like `{"tool": "file_delete", "path": "obsolete.tmp"}`.

**A 0/9 here is a statement about the template, not about the model.**
An experiment on 2026-08-17 served each of the three a substitute
template and re-ran the same nine tasks; two of them came off zero. The
per-model causes, as far as they are known:

- **Phi-4-mini** - a defect in its own bundled template, and the
  clearest case. The template does have a tool branch, but the branch
  reads a per-message `tools` key
  (`{% if message['role'] == 'system' and 'tools' in message ... %}`)
  and never the top-level `tools` variable that every standard pipeline
  passes: `apply_chat_template(..., tools=...)`, llama.cpp `--jinja`,
  vllm. So the template renders BYTE-IDENTICALLY with and without
  tools, llama.cpp's capability probe concludes `supports_tools: false`,
  and the array is dropped. A report to the model publisher is drafted
  (2026-08-18) but not yet filed; the defect is still present in the
  published `tokenizer_config.json` as of 2026-08-18.
- **SmolLM2-1.7B** - its template has no tool branch at all. Nothing is
  broken; the capability is simply absent, which is why a template that
  has one is enough to get it calling.
- **gemma-3-4b** - unresolved. It stayed at 0/9 with ZERO tool calls
  even under a substitute template that worked for the other two, so
  whatever is in its way is not only the delivery problem. That one is
  still open.

### Substitute chat template (not comparable)

One run each, 2026-08-17, temur 0.22.0, llama.cpp `server-b10438`, ctx
8192, compact profile, `EVAL_MAX_TOKENS` 3072, serving
`Qwen-Qwen2.5-7B-Instruct.jinja` (taken from llama.cpp's own
`models/templates/` at that tag) via `CHAT_TEMPLATE_FILE` instead of
each model's bundled template:

| Model | Native template | Substitute template | Native tool calls in the run |
|---|---|---|---|
| Phi-4-mini-instruct | 0/9 | **4/9** | 419 |
| SmolLM2-1.7B-Instruct | 0/9 | **2/9** | 63 |
| Gemma-3-4B-it | 0/9 | 0/9 | 0 |

**These numbers are NOT comparable to the matrix above.** They are a
different prompt encoding with different failure modes, one run each
rather than two or three, and run-to-run variance is this instrument's
own headline finding: two models changed score between consecutive runs
under fixed conditions in the 2026-08-15 matrix. Read the table as
"these models can drive tools once the tools reach them", not as a
ranking.

The passes are earned, not rescued: Phi-4-mini's four came from 419
ordinary structured tool calls parsed by llama.cpp, with temur's
prose-call recovery executing exactly once in the whole run. But the
model pays for the foreign encoding the entire time. Phi has single-token
markers for its own turn boundaries and none for ChatML's, so
`<|im_end|>` never stops generation, and four of the nine tasks ran past
350 seconds with the model writing imaginary user turns until it hit
`max_tokens`.

And gemma-3-4b is the warning label. Under the same substitute template
it produced zero tool calls, zero recoveries, and spent 150-430 seconds
per task generating confident, entirely fabricated tool results,
including the contents of a `README.md` that does not exist. The knob
that moved two models off zero turned the third's *silent* failure into
an *expensive* one. This is why both scripts print a loud banner
whenever a substitute template is active.

Llama-3.2-3B is not in the tools-dropped category, and the 2/9 pair it scored on
2026-08-15 is its own story, with two independent causes. It receives the full tool array, and
llama.cpp's own tool-call grammar then rejects the model's output
server-side with `The model produced output that does not match the
expected peg-native format`, upstream of anything temur parses. That
accounted for nine of its failures on 2026-08-15.

The second cause is visible only with the session store mounted, which
the harness now does for failed tasks. The model also emits well-formed
tool calls whose scalar arguments are stringified: an otherwise perfect
`edit` call carrying `"replaceAll": "false"`, the JSON string rather
than the boolean. temur answered `invalid type: string "false",
expected a boolean`, the model resent the identical call, and the
repeat guard stopped it at three. Sixteen such rejections were recorded
across the 2026-08-15 pass, all of them booleans or `u64` counts sent
as strings, and all of them this model. No other model in the matrix
produced one.

That was a temur-side tolerance question rather than a model verdict,
and T33 answered it: the four non-string scalar arguments in the tool
schemas now accept `"true"`/`"false"` for a boolean and a digit string
for a count, at the parse boundary only. Re-measured on 2026-08-16
against the fixed binary, the same enumeration over the archived
session JSONs returns ZERO stringified-scalar rejections against the
earlier sixteen.

What that did NOT do is make this model reliable, and the re-measure is
worth reading carefully. Its score moved from 2/9, 2/9 to 4/9, 3/9,
which is two samples against two and overlaps once. The server-side
grammar rejections above are untouched (nine then, eight now) and still
account for most of the row. And the coercion moved one failure rather
than removing it: every `offset` this model sent in the re-measure was
a string, and while `"1"`, `"2"` and `"null"` now parse and run, the
nineteen `"0"`s now parse and then fail the read tool's own range check
(`offset must be greater than or equal to 1`, since offsets are
1-indexed). A type rejection became a range rejection on that subset.
The model is still wrong about the value; temur is no longer wrong
about the type.

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
local endpoint: it sends one tiny completion twice, bare and carrying
the tool definitions this session would really send, and compares the
reported prompt tokens. Identical counts mean the array went nowhere
and doctor WARNs; differing counts PASS. Re-confirmed on
`b10423-a94d563ed` on 2026-08-14 (gemma-3-4b 10/10, Phi-4-mini 4/4,
SmolLM2 31/31 prompt tokens with and without tools, against a Qwen3-4B
control that moved), so it is current behavior, not a fixed historical
quirk. Tracked upstream at ggml-org/llama.cpp#27129. The probe's own
WARN was confirmed live on 2026-08-15 across ten served models,
reproducing those three counts exactly on a different server build. The
fix is a chat template with tool support, or a model whose bundled one
has it.

**The probe carries the real definitions for a reason.** It used to
send one small synthetic tool, and on 2026-08-17 that made it report
PASS against a server that then returned HTTP 400 on every actual
request: the template could render a toy schema and threw on temur's
own. So there is a third answer besides drop and PASS, and it is the
one you get when a template cannot render what temur sends:

```
WARN: the server at http://127.0.0.1:8080/v1 rejected temur's tool definitions for "local-gguf" (HTTP 400: <the server's own message>): every turn that sends tools will fail the same way
```

Unlike the drop, that one will not be silent in use; every turn dies
there. It is still a WARN, never a FAIL.

One cost to expect: the second request makes the server prefill every
tool definition, about 24KB on the full prompt profile. On a CPU-only
local server that took 106 seconds the first time (measured 2026-08-18,
4814 prompt tokens at 22.6 ms/token). Doctor says so before it goes
quiet. It is the same prefill the session's first real turn would pay
for the same bytes, and llama.cpp's prompt cache means a second run,
including that first turn, comes back quickly.

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
PASS/FAIL table plus a `SCORE: N/9` line. A task killed by the per-task
timeout is a FAIL carrying a `TIMEOUT@<n>s` note in that table's last
column, so an overrun is never mistaken for an ordinary failure.

```sh
MODEL_GGUF=/path/to/model.gguf scripts/weak_model_eval.sh
```

Knobs: `MUSL_BIN`, `LLAMA_IMAGE`, `CTX` (default 8192), `PROMPT_PROFILE`
(default `compact`, written into the generated keyless config),
`EVAL_TASK_TIMEOUT` (seconds per task, enforced, default 1200; `0`
disables it, and a task the bound kills is recorded FAIL with a
`TIMEOUT@<n>s` note), `EVAL_MIN` (default 0 = informational; a nonzero
value makes the script exit 1 below that score), and
`EVAL_TRANSCRIPT_DIR` (per-task transcripts are kept there for
debugging). Also `CHAT_TEMPLATE_FILE`, with the warning above: the
template in force is written into the run banner, the summary, and a
header line on every archived `results.run<r>.txt`, by path AND sha256,
so a results file found on its own still identifies the exact template
bytes it was measured under. (A path alone would not: these files get
fetched at a tag and hand-edited while a recipe is being found.)

`scripts/offline_demo.sh` deliberately has NO template knob. It is a
fixed acceptance demo on a known-good model, where the only thing a
substitute template could do is break a proof.

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
