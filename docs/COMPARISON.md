# Comparison: temur against OpenCode and Codex CLI

## Read this first

This page compares temur against two released coding agents on two
axes: what they cost to install and run, and how well they drive a
small local model. Three things should shape how much weight you give
it.

**It was built by temur's side.** The nine tasks come from temur's own
eval suite, written months earlier to find temur's failures. A suite
written by one project and run against three favours the one that
wrote it, and no amount of care in the running removes that. Read the
scores as "how do these harnesses handle tasks temur already considers
representative", not as a general capability ranking. A neutral
third-party suite (Terminal-Bench) is queued and does not exist yet;
until it does, this page does not claim to be one.

**Delivery is pinned, not trusted.** The prompts are not copied between
scripts. `scripts/harness_compare/tasks.sh` is generated from the eval
suite, and a drift test in `scripts/check.sh` compares raw source bytes
on every gate run, so the three harnesses cannot silently diverge.

**Every cell that ran is published, losses included.** No cell was
dropped for being unflattering. Cells that could not be scored honestly
are marked VOID and quarantined rather than re-run until they looked
better. In the published matrices there are none.

What this is not: a comparison against frontier models. No hosted
provider and no API key was used anywhere in it. The question is
narrow on purpose. Given the same small model on the same machine, how
much does the harness around it matter?

## What was pinned

| Thing | Value |
| --- | --- |
| temur | 0.25.0 (x86_64 static-pie build for all runs) |
| OpenCode | 1.18.21 (glibc asset; see footprint note) |
| Codex CLI | 0.149.0 |
| Server | `ghcr.io/ggml-org/llama.cpp:server-b10438` |
| Server digest | `sha256:190813e82f33a82f506e66826f367004a3159f8b8139b11d07566437aecdac93` |
| Qwen3-4B-Instruct-2507-Q4_K_M | `3605803b982cb64aead44f6c1b2ae36e3acdb41d8e46c8a94c6533bc4c67e597` |
| Qwen2.5-Coder-3B-Instruct-Q4_K_M | `32f0014400ca1c1f81e7fb5befa9b9af476ba967dcbf92bad27409228c57c5b4` |
| Context | 12288, `--parallel 1 --jinja` |
| Host | one x86-64 machine, 7.61 GiB RAM, for all three harnesses |

## Method

Each harness gets a fresh git-initialised working directory per task
and is pinned explicitly to the local model. Two runs per cell; a third
runs when the spread is 2 or more, judged within a cell and never
across harnesses. No third run was triggered in either matrix.

**A fresh server per task.** The server is restarted before every task,
which is not free and is disclosed rather than hidden: restarting
forfeits llama.cpp's cross-task prefix cache, so each task pays its own
full prefill. That cost lands on the harness whose prompt it is, which
is the measurement this page exists to make. Published durations
exclude server-ready time, recorded separately at roughly one second
per restart (the gguf is in page cache) and under ten seconds per cell.

The reason is worth stating with its limit. Under a per-cell server the
kernel OOM-killed llama-server six times on this machine; memory
climbed across prompt-processing cycles within a cell until the kill.
What that climb *is* was never established, because nothing here
instrumented the allocator. Two things were observed: lowering the
context moved where the climb started without stopping it, and
restarting per task held memory flat across a whole cell. The
methodology follows the observed effect, not a diagnosis. The full arc
is in `RUNBOOK.md`.

**Context 12288** rather than the 16384 originally planned, because
16384 is where the kills happened. 12288 clears the largest harness
prompt (~7.4k tokens) with room to work, so the tables measure
capability rather than context exhaustion.

**Two memory quantities, never mixed.** *Server* memory is
llama-server: model weights and KV cache, a property of the model and
context, not of the harness. *Harness* memory is the agent process.
Every figure below says which. Kernel `kB` is KiB everywhere, in
`/proc/meminfo` and dmesg alike; all conversions here use 1024.

**Wall clock is reported, not explained.** Per-task durations differ
substantially between harnesses. The cause was not instrumented per
request, and this page does not attribute one.

## Differential: Qwen3-4B-Instruct-2507

| Harness | run 1 | run 2 | spread | failed tasks | task wall clock |
| --- | --- | --- | --- | --- | --- |
| temur 0.25.0 | 9/9 | 9/9 | 0 | none | 13 / 12 min |
| codex-cli 0.149.0 | 8/9 | 8/9 | 0 | t7 both runs | 35 / 35 min |
| opencode 1.18.21 | 7/9 | 6/9 | 1 | t6,t9 / t3,t5,t9 | 31 / 30 min |

Max server anon: temur 3.50, codex 3.51, opencode 3.79 GiB.

Codex's t7 failure reproduced in both runs and in two earlier
observations: it writes the `rm` inside a fenced bash block instead of
calling its exec tool, then reports success while the file survives.

### Control: recovery disabled

The recovery-disabled control described under Qwen2.5-Coder-3B below
scores 9/9 and 9/9 here, with zero nudges and zero recoveries, which is
the expected result and the reason for spending a cell on it: where the
model calls tools natively, prose-call recovery never engages, and
removing it changes nothing.

### A methodology observation, not a harness result

An earlier procedure used one long-lived server per cell. Moving to
per-task servers changed exactly one harness:

| | per-cell server | per-task server |
| --- | --- | --- |
| temur | 9/9, 9/9 | 9/9, 9/9 |
| codex | 8/9, 8/9 | 8/9, 8/9 |
| opencode | 4/9 (single run) | 7/9, 6/9 |

temur and Codex score identically under both, which is also the best
available evidence that the two procedures are otherwise comparable.
The change was neutral for the home team and favourable to the
competitor that had been doing worst. Stated as an observed delta under
a methodology change; the cause is not attributed. Two limits: the
per-cell OpenCode figure is a single run against two, and it comes from
the block whose cells were ending near the OOM ceiling.

## Differential: Qwen2.5-Coder-3B-Instruct

| Harness | run 1 | run 2 | spread | task wall clock |
| --- | --- | --- | --- | --- |
| temur 0.25.0 | 8/9 | 9/9 | 1 | 9 / 16 min |
| codex-cli 0.149.0 | 0/9 | 0/9 | 0 | 19 / 20 min |
| opencode 1.18.21 | 0/9 | 0/9 | 0 | 19 / 18 min |

Max server anon 1.80-1.83 GiB for every cell. temur's single miss was
task 9, the context-pressure task.

This is the clearest result in the milestone, and it is a result about
harnesses rather than about the model.

**Qwen2.5-Coder-3B emits no native tool calls here.** Not few: none, in
any transcript, under either harness. It writes the call as prose
instead, in at least three improvised shapes across tasks (a bare JSON
object, a fenced `json` block, and an XML-ish
`<function-name>...<arguments>` form). Verified at the wire, with no
harness involved: a single `/v1/chat/completions` request at
temperature 0 carrying one tool returns `finish_reason: stop` and a
fenced JSON blob, where Qwen3-4B on the identical request returns a
native `tool_calls` with empty content.

**The template is not the cause, and that is the opposite of what a
previous milestone found.** T34 traced this same prose symptom in
Phi-4-mini to a bundled template that never read top-level `tools`, so
the tools never reached the model. Here the template does read `tools`,
renders them into `<tools>` tags, and explicitly instructs the model to
reply inside `<tool_call>` tags. The instruction is delivered correctly
and the model does not comply. In one task it emitted the template's
own placeholder syntax literally rather than substituting it. Probe and
both templates: `t37-harness-compare-v2-pertask/probes/`.

**So temur's 17/18 on this model rests wholly on prose-call recovery**,
a feature that executes a tool call the model wrote as text: 20
recoveries in run 1, 67 in run 2, zero native calls in either. Same
model, sha, server, context, and prompts; the harness is the entire
difference between 0/9 and 9/9.

### Control: the same temur with recovery disabled

The sentence above used to end "without it temur scores what the others
score", which was an inference. It is now a measurement (run 2026-08-25,
same conditions as the table above).

The control's exact shape matters, so it is stated rather than
summarised. `temur-noprose` is the **same 0.25.0 binary**, invoked by
the same adapter with the same flags in the same working directory,
reading the same config template with **one field added**:
`"prose_tool_calls": false`. That switch turns off *execution* of a tool
call the model wrote as prose. **Detection stays on and the corrective
nudge stays on.** Exactly one thing is removed.

| Harness | run 1 | run 2 | spread | task wall clock |
| --- | --- | --- | --- | --- |
| temur 0.25.0, recovery on | 8/9 | 9/9 | 1 | 9 / 16 min |
| temur 0.25.0, recovery off | 0/9 | 0/9 | 0 | 7 / 7 min |
| codex-cli 0.149.0 | 0/9 | 0/9 | 0 | 19 / 20 min |
| opencode 1.18.21 | 0/9 | 0/9 | 0 | 19 / 18 min |

The inference was right, and the control turned out to be a clean one:
all eighteen control tasks failed, and the recovery-notice count was
zero in every one of them, asserted over the transcripts rather than
assumed from the config.

Two further things the control settles, both of them about temur rather
than about the competitors.

**The nudge converts nothing on this model.** Every one of the eighteen
tasks emitted exactly two "you wrote a tool call as plain text" notices
and then ended the turn, `NUDGE_LIMIT` being 2. Across 36 nudges the
model never once answered with a native tool call: native structured
dispatches in the control cells number **zero**. A nonzero control score
would have been nudge-attributable rather than noise, because with
execution off the nudge is the only remaining path to a pass; there is
simply nothing to attribute, because the score is zero.

**The 0/9 is not a crash, a timeout or a dead server.** The control
cells are the fastest cells in the whole table, 7 minutes against
temur's own 9 and 16, precisely because a turn that nudges twice and
stops does less work than a turn that executes. Nothing hit the 1200s
per-task bound and no cell went VOID.

What the control does not establish: anything about a model that emits
prose calls *and* responds to correction. Qwen2.5-Coder-3B does neither
of the two things that could rescue the control cell, and one model is
one model.

## Prompt size

Tokens in each harness's first tool-carrying request, counted
server-side by llama.cpp, not estimated and not self-reported.

| Harness | first tool-carrying request |
| --- | --- |
| temur | 2761 |
| codex-cli | 7413 |
| opencode | 7276 |

Method matters here. Measured against a **fresh server per harness**:
llama.cpp's prompt-eval count excludes tokens served from the prefix
cache, so a warm server understates the prompt (a first pass read Codex
at 3305, a cache-reduced figure and not a prompt size). And the
**first** request is not always the largest: OpenCode's first request
is a 553-token session-title call carrying no tools, and its agent
request is the one after. Cross-check: Codex self-reported 7441 input
tokens on its own first turn against a different model's tokeniser,
0.4% from the 7413 measured here.

temur spends about 2.7x less before the model has done any work, and
under per-task restarts every task pays it again. That is a measured
input, not an explanation of the wall clock.

## Footprint

Harness-process figures. None of this is llama-server.

| Harness | shipped bytes | linkage | shared libs | peak RSS (1 task) | cold start warm / first |
| --- | --- | --- | --- | --- | --- |
| temur 0.25.0 x86_64 | 7497072 | static-pie | 0 | 37.1 MiB | 0.03s / 0.04s |
| temur 0.25.0 i686-musl | 6166932 | static | 0 | not run | not run |
| codex-cli 0.149.0 | 258322048 | static-pie | 0 | 119.2 MiB | 0.23s / 1.34s |
| opencode 1.18.21 | 184498304 | dynamic | 4 | 842.9 MiB | 2.48s / 5.11s |

Peak RSS is `/usr/bin/time -v` maximum over the process tree during one
real task against a warm server. Cold start is exec to the harness's
first request arriving at the server, marked server-side; the server
stays warm, so **model load is not in it**. Three reps each; the
"first" column is rep 1, which pays to fault the binary into page
cache. The i686-musl row is the shipped 32-bit artifact, whose size is
quoted for that reason; no runtime figure was taken on it, so those
cells say "not run" rather than borrowing the x86_64 numbers.

Two facts that cut against the easy framing, kept because they are
true. **Codex CLI is also a zero-shared-library static binary**, so a
single static binary is not a temur differentiator; size, RSS and
startup are. And **OpenCode's own musl asset is not static**: it links
against `/lib/ld-musl-x86_64.so.1` and does not start on the bare
busybox container temur's release gate already passes, which is why the
glibc build was used throughout and is what the dynamic row describes.

## Two harness properties worth knowing

**Codex requires `/v1/responses`.** This is not a preference:
codex-cli 0.149.0 rejects `wire_api = "chat"` outright as no longer
supported. Its row here depends on the served build implementing the
Responses API at all, and a server that speaks only Chat Completions
cannot run it. That is a portability fact about the harness, not a
score.

**Off-box connections during a keyless, local-only task.** Polled with
`ss -tnp` for the duration of one task, no API key configured, peers
matched against current A records.

| Harness | off-box peers |
| --- | --- |
| temur | none observed |
| codex-cli | address matching chatgpt.com's A records |
| opencode | addresses matching api.opencode.ai's and registry.npmjs.org's A records |

The limit: these are Cloudflare addresses and Cloudflare fronts many
domains from shared IPs, so an address match is strong evidence and not
proof of a hostname; SNI was not captured. temur was probed
identically, and the claim is publishable because of that rather than
in spite of it.

## Terminal-Bench 2 (neutral suite)

Everything above this section uses tasks written for temur's own eval.
This section does not. It is the first result here on an
externally authored suite.

**Headline: pass rate does not separate the three harnesses at this
model. Timeouts and wall clock do.**

### Conditions

Harbor 0.22.0 driving `terminal-bench/terminal-bench-2` (89 tasks),
run 2026-08-25 and 2026-08-26 on the same box as every other table
here. Model Qwen3-4B-Instruct-2507 Q4_K_M on
`ghcr.io/ggml-org/llama.cpp:server-b10438`, ctx 12288, `--parallel 1`,
a fresh server per cell, one trial at a time. Harness versions pinned:
temur 0.27.0 (x86_64 static musl, sha256 `d962af97...`, verified
against the published SHA256SUMS and re-verified by the adapter on
every cell), codex 0.149.1, opencode 1.18.23. Harbor installs the
latter two with `@latest` by default, which is drift rather than a
measurement, so both were pinned explicitly.

A 16-task subset was **pre-registered before any score was seen**, by a
mechanical rule: exclude every task requesting 4096 MB or more, then
take all remaining easy tasks followed by medium tasks in ascending
agent timeout, ties by name, until 16. The rule is a resource rule and
applies uniformly, so one easy task (`overfull-hbox`, 4096 MB) is
excluded and the subset holds 3 easy and 13 medium. The subset file is
`subset.txt`, sha256 `57160ac7b535027acc7e7385577405e8e4de8a62b78e3f307c45558cc6fc7362`,
hashed into the run ledger before the first cell.

Each task carries its own agent budget from the suite, median 900s.
That budget is the suite's, and it was not changed: Terminal-Bench
defines a task as its instruction plus its budget, so a harness that
cannot finish inside the budget has not solved it, and raising the
clock would make these cells incomparable to anyone else's.

### Result

Pass rate over all 16 subset tasks, timeouts counted as failures.

| harness | run 1 | run 2 | reproducible pass |
|---|---|---|---|
| temur 0.27.0 | 1/16 | 1/16 | yes |
| codex 0.149.1 | 1/16 | 0/16 | no |
| opencode 1.18.23 | 1/16 | 1/16 | yes |

Spread was at most 1, below the threshold that would have triggered a
third run.

| harness / run | pass | fail | timeout | ctx-exhausted | VOID |
|---|---|---|---|---|---|
| temur r1 | 1 | 11 | 3 | 1 | 0 |
| temur r2 | 1 | 12 | 1 | 2 | 0 |
| codex r1 | 1 | 9 | 4 | 2 | 0 |
| codex r2 | 0 | 10 | 5 | 1 | 0 |
| opencode r1 | 1 | 15 | 0 | 0 | 0 |
| opencode r2 | 1 | 15 | 0 | 0 | 0 |

`ctx-exhausted` is a cell that died when a request exceeded the pinned
12288 window. It is a scored failure like any other, and it is not
specific to one harness: temur hits it 3 times and codex 3 times.

Exactly one task was solved by anyone, `modernize-scientific-stack`,
and all three solve it:

| harness | run 1 | run 2 |
|---|---|---|
| temur | 343s | 379s |
| opencode | 570s | 545s |
| codex | 640s | timeout at 800s |

**Wall clock**, 32 cells each: temur 2.89h, opencode 3.80h, codex
6.14h. Typical non-solving temur cells finish in 128 to 251s where
codex takes 266 to 566s. No cause is attributed here; the requests
were not instrumented server-side, and this page has retracted one
wall-clock explanation already.

**Install time sits outside the measured budget.** Harbor times agent
setup as its own phase, so none of it comes out of the task clock.
Measured: temur 4.2s, which copies one 7.2 MB static binary and
nothing else; opencode 119.8s and codex 182.1s, each installing curl,
bash, Node and npm and then fetching the harness over the network.
That is a real property of what each ships, and it costs wall clock
and a network dependency rather than score.

### The first temur matrix was invalid, and is disclosed

The first 32 temur cells were thrown away. The adapter written for
this suite piped each task instruction into `temur --plain`, which is
the line REPL and reads one line per turn. 12 of the 16 subset
instructions are multi-line, so temur received only the first line as
its task and every later line arrived as a separate user message after
the previous turn had ended. Measured on one cell, a 21-line
instruction became 20 user messages, two of them empty because the
instruction had blank lines and one of them a bare code fence.
codex and opencode each received the whole instruction in one message,
so for 12 of 16 tasks this was not an equal-footing comparison.

Under that defect temur scored 0/16 twice. Repaired, it scores 1/16
twice. A product finding derived from those cells, that temur's
timeouts were turns which asserted completion without acting, was
**withdrawn**: it was an artifact of the adapter, and the signal it
rested on falls from 52% of turns to 14% once the instruction arrives
whole.

Two details of the repair matter for reading the table. The four
single-line tasks were delivered correctly even by the broken adapter,
verified per cell, so their original results were sound. And codex and
opencode were **not** re-run, because their delivery was never broken;
their rows are from the original matrix, with the same pins, model,
server and subset.

### Instrumentation, per harness

Turn and tool-call counts come from each harness's own transcript
format and count different things. They are published per harness and
are **not** comparable across harnesses.

- **temur**: a turn is one model round trip, read from the session
  file. Repaired cells, median 6 turns and 5 tool calls.
- **codex**: a turn is one whole agentic turn which may hold many
  calls, read from `turn.completed`. Median 1 turn and 10 tool calls,
  and a cell that times out mid-turn emits no `turn.completed` at all,
  so its token counts read as zero while real work happened.
- **opencode**: a turn is one step within a session. Median 1 step and
  0 tool calls, which is its signature here: it stops early rather than
  running out of clock, and never once hit a timeout in 32 cells.

One limitation, stated because it bounds what the temur column can
say: temur writes its session file at exit, and the suite enforces its
budget with a hard kill, so a timed-out temur cell leaves no session
behind. 4 of 32 repaired cells have none, and all four are timeouts.
The temur medians above are therefore over non-timeout cells.

### What this section does not establish

It does not rank the harnesses. At 1/16, 1/16 and 0-to-1/16 with a
single unreproduced cell between them, the suite did not discriminate
them at this model. It says nothing about frontier models or hosted
providers. And it covers 16 of 89 tasks, chosen by a rule that skews
easy to medium, so the harder two thirds of the suite are unmeasured.

## GPU desktop (Terminal-Bench 2 subset)

The section above ran on one CPU-only box. This one runs the same
16-task subset on a second machine with a GPU, so the suite has now
been driven on two boxes with different hardware postures.

**Headline: pass rate does not separate the three harnesses at this
model here either. Wall clock does, and no cause is attributed to it.**

### Conditions

Run 2026-08-27 on DESKTOP-6O763EN: GTX 1070 Ti 8 GB, driver 580.97
(CUDA 13.0), WSL2 Ubuntu 26.04. Model
`Qwen3-4B-Instruct-2507-Q4_K_M.gguf`, sha256 `3605803b...`, served by
`ghcr.io/ggml-org/llama.cpp:server-cuda-b10438`, image digest
`sha256:b5e13ddf...`, ctx 12288, `-ngl 99 --parallel 1 --jinja`, a
fresh server per cell. All 96 cells offloaded 37/37 layers to the GPU;
peak VRAM 4362 MiB of 8192; zero server deaths.

The same pre-registered 16-task subset as above, sha256 `57160ac7...`,
and the suite's own per-task agent budgets, median 900s, unchanged.
Harness pins: temur 0.28.0 (x86_64 static musl, sha256 `813d539f...`,
re-verified by the adapter on every cell), codex 0.149.1, opencode
1.18.23.

### Result

Pass rate over all 16 subset tasks, timeouts counted as failures, the
same rule as the CPU section.

| harness | run | pass | fail | ctx-exhausted | exc | void | wall clock |
|---|---|---|---|---|---|---|---|
| temur 0.28.0 | run 1 | 2/16 | 12 | 2 | 0 | 0 | 0.39 h |
| temur 0.28.0 | run 2 | 1/16 | 14 | 1 | 0 | 0 | 0.38 h |
| opencode 1.18.23 | run 1 | 1/16 | 15 | 0 | 0 | 0 | 1.29 h |
| opencode 1.18.23 | run 2 | 2/16 | 12 | 1 | 0 | 1 | 1.51 h |
| codex 0.149.1 | run 1 | 1/16 | 11 | 1 | 2 | 1 | 1.55 h |
| codex 0.149.1 | run 2 | 0/16 | 11 | 2 | 3 | 0 | 1.62 h |

Wall clock per harness over 32 cells each: **temur 0.77 h, opencode
2.80 h, codex 3.16 h.** Spread between each harness's two runs was 1
pass, below the threshold that would have triggered a third run.

`exc` is a harness-level exception scored as a failure; here all five
are `codex exec` itself exiting non-zero. `void` is a cell with no
verdict, excluded from the scored denominator rather than counted as a
failure. Both VOIDs are agent-setup timeouts, and both landed on the
same task, `git-leak-recovery`, which was already the slowest setup on
this suite.

Two tasks were solved by anyone:

| task | temur | opencode | codex |
|---|---|---|---|
| `modernize-scientific-stack` | run 1, run 2 | run 1, run 2 | none |
| `prove-plus-comm` | run 1 | run 2 | run 1 |

Server throughput on this GPU, warm, from the server's own timings at
a 2087-token prompt: prompt processing 935.2 tok/s mean, generation
50.2 tok/s mean.

For reference, the CPU box's repaired temur row from the section above
is 1/16, 1/16, 2.89 h. **Two variables differ at once**, the GPU and
the machine, so nothing about either box is inferred from the pair;
the build, model, binary source, subset, ctx and budget are identical
between them.

### Same rig, larger model (Qwen3-8B, thinking off)

A third matrix ran on the same box, 2026-08-27 to 2026-08-29, changing
one thing: the model. Build, image digest, ctx, per-task budgets, the
16-task subset and all three harness pins are the ones listed above.
The model is `Qwen3-8B-Q4_K_M.gguf` from `unsloth/Qwen3-8B-GGUF`,
sha256 `120307ba...`, which is also its Hugging Face LFS oid.

Qwen3-8B is a hybrid thinking model, so thinking was held **off** with
`--chat-template-kwargs '{"enable_thinking": false}'`, and that is
established three ways rather than asserted once: a control server on
the same build, model and prompt with the flag removed **did** reason;
every cell issued an 8-token probe before its agent started that had
to return no `<think>` tag and no `reasoning_content` field, 96/96;
and a sweep of every transcript after the matrix returned 0 hits.

All 96 cells again offloaded 37/37 layers to the GPU, 84 of them at a
peak of 6464 MiB of VRAM and the highest at 7049 MiB. The temur binary
is 0.28.0, the same one the 4B run used: **auto-compaction, which
shipped in v0.29.x, is not in these numbers.**

| harness | run | pass | fail | ctx-exhausted | exc | void | wall clock |
|---|---|---|---|---|---|---|---|
| temur 0.28.0 | run 1 | 1/16 | 12 | 3 | 0 | 0 | 0.66 h |
| temur 0.28.0 | run 2 | 0/16 | 12 | 4 | 0 | 0 | 0.53 h |
| opencode 1.18.23 | run 1 | 1/16 | 8 | 7 | 0 | 0 | 1.70 h |
| opencode 1.18.23 | run 2 | 1/16 | 8 | 7 | 0 | 0 | 1.73 h |
| codex 0.149.1 | run 1 | 1/16 | 11 | 1 | 2 | 1 | 1.32 h |
| codex 0.149.1 | run 2 | 0/16 | 15 | 0 | 1 | 0 | 1.19 h |

Wall clock per harness over 32 cells each: **temur 1.18 h, opencode
3.43 h, codex 2.51 h.** The spread between each harness's own two runs
was 1, 0 and 1 pass, all below the threshold that triggers a third
run, and no third run was made.

All three `exc` are `codex exec` itself exiting non-zero, scored as
failures. The one void is an agent-setup timeout on
`git-leak-recovery`; it took the single retry the ruling allows, hit
the same 360-second setup budget again, and so stays excluded from the
scored denominator rather than counted as a failure. That is the same
task both of the 4B run's voids landed on.

Every pass in this table is `modernize-scientific-stack`: temur run 1,
opencode run 1 and run 2, codex run 1. `prove-plus-comm`, which
produced 3 of the 4B run's 7 passes, produced none here.

Beside the 4B section: passes 7/96 to 4/96, and ctx-exhausted 7 to 22
(temur 7, opencode 14, codex 1, spread across 9 of the 16 tasks).
**Same rig, model changed** is the whole of what may be said about the
pair. The per-cell counts sit inside the run-to-run noise the 4B pair
already showed, where a harness's own two runs differed by as much as
a pass; the exception is ctx-exhausted, which is recorded here as an
observation with no cause attributed to it. Throughput on the same
build, GPU and context, from the server's own timings: prompt
processing 593.9 tok/s, generation 32.2 tok/s, which is 0.63x and
0.64x of the 4B.

Required disclosure: the host slept for 18 h 54 m mid-matrix on a
Windows power plan since corrected, with 48 cells run before it and 48
after, the interrupted cell deleted and re-run, no cell spanning the
gap, an `INTERRUPTED` line at that point in the append-only ledger,
and 8 h 07 m of running elapsed. Full report, cell tree and ledger for
this run: `~/temur-eval-archive/desktop-exp3/`.

### Same rig, MoE model (Qwen3-Coder-30B-A3B)

A fourth matrix ran on the same box, 2026-08-29 to 2026-08-30,
changing the model again. Build, image digest, ctx 12288, per-task
budgets, the 16-task subset and all three harness pins are the ones
listed above, and the temur binary is still 0.28.0, so
**auto-compaction, which shipped in v0.29.x, is again not in these
numbers.** The model is `Qwen3-Coder-30B-A3B-Instruct-Q4_K_M.gguf`
from `unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF`, sha256
`fadc3e5f...`, which is also its Hugging Face LFS oid, 18,556,689,568
bytes. It is a non-thinking model, so no thinking flag was set; the
per-cell probe experiment 3 introduced was run anyway and passed
trivially on all 96 cells.

The offload here is **partial, and the server's own log line says
otherwise.** Every cell prints `offloaded 49/49 layers to GPU`. That
line counts layers, not tensors, and it is misleading at this
setting. Under `-ngl 99 --n-cpu-moe 34`, all 48 layers' attention and
dense tensors and the full 12288-token KV cache are on the GPU (5766
MiB of weights, 1152 MiB of KV, 222 MiB of compute), the experts of
the first 34 layers are on the CPU (12,308 MiB resident there), and
the experts of the last 14 are on the GPU. That configuration was
chosen over the best dense split by measurement rather than by
preference: `-ngl 18` reached 9.27 tok/s of generation against
18.76, so the MoE split is 2.0x faster. Throughput of the chosen
configuration, warm, from the server's own timings at the same
2087-token prompt: prompt processing 190.7 tok/s, generation 18.65
tok/s.

| harness | run | pass | fail | timeout | ctx-exhausted | exc | void | wall clock |
|---|---|---|---|---|---|---|---|---|
| temur 0.28.0 | run 1 | 3/16 | 5 | 0 | 8 | 0 | 0 | 1.14 h |
| temur 0.28.0 | run 2 | 4/16 | 4 | 0 | 8 | 0 | 0 | 1.16 h |
| opencode 1.18.23 | run 1 | 4/16 | 5 | 4 | 3 | 0 | 0 | 3.37 h |
| opencode 1.18.23 | run 2 | 3/16 | 4 | 2 | 7 | 0 | 0 | 3.19 h |
| codex 0.149.1 | run 1 | 3/16 | 2 | 0 | 8 | 3 | 0 | 2.00 h |
| codex 0.149.1 | run 2 | 4/16 | 3 | 0 | 6 | 3 | 0 | 1.97 h |

Wall clock per harness over 32 cells each: **temur 2.31 h, opencode
6.56 h, codex 3.97 h.** Every harness's two runs differed by exactly
1 pass, below the threshold that triggers a third run, and no third
run was made.

Four cells VOIDed on an agent-setup timeout and each took the single
retry the experiment-2 rule allows. All four retries produced a
verdict: `prove-plus-comm` passed, and the other three scored as
failures, one of them ctx-exhausted. **There are no VOIDs in the
scored table**, and the first attempts are preserved in the archive
and excluded from every count above.

Read in this order:

- **Every harness scored exactly 7 of 32.** Two runs of sixteen tasks
  cannot rank three harnesses, and this table does not.
- **The model ladder moved: 7/96 at 4B, 4/96 at 8B, 21/96 here**,
  with passes landing on five of the sixteen tasks instead of one.
  The flatness of the two earlier tables was the models, not the
  suite.
- **Cost still separates what the score does not.** opencode spent
  2.8x temur's wall clock to reach the same total.
- **At 18.65 tok/s the 900-second agent budget has started to bind.**
  Six cells ended in `TIMEOUT`, the first in this series, and all six
  are opencode, the harness that spends the most turns per cell. Part
  of what that row now measures is the clock rather than the agent,
  which weakens it in a way the temur and codex rows do not share.
- **temur's ctx-exhausted failures are 16 of 32**, the largest count
  yet, and they were measured on v0.28.0, **without auto-compaction**.
  The controlled run that would say whether the feature converts them
  is queued as desktop experiment 5.

Required disclosures. The matrix halted once, 42 cells in, on three
consecutive opencode VOIDs, all of them
`AgentSetupTimeoutError: Agent setup timed out after 360.0 seconds`.
The halt condition was obeyed rather than edited, and a ruling then
resumed the run with one change **for this experiment only**: a
setup-timeout VOID is exempt from that halt condition, because the
experiment-2 ruling already treats a setup timeout as a property of
this box's link rather than of the run. The 360-second budget was not
raised and no pin was changed. The gap is 9 h 33 m, against 14 h 43 m
of running elapsed, and only the running figure is used anywhere; the
driver, image digest, model hash, temur hash, context, budgets and
pins were all read back unchanged before the restart. Separately, a
census note for anyone recomputing the table: a raw count of the
`CTX` tag finds 45 cells, and counting only ctx-exhausted **failures**
finds 40, because six cells carry the tag and nevertheless passed.
Passes are classified first, so 40 is the number the table uses. Full
report, cell tree and stage records for this run:
`~/temur-eval-archive/desktop-exp4/`.

### The earlier GPU run is archive-only

An experiment 1 ran on this box on 2026-08-26/27 against llama.cpp b8580,
which was forced by the then-installed 560.94 driver. Its codex column
was not a measurement of codex: all 32 codex cells failed identically
at the first request because that build rejected codex's Responses-API
tool type with `HTTP 400 'type' of tool must be 'function'`. On
b10438 that failure does not occur in any of the 96 cells, and codex
completes turns, runs shell commands and solves a task. Experiment 1
is therefore kept as an archive record and none of its numbers are
published here.

### What this section does not establish

It does not rank the harnesses, for the same reason as the CPU
section: at 2/16 down to 0/16 with one pass of spread, the suite did
not discriminate them at this model. It says nothing about frontier
models or hosted providers, and it covers 16 of 89 tasks. Per-harness
turn and tool-call instrumentation was collected but is **not
published here**, because the three harnesses count different things
and one of the parsers has a known hole; the figures and that caveat
live in the archive.

Full report, cell tree and ledger:
`~/temur-eval-archive/desktop-exp2/`.

## Not comparable to OFFLINE.md

`docs/OFFLINE.md` carries temur's own small-model results. Those
numbers and these are **not comparable**: they were taken at a
different context size and under a different server methodology (one
long-lived server rather than a fresh one per task). Compare within a
table here, not across the two documents.

## Reproducing

    scripts/harness_compare/matrix.sh <model-label> 2

The recovery-disabled control is a fourth harness name rather than an
environment knob, so its cells, ledger lines and scores stay separate
from temur's by construction:

    HARNESSES=temur-noprose scripts/harness_compare/matrix.sh <model-label> 2

Artifacts, including per-cell ledgers, transcripts, gate logs and every
probe quoted above, are archived outside the repository under
`~/temur-eval-archive/t37-harness-compare-v2-pertask/`.
