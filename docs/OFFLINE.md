# Offline operation

Offline is the mode temur is built for. One static binary with zero
runtime dependencies, pointed at a local inference server, is a
complete AI agent with no internet anywhere in the loop: air-gapped
labs, regulated networks, ships, field sites, or a laptop on a plane.

No useful LLM runs *on* a 32-bit or embedded box.
temur runs where the code lives (the constrained device) and the model
serves from a capable machine, either the same host (a modern workstation
with no internet) or elsewhere on the LAN. Everything below assumes that
shape.

## Quickstart: llama.cpp

llama.cpp's `llama-server` speaks the OpenAI-compatible API temur's
`openai-compat` provider targets.

Three ways to start it. Pick one; they all serve the same API on the
same port, and two at once fight over it.

**Native**, using `llama-server` from a llama.cpp release binary or your
own build:

```sh
llama-server -m /path/to/model.gguf -c 8192 --jinja --port 8080
```

**Container** (the repo pins `server-b10438`; the tag scheme is
`server-b<build>`; never track `latest`):

```sh
podman run --rm -p 127.0.0.1:8080:8080 \
    -v /path/to/model.gguf:/model.gguf:ro \
    ghcr.io/ggml-org/llama.cpp:server-b10438 \
    -m /model.gguf -c 8192 --jinja --host 0.0.0.0 --port 8080
```

**One window** (checkout only): `scripts/serve.sh` runs the container
form detached, so the server and temur share one terminal:

```sh
scripts/serve.sh start           # lone .gguf in MODELS_DIR auto-selected
scripts/serve.sh start qwen3-4b  # pick by name from MODELS_DIR
scripts/serve.sh status          # health + summary (shows the mounted model)
scripts/serve.sh stop            # idempotent teardown
```

Model selection: the optional `start` argument matches
case-insensitively against the basenames of `$MODELS_DIR/*.gguf`
(default `$HOME/models`). An exact basename match (`name` or
`name.gguf`) wins; otherwise a unique substring match selects; zero or
several matches fail and list every candidate with its size (matches
marked when ambiguous). With no argument, a lone `.gguf` in the dir is
auto-selected; zero or several fail and list the candidates.
`MODEL_GGUF=/path/to/model.gguf` is an explicit override; combining it
with a name argument is an error. A running server keeps its current
model: `start` against a running container reports it, so switching
models is `stop` then `start <name>`.

RAM fit warning: before starting, the script compares the model file
size plus a generous context allowance (128 KiB per context token,
covering f16 KV cache and compute buffers at these defaults) against
`MemAvailable` and prints one WARN line when it does not fit. The start
proceeds anyway, because mmap'd weights can still limp along (expect
thrashing).

Knobs (env overrides): `MODEL_GGUF` (explicit path, see above),
`MODELS_DIR` (the search dir; serve.sh only, the demo and eval scripts
stay explicit),
`LLAMA_IMAGE` (the pin above; a missing image prints the exact
`podman pull` command and stops, nothing is pulled for you), `CTX`,
`PORT`/`BIND` (published host side only; the container-internal port is
always 8080, and a non-default `PORT` prints the `base_url` to set),
`CONTAINER_NAME` (default `temur-llama`), `MEMINFO` (the meminfo file
the RAM warning reads; default `/proc/meminfo`), `CHAT_TEMPLATE_FILE`
(serve a `.jinja` chat template INSTEAD of the model's bundled one).

> `CHAT_TEMPLATE_FILE` is a diagnostic knob. A template the model was
> not trained on can produce confident, wrong output: see "Substitute
> chat template (not comparable)" below, where the template that took
> two models off 0/9 left a third at 0/9 inventing tool results for
> minutes per task. Both scripts print a banner while it is set, and a
> running server keeps the template it was started with (asking for a
> different one fails, like asking for a different model).

> Pass `--jinja` for tool calls. Many chat templates need it before
> llama-server presents tool definitions; without it those models answer
> in prose. Some combinations (e.g. Qwen3 on recent llama.cpp builds)
> emit tool calls without the flag. If temur connects and never executes
> a tool, check this flag first.

`-c` sets the server-side context size in tokens; set temur's
`context_window` (below) to the same number so its advisory warnings
match.

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

Mind the context size: Ollama defaults to a small `num_ctx` regardless
of what the model supports. Raise it (e.g. `OLLAMA_CONTEXT_LENGTH=8192`,
or `num_ctx` in a Modelfile) and set temur's `context_window` to the
same value: an agent conversation with tool definitions overflows a
default window quickly, and Ollama truncates silently.

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

`/models` works here too and lists the loaded and downloaded models, the
quickest way to find the exact model id for the profile.

Reaching a Windows-host LM Studio from WSL2 varies by setup; nothing in
the repo scripts it.

- Mirrored networking (Windows 11: `networkingMode=mirrored` in
  `%USERPROFILE%\.wslconfig`, then `wsl --shutdown`): WSL2 shares the
  host's interfaces, so `http://127.0.0.1:1234/v1` works as-is.
- Classic NAT (the default on Windows 10 and unconfigured Win11): WSL2
  is a separate network. Use the Windows host's IP as seen from WSL2,
  usually the default-gateway address (`ip route show default`).
  `/etc/resolv.conf`'s nameserver is a common suggestion but lies
  whenever DNS is overridden; prefer the route. Two host-side
  requirements: LM Studio must listen on all interfaces (serve on
  network / bind `0.0.0.0`, so it is reachable beyond localhost), and
  Windows Defender Firewall must allow inbound connections to it on
  port 1234.

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
temur's default (32000) suits large cloud contexts; against an
8192-token window it lets one response outgrow the context, and the
advisory fires immediately. 1024 to 4096 is a sensible local range.

## Compact prompt profile

The stock tool descriptions are the OpenCode-ported prompts, sized for
Claude-class context windows (~24 KB of tool text). On a small local
window that is a tax before the conversation starts: measured against a
live llama.cpp server, the full profile spends 7,433 prompt tokens
before the task is read, 60% of a 12288-token window. The compact
profile spends 3,205.

```json
{ "prompt_profile": "compact" }
```

(top-level, next to `provider`) swaps in hand-trimmed descriptions for
the largest tools (bash, todowrite, edit) and a shorter default system
prompt, bringing total tool text under 8 KB. Tool set, order, and input
schemas are identical in both profiles; only the description text
varies. An explicit `system_prompt` in config wins over either default.

### Auto-selection (the default since v0.30.0)

`prompt_profile` takes three values: `"auto"`, `"full"`, `"compact"`.
Absent means `"auto"`.

Auto is one rule on one value:

| `context_window` | Profile chosen |
| --- | --- |
| set, below 20480 | `compact` |
| set, 20480 or above | `full` |
| not configured | `full` |

An unconfigured window resolves to `full`: guessing smaller would trim
the descriptions on a model that never needed it. So auto only works
where a window is configured, which for a local server `temur init`
writes from the server's `/props` allocation.

When auto picks compact, temur says so once at startup:

```
  [!] prompt profile: compact (context_window 12288 is below 20480; set prompt_profile to "full" to override)
```

Nothing is printed when auto picks full. `/status` distinguishes the
two sources: `prompt: compact (auto)` versus a configured
`prompt: compact`.

An explicit `"full"` or `"compact"` is never second-guessed, and
anything but those three spellings is a startup config error. Before
v0.30.0 the field was explicit-only; only the meaning of an absent field
changed.

Upgrading from 0.29.x or earlier: if your config sets a
`context_window` below 20480 and no `prompt_profile`, you now get the
compact descriptions where you used to get the full ones. Add
`"prompt_profile": "full"` to keep the old behavior.

The threshold moved in v0.30.1, from 16384 to 20480. 16384 sat below
temur's own full-profile floor, so a 16384 window (what `temur init`
writes from a 16k llama.cpp server) got `full` from the rule and a
`doctor` WARN against it in the same run. 20480 is the smallest round
window where the full floor stays under that WARN line: 36% of it
measured, 37% estimated. Windows from 16384 to 20479 with no
`prompt_profile` move to compact in v0.30.1, the better trade there
anyway (compact leaves 13.2k tokens of a 16384 window for the task where
full leaves 9.0k).

Named profiles can each carry their own `prompt_profile` (same three
values; absent = the global setting above), and `"auto"` resolves
against that profile's own `context_window`, so one config can hold a
small local server and a large hosted model and get the right answer for
each without naming either. Explicit values work per profile:

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
window rule chose it, and a switch onto an auto-chosen compact
profile prints the same one-line notice startup does); an explicit
`system_prompt` override still wins in both profiles, and a raw-id
switch (`/model <model-id>`) never changes the prompt profile.

`temur doctor` reports what the active profile costs; see USAGE.md, "The
prompt floor".

## LAN topology

```
constrained box (i686/ARM/router/…)          capable machine (x86_64, GPU…)
┌──────────────────────────────┐             ┌─────────────────────────────┐
│ temur (musl-static, ~6 MB)   │  HTTP LAN   │ llama-server -c 8192 --jinja │
│ + your code                  │────────────▶│ + model.gguf                │
└──────────────────────────────┘   :8080     └─────────────────────────────┘
```

No internet is required on either side. The constrained box needs only
the temur binary and your working tree; the model machine needs only
llama.cpp (or Ollama) and a `.gguf` file.

## Recommended small models

Small-model tool-calling has a floor. These are the smallest models
observed to drive temur's tools with acceptable reliability, and the
smallest is not the best. "Tool calls" means the model reliably emits
structured tool calls when told which tool to use; "indirect selection"
means it picks the right tool on its own when the task does not name one
(the weak-model eval's task 7). "Verified" rows ran the full eval
harness on the stated date. Every row is a measurement; nothing is
carried over from earlier observation.

temur can also read a running server's model listing: `temur init`'s
local template offers the served models as a numbered pick (a two-row
summary of this table prints only when no server answers), and `temur
doctor` warns when a configured model is not in the listing. Both use
one unauthenticated GET, against keyless endpoints only.

| Model | Quant | File size | Est. RAM at 8k ctx | Tool calls | Indirect selection | D22 file denial | Status |
|---|---|---|---|---|---|---|---|
| **Qwen3-4B-Instruct-2507** (primary) | Q4_K_M | ~2.4 GB | ~3.4 GB | yes | yes | not measured | verified 2026-08-15 (eval 9/9, 9/9) |
| Qwen3-4B-Thinking-2507 | Q4_K_M | ~2.4 GB | ~3.4 GB | yes | yes | not measured | verified 2026-08-15 (eval 7/9, 9/9, 9/9) |
| Qwen2.5-Coder-3B-Instruct | Q4_K_M | ~1.9 GB | ~2.9 GB | via prose recovery | yes | not measured | verified 2026-08-15 (eval 6/9, 9/9, 7/9) |
| Qwen3-1.7B (low-RAM floor) | Q4_K_M | ~1.1 GB | ~2.1 GB | yes | yes | not measured | verified 2026-08-15 (eval 7/9, 7/9) |
| Qwen3-0.6B | Q4_K_M | ~0.4 GB | ~1.4 GB | degraded | yes | not measured | verified 2026-08-15 (eval 5/9, 5/9) |
| Qwen2.5-Coder-1.5B-Instruct | Q4_K_M | ~0.9 GB | ~1.9 GB | intermittent | 1 of 2 runs | not measured | verified 2026-08-15 (eval 4/9, 4/9) |
| Llama-3.2-3B-Instruct | Q4_K_M | ~1.9 GB | ~2.9 GB | unreliable | no | not measured | re-measured 2026-08-16 on v0.22.0, different binary from the rows above (eval 4/9, 3/9; was 2/9, 2/9 on 2026-08-15) |
| Gemma-3-4B-it | Q4_K_M | ~2.3 GB | ~3.3 GB | not delivered by its template | n/a | not measured | verified 2026-08-15 (eval 0/9) |
| Phi-4-mini-instruct | Q4_K_M | ~2.3 GB | ~3.3 GB | not delivered by its template | n/a | not measured | verified 2026-08-15 (eval 0/9) |
| SmolLM2-1.7B-Instruct | Q4_K_M | ~1.0 GB | ~2.0 GB | not delivered by its template | n/a | not measured | verified 2026-08-15 (eval 0/9) |
| Llama-3.1-8B-Instruct | Q4_K_M | ~4.9 GB | ~5.9 GB | emitted, but the server rejected 4 of 9 | yes | not measured | measured 2026-09-09 on head `8ea2aa3`, a different binary from every row above (eval 5/9, 5/9; identical failing set both runs) |
| gpt-oss-20b | MXFP4 | ~12.1 GB | ~13.1 GB | yes | yes | not measured | measured 2026-09-09 on head `f7ab9df` (T57 P1), musl i686 sha256 `76c8e33e...` (eval 9/9, 9/9); scored 0/9, 0/9 on the parent binary, whose tool definitions its template could not render, see below |
| **Qwen3-4B-Instruct-2507** (T58 re-measure) | Q4_K_M | ~2.4 GB | ~3.4 GB | yes | yes | 2/2, both via the nudge | measured 2026-09-09 on head `0cfc653` (T58 P2), musl i686 sha256 `596e3e43...` (eval 8/9, 9/9); the one miss is task 5 in run 1, which is the glob shape described below and not a regression |

The rows that say "not delivered by its template" mean the tools never
reached those models. Some of them score when the tools do reach them.
See "Substitute chat template" below.

Est. RAM uses the serve.sh warning's own arithmetic: file size plus
128 KiB per context token of KV and compute allowance at 8192 ctx
(about 1.0 GB). Every row ran the same nine-task eval on 2026-08-15
under identical conditions: compact profile, llama.cpp
`server-b10438` (digest `sha256:190813e8...`), ctx 8192, `--jinja`,
`EVAL_MAX_TOKENS` 3072, and a pod created with `--network none`. Each
model ran the nine tasks twice and every score is shown; where the two
runs differed by 2 or more tasks a third run was taken (2026-08-16, on
the same binary, server and settings) and it is shown too. The three
rows that deliver no tools ran once, since a second 0/9 measures the
same template.

One row is outside that sentence. Llama-3.2-3B was re-measured on
2026-08-16 against a later temur binary, the one carrying T33's tolerant
scalar coercion, because that fix addressed a defect only this model
exhibited. Server build, ctx, profile, `max_tokens`, seeds and task
wording are unchanged, so the only differences from its 2026-08-15 pair
are the temur binary and the per-task bound described below. Its numbers
are a two-sample comparison against a two-sample baseline; the paragraph
on scalar coercion further down says what moved and what did not. Every
other row is the 2026-08-15 measurement.

Since 2026-08-16 `EVAL_TASK_TIMEOUT` is enforced (T33); before that it
bound nothing. The default is 1200s, set above the slowest legitimate
task ever observed (994s), and no task in any published row has
approached it. No score in this table was truncated by the bound.

The last two rows are a later pass (2026-09-09) and carry the same
caveat as the Llama-3.2-3B row: a different temur binary. They sit on
two different ones. Llama-3.1-8B was measured on the local head
`8ea2aa3`, musl-static i686, sha256 `09c8fdc7...`; gpt-oss-20b was
re-measured on `f7ab9df` (T57 P1), sha256 `76c8e33e...`, which is that
binary plus the one `items` key described below and nothing else.
Server build,
image digest, ctx 8192, `--jinja`, compact profile, `EVAL_MAX_TOKENS`
3072, `EVAL_RUNS` 2 and the `--network none` pod are unchanged from the
rows above, so those two rows are comparable to each other and only
loosely to the 2026-08-15 batch. Model files measured: Llama-3.1-8B
sha256 `7b064f58...` (4,920,739,232 bytes), gpt-oss-20b sha256
`27cd6c43...` (12,109,566,624 bytes).

Llama-3.1-8B-Instruct receives the tools, and the server rejects what
the model writes back. Its template delivers the definitions (doctor's
tools-drop probe: prompt_tokens 36 without tools, 3563 with), and five
tasks pass, task 7 among them, so indirect tool selection works. Both
runs scored 5/9 and failed the SAME four tasks (2 read-extract, 5
find-needle, 6 bump-and-copy, 9 large-tail), every one of them with the
identical error, mid-stream, on the first tool call:

```
provider error: api error (HTTP 200) server_error: The model produced output that does not match the expected peg-native format
```

That is llama.cpp's own tool-call parser refusing the model's output.
temur rejected nothing, and the per-task bound played no part: the
failing tasks ran 21-38s against a 1200s bound. The measured prompt
floor is 3667 tokens, 44% of the 8192 window with the compact profile
already active.

One missing JSON Schema key stops every gpt-oss-20b turn. Every task in
both runs failed in 6-7 seconds with HTTP 500 before a token was
generated, all with the same template render error:

```
While executing If at line 12, column 13 in source:
...am_spec.type == "array" -%}  {%- if param_spec['items'] -%}
Error: Function is not a bool value
```

Isolated against the running server with single-tool requests, smallest
difference between them: no tools 200; one string parameter 200; one
array parameter WITHOUT `items` 500; the same array parameter WITH
`items` 200. temur has exactly one array schema with no `items`, the
element type of `spreadsheet`'s `rows`
(`"rows": {"type": "array", "items": {"type": "array"}}`), and sending
that exact shape alone reproduces the 500 while adding `items` to the
inner array alone returns 200. An array without `items` is valid JSON
Schema; gpt-oss's template asks `param_spec['items']` and the engine
resolves the absent key to the mapping's own `.items` method, then
refuses a function in boolean context.

The model is not the cause. The same model, template, server build and
settings render temur's full tool set on temur 0.29.1
(pre-`spreadsheet`): prompt_tokens 68 without tools, 1986 with, PASS.
So gpt-oss-20b's row measures a temur schema that only stopped working
when the `spreadsheet` tool arrived, and it says nothing yet about how
well the model drives tools. It has no score to report until that
element type carries an `items`.

`temur doctor` names this failure without any of the above: its
tools-drop probe returns `WARN: the server ... rejected temur's tool
definitions for "local-gguf" (HTTP 500: ...): every turn that sends
tools will fail the same way`, quoting the server's own words.

T57 added the key, and the row above is the result. The element type of
`spreadsheet`'s `rows` now carries an `items`, picked by measuring three
candidate shapes against three bundled templates; the one taken
describes a cell value and declares no `type`, so the schema does not
push a model toward quoting numbers. On the same model file, template,
server build and settings, gpt-oss-20b went from 0/9, 0/9 to 9/9, 9/9,
with no tool-call parser rejection in either run and no change to the
eval script. A test now walks every tool schema in both prompt profiles
and fails any array that declares no element type, naming the tool and
the JSON path.

The same change was checked against the primary row's model. Task 5
(find-needle) is the only cell that moved on Qwen3-4B, and its T57 tally
is 3 of 6 against a parent lineage of 9 of 10 (T53 parent 2/2, T53 P2
2/2, T54 P4 parent 1/2, T54 P4 P3 2/2, T57 parent control 2/2). Every
other task passed in all six runs. The reading was fixed before the
deciding samples were taken, three or more passes in four fresh runs
meaning variance, and four fresh runs returned three passes. The
transcripts say why the number is noisy: on both binaries the model
sometimes calls `glob` with the three file names joined by commas and
then reads its own successful result as "no files found", and on both it
usually recovers through `grep` and `read`. Whether it recovers is the
coin flip. Both figures are here so the tally and the lineage are read
together.

The Llama re-measure's slowest task, 434s, is not a second data point
(corrected 2026-08-17, having first been written as one): it spent its
time in an unguarded loop of 77 tool calls that ended at the context
window, so it says nothing about how long a real task needs.

These numbers are not comparable to the table published on 2026-08-12.
Three things changed between the two passes: the server build, the
per-turn completion budget (`max_tokens` 2048 to 3072), and the wording
of eval tasks 2 and 9. A row that moved could have moved for any of
those reasons.

Read a score as one sample, and read the whole row before any single
number. Under fixed conditions Qwen2.5-Coder-3B scored 6/9, 9/9 and 7/9
across three runs, and the third run fell between the first two.
Qwen3-4B-Thinking went 7/9 then 9/9 twice: 9/9 is its level and the 7/9
was the outlier. Two more models held their score while the tasks moved:
Qwen2.5-Coder-1.5B scored 4/9 twice with only two of nine tasks passing
both times, and Qwen3-1.7B scored 7/9 twice failing a different pair
each run. Only Qwen3-0.6B repeated its exact task set. A one-task
difference between two rows is not a real difference; a single run
locates a model to within roughly two tasks.

Since 2026-08-15 the two tasks that phrased their target as a
placeholder name the value indirectly ("the text that follows `token: `
on the line you just read"), so no literal decoy appears in either
prompt. Earlier scores in this table's 2026-08-12 edition included
models copying that placeholder; these do not.

Three families score 0/9 because of their chat templates: llama.cpp
`--jinja` silently drops the tools array for gemma-3, Phi-4-mini and
SmolLM2, whose bundled templates do not expose tool support the way the
standard convention requires. Measured by sending one request three ways
and comparing prompt tokens: with a system message plus one tool schema,
with the system message alone, and with neither. For those three the
first two are byte-identical in token count (gemma-3 28/28, Phi-4-mini
22/22, SmolLM2 35/35), while Qwen3-1.7B goes 207/30 and Llama-3.2-3B
240/52. The system message arrives in every case; only the tools vanish,
the server returns HTTP 200, and nothing warns. Those models are never
told tools exist, so they invent shapes like `{"tool": "file_delete",
"path": "obsolete.tmp"}`.

A 0/9 here describes the template. An experiment on 2026-08-17 served
each of the three a substitute template and re-ran the same nine
tasks; two of them came off zero. The per-model causes, as far as they
are known:

- **Phi-4-mini**: a defect in its bundled template, the clearest case.
  The template has a tool branch, but it reads a per-message `tools`
  key (`{% if message['role'] == 'system' and 'tools' in message ... %}`)
  and never the top-level `tools` variable every standard pipeline
  passes: `apply_chat_template(..., tools=...)`, llama.cpp `--jinja`,
  vllm. So the template renders byte-identically with and without
  tools, llama.cpp's capability probe concludes `supports_tools: false`,
  and the array is dropped. A report to the model publisher was filed
  2026-09-03 as
  https://huggingface.co/microsoft/Phi-4-mini-instruct/discussions/47;
  the defect was still present in the published `tokenizer_config.json`
  as of 2026-09-02.
- **SmolLM2-1.7B**: its template has no tool branch. Nothing is broken;
  the capability is absent, so a template that has one is enough to get
  it calling.
- **gemma-3-4b**: unresolved. It stayed at 0/9 with zero tool calls
  even under a substitute template that worked for the other two, so
  whatever is in its way is not only the delivery problem.

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

These numbers are not comparable to the matrix above. They are a
different prompt encoding with different failure modes, one run each
where the matrix had two or three, and run-to-run variance is this
instrument's headline finding: two models changed score between
consecutive runs under fixed conditions in the 2026-08-15 matrix. Read
the table as "these models can drive tools once the tools reach them".
It is not a ranking.

Phi-4-mini's four passes came from 419 ordinary structured tool calls
parsed by llama.cpp, with temur's prose-call recovery executing once in
the whole run. But the model pays for the foreign encoding throughout:
Phi has single-token markers for its own turn boundaries and none for
ChatML's, so `<|im_end|>` never stops generation, and four of the nine
tasks ran past 350 seconds writing imaginary user turns until
`max_tokens`.

gemma-3-4b shows the risk. Under the same substitute template it
produced zero tool calls, zero recoveries, and spent 150-430 seconds per
task generating fabricated tool results, including the contents of a
`README.md` that does not exist. The knob that moved two models off zero
turned the third's *silent* failure into an *expensive* one, which is
why both scripts print a banner whenever a substitute template is
active.

Llama-3.2-3B is not in the tools-dropped category; its 2/9 pair on
2026-08-15 has two independent causes. It receives the full tool array,
and llama.cpp's tool-call grammar then rejects the model's output
server-side with `The model produced output that does not match the
expected peg-native format`, upstream of anything temur parses. That
accounted for nine of its failures.

The second cause is visible only with the session store mounted, which
the harness now does for failed tasks. The model also emits well-formed
tool calls with stringified scalar arguments: an otherwise perfect
`edit` call carrying `"replaceAll": "false"`, the JSON string rather
than the boolean. temur answered `invalid type: string "false", expected
a boolean`, the model resent the identical call, and the repeat guard
stopped it at three. Sixteen such rejections were recorded across the
2026-08-15 pass, all booleans or `u64` counts sent as strings, and all
from this model.

T33 answered that on temur's side: the four non-string scalar arguments
in the tool schemas now accept `"true"`/`"false"` for a boolean and a
digit string for a count, at the parse boundary only. Re-measured on
2026-08-16 against the fixed binary, the same enumeration over the
archived session JSONs returns zero stringified-scalar rejections,
against sixteen before.

That did not make the model reliable. Its score moved from 2/9, 2/9 to
4/9, 3/9, two samples against two, overlapping once. The grammar
rejections above are untouched (nine then, eight now) and still account
for most of the row. The coercion also moved one failure without
removing it: every `offset` this model sent was a string, and while
`"1"`, `"2"` and `"null"` now parse and run, the nineteen `"0"`s parse
and then fail the read tool's range check (`offset must be greater than
or equal to 1`, since offsets are 1-indexed). A type rejection became a
range rejection: the model is still wrong about the value, and temur is
no longer wrong about the type.

Qwen2.5-Coder-3B is the row that changed most across milestones, from
`0/7` to 8/9 on 2026-08-12 and to 6/9 and 9/9 on 2026-08-15, and the
first jump came from a temur change (the spread within round two is
sampling noise). It always picked the right tool and wrote the call as
plain text; T19's prose-call recovery executes such a call when the
message is a bare JSON object or a bare fenced block, and the
transcripts show the notice each time (`prose-call recovery: executed
the bash tool call the model wrote as plain text`). The same feature
explains its 1.5B sibling's lower score: that model writes the identical
JSON behind a sentence of preamble, which the recovery does not accept,
so those calls neither run nor prompt a retry.

Qwen3-4B-Instruct-2507 is the primary recommendation and the default
`temur init` writes: the only model that swept 9/9 in both runs, and
several times faster per task than the 1.7B. Qwen3-4B-Thinking-2507
reaches the same 9/9 and is the same size, but took roughly twelve times
as long over the nine tasks; prefer the Instruct variant unless a task
needs the thinking budget. Qwen3-1.7B is the low-RAM choice, a
reasonable floor at 1.1 GB, 1.3 GB less resident than the 4B; take it
when the serving machine cannot hold the 4B. Qwen2.5-Coder-3B reaches
9/9 on a good run but is the least consistent row; its 1.5B sibling
measures below the 1.7B and is no longer recommended at any size.
Qwen3-0.6B fits almost anywhere and degrades as advertised: it passes
the single-call tasks and fails the multi-step ones, though on
2026-08-15 it passed the indirect-selection probe in both runs, where
the 2026-08-12 pass had it name the correct `bash` command and then
decline to run it.

Download source: every Q4_K_M quant measured above came from the
community `unsloth/…-GGUF` repositories on Hugging Face (e.g.
`unsloth/Qwen3-1.7B-GGUF`, `unsloth/gemma-3-4b-it-GGUF`); the official
`Qwen/Qwen3-1.7B-GGUF` repo publishes Q8_0 only (1.83 GB), a fine
larger-footprint alternative.

Larger is better whenever the serving machine allows it; anything in the
7B+ class changes the experience qualitatively.

## `context_window`: what it does and does not do

`openai_compat.context_window` tells temur how big the *served* context
is, a property of the server (llama.cpp `-c`, Ollama `num_ctx`) that the
OpenAI-compatible API does not expose. llama.cpp does expose it out of
band, at the server root's `/props` endpoint
(`default_generation_settings.n_ctx`), and temur reads it there with the
same unauthenticated keyless GET as the model listing: `temur init`
fills a fresh local config with the detected value (server down, or any
non-llama.cpp server, keeps the baked 8192), and `temur doctor` checks a
configured value against the live allocation, warning on a mismatch in
either direction and suggesting the exact config line when the value is
missing. Since v0.31.0 startup probes `/props` as well, but only for a
keyless openai-compat selection with no configured window: it says so
once, uses the value for that run, and writes nothing to disk. Ollama's
equivalent (`/api/show`) is deliberately not probed, so for Ollama and
LM Studio you state the value by hand. However it gets set, temur then:

- advises once per session when the conversation gets tight: at 80% of
  the window, or when the remaining room drops below `max_tokens` (the
  next response may not fit), whichever comes first; the advisory names
  `/compact` and a new session as the remedies, and also fires at
  `--continue`/`--resume`/`/resume` when the restored session is
  already past the threshold;
- rewords a `max_tokens` truncation that happens near the window to name
  the likely real cause: context overflow.

temur ships no tokenizer. The estimate is the input+output token count
of the most recent response, as reported by the server, and absent on
servers that never report usage (then the feature stays silent). That
count is one round-trip behind, so since v0.31.0 the check runs again
immediately before each request goes out, adding a rough
four-characters-per-token estimate of everything appended since; dense
content defeats that average, so it catches the ordinary large result
and misses some. That is why every figure is written `~N`.

This warning is an advisory: it never trims or blocks a request, and
`/compact` runs only because you typed it. temur does compact and cut
on its own elsewhere. Those paths are in [USAGE.md](USAGE.md):
"Auto-compaction for unattended runs" (default on in one-shot `-p`) and
"When the server rejects the request anyway" (recovery after the server
rejects an over-sized request).

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

The tools-drop quirk looks like a bad model. When llama.cpp runs
`--jinja` against a chat template with no tool support, the tools array
is dropped: HTTP 200, no log line, no response signal. The model, never
told tools exist, answers in prose or invents shapes like `{"name":
"delete", "arguments": {...}}`, and the session reads as a model that
cannot follow instructions.

`temur doctor` diagnoses it for the active selection on a keyless local
endpoint: it sends one tiny completion twice, bare and carrying the tool
definitions this session would send, and compares the reported prompt
tokens. Identical counts mean the array went nowhere and doctor WARNs;
differing counts PASS. Re-confirmed on `b10423-a94d563ed` on 2026-08-14
(gemma-3-4b 10/10, Phi-4-mini 4/4, SmolLM2 31/31 prompt tokens with and
without tools, against a Qwen3-4B control that moved). Tracked upstream
at ggml-org/llama.cpp#27129. The probe's WARN was confirmed live on
2026-08-15 across ten served models, reproducing those three counts on a
different server build. The fix is a chat template with tool support, or
a model whose bundled one has it.

The probe carries the real definitions because a synthetic one misled
it: on 2026-08-17 a one-tool toy schema reported PASS against a server
that then returned HTTP 400 on every real request, since the template
could render the toy and threw on temur's own. So there is a third
answer besides drop and PASS, for a template that cannot render what
temur sends:

```
WARN: the server at http://127.0.0.1:8080/v1 rejected temur's tool definitions for "local-gguf" (HTTP 400: <the server's own message>): every turn that sends tools will fail the same way
```

Unlike the drop, that one is not silent in use: every turn dies there.
It is still a WARN, never a FAIL.

One cost: the second request makes the server prefill every tool
definition, about 24KB on the full prompt profile. On a CPU-only local
server that took 106 seconds the first time (measured 2026-08-18, 4814
prompt tokens at 22.6 ms/token); doctor says so before it goes quiet. It
is the same prefill the session's first real turn would pay, and
llama.cpp's prompt cache makes a second run, including that first turn,
fast.

## The offline demo

`scripts/offline_demo.sh` proves the story end-to-end with zero internet
by construction: one podman pod created with `--network none` (loopback
only) holding a llama.cpp server and the musl-static temur binary, which
must then drive a real tool call. It is operator-run, outside
`check.sh`.

```sh
MODEL_GGUF=/path/to/model.gguf scripts/offline_demo.sh
```

The script never pulls images or models; preflight prints the `podman
pull` command and exits if anything is missing. It asserts the negative
(TLS to the internet must fail inside the pod) before the positive (the
model must use the bash tool to write a proof file, verified from the
host; model prose is never evidence).

## The weak-model eval

`scripts/weak_model_eval.sh` measures how well a small model drives
temur's tools. Same setup as the demo (operator-run, outside `check.sh`;
podman pod with `--network none`; nothing ever pulled; musl binary
readelf-checked), then nine fixed tasks, each in a fresh work directory
with a fresh temur process: a plain file write, a read-and-extract, a
targeted edit that must leave the rest of the file unchanged, a bash
mkdir+write, a search across three files, an edit-then-bash chain where
order matters, an indirect-tool-selection probe ("delete the file",
naming no tool: the registry has no delete tool, so the model must
choose bash by itself), a gzip binary-format nudge (a valid `.gz` must
be produced through a scripted bash run, proven by host-side `gunzip`,
never by writing raw bytes), and a large-output tail task (a needle on
the final line of an oversized tool output survives only through the
head+tail truncation). Every task is scored by a host-verified
filesystem assertion (model prose is never evidence; the indirect probe
also requires a bash `rm` call in the transcript), and the run ends with
a fixed-width PASS/FAIL table plus a `SCORE: N/9` line. A task killed by
the per-task timeout is a FAIL carrying a `TIMEOUT@<n>s` note in that
table's last column, so an overrun is never mistaken for an ordinary
failure.

A tenth task, `resume-feedback`, runs after the nine and is reported
separately. The nine are imperative tool instructions, so none of them
can see a model that refuses a request rather than mishandling it. This
one is conversational and names no tool: the work directory holds one
PDF and the prompt is "can you read my resume and give me feedback?".
It passes only if the transcript shows a `read` call on that PDF AND
the model's own prose quotes a fact out of the document, since a model
can call the tool and still answer from nothing. Its line follows the
score:

```
SCORE (run 2): 9/9
D22 (run 2): resume-feedback PASS read the pdf and cited it (144s)
```

The score keeps its denominator of nine and task 10 is never added to
it, so every row published before this task existed stays comparable.

Task 5 prints one more line after its PASS/FAIL, naming the first
`glob` pattern the model sent. A comma-joined list of the three file
names was the shape most of that task's failures had in common, and it
is chosen before any tool output exists, so the line records it:

```
task 5 (find-needle): PASS (35s)
T59 (run 2): find-needle glob=alpha.txt,beta.txt,gamma.txt comma-joined
```

`comma-joined` means a comma outside braces, `plain` any other pattern,
`none` no glob call. The line is repeated after the score and written
into the results file as a comment. Nothing is scored from it.

Two document tasks, `memo-docx` and `summary-pdf`, run after task 10
and are reported the same way. One asks for a memo saved as
`memo.docx`, the other for three points summarised into `summary.pdf`.
Each passes only if the file exists AND reading it back through
temur's own `read` tool yields a token from the prompt, so a text file
saved under the extension fails: the docx and PDF parsers reject it.
The read-back runs the `tools` test binary that
`cargo test --release --target i686-unknown-linux-musl --no-run` leaves
beside the musl binary, and the eval refuses at preflight without it.

```
T60 (run 2): memo-docx PASS read back 9:30 (24s)
T60 (run 3): summary-pdf FAIL no summary.pdf in the work dir (87s)
```

Measured on Qwen3-4B-Instruct-2507 Q4_K_M (T60, 2026-09-10 and 11):
memo-docx 0 of 3 on the binary before the writers (a text file under
the .docx name every time) and 5 of 5 after; summary-pdf 0 of 3 before,
0 of 5 with the writer alone (the model wrote its text to summary.txt
and then hunted for pdftk, pandoc or pip under bash, and never sent the
.pdf path to `write`), and 3 of 5 once a failed converter's result
names the write tool. The two remaining misses read "the three points
below" as files to find, searched the work directory, and asked for
them.

Tasks 10 to 12 are deliberately absent from
`scripts/harness_compare/tasks.sh`: they measure temur's own recovery
from a refusal and its own document writing, which no other harness
has, so a cross-harness score for them would compare nothing.

```sh
MODEL_GGUF=/path/to/model.gguf scripts/weak_model_eval.sh
```

Knobs: `MUSL_BIN`, `LLAMA_IMAGE`, `CTX` (default 8192), `PROMPT_PROFILE`
(default `compact`, written into the generated keyless config),
`EVAL_TASK_TIMEOUT` (seconds per task, default 1200; `0` disables it),
`EVAL_MIN` (default 0 = informational; a nonzero value makes the script
exit 1 below that score), `EVAL_ONLY` (a task number 1 to 10; only that
task runs, and the score line and the archived results file both carry
`EVAL_ONLY=<n>, not a published row`), and `EVAL_TRANSCRIPT_DIR`
(per-task transcripts are kept there for debugging). Also
`CHAT_TEMPLATE_FILE`, with the warning above: the template in force is
written into the run
banner, the summary, and a header line on every archived
`results.run<r>.txt`, by path and sha256, so a results file found on its
own identifies the exact template bytes it was measured under (a path
alone would not: template files get fetched at a tag and hand-edited
while a recipe is being found).

`scripts/offline_demo.sh` has no template knob. It is a
fixed acceptance demo on a known-good model, where the only thing a
substitute template could do is break a proof.

## Troubleshooting

1. **Tools never get called; the model answers in prose.** llama.cpp:
   you forgot `--jinja`. Check this before anything else.
2. **Connection refused.** Server not up, wrong port, or `base_url`
   points at the wrong host. For Ollama remember the port is 11434 and
   the path prefix is `/v1`.
3. **Responses cut off mid-thought.** `max_tokens` too small. If
   temur's notice mentions the context window instead, the conversation
   has outgrown `-c`/`num_ctx`: start a new session or serve a bigger
   window.
4. **Token counts show `—`.** The server doesn't report usage. Harmless;
   the context advisory is off in this state.
5. **First response is slow.** Model load and prompt processing on
   the server; subsequent turns reuse the loaded model.
6. **Small model loops or fumbles tool arguments.** Known floor: see
   the models table; a schema error feeding back gives the model a
   retry, but persistent loops trip temur's doom-loop guard by design.
