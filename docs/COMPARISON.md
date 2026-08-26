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
