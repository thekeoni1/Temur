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
recoveries in run 1, 67 in run 2, zero native calls in either. Without
it temur scores what the others score. Same model, sha, server,
context, and prompts; the harness is the entire difference between 0/9
and 9/9.

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

## Not comparable to OFFLINE.md

`docs/OFFLINE.md` carries temur's own small-model results. Those
numbers and these are **not comparable**: they were taken at a
different context size and under a different server methodology (one
long-lived server rather than a fresh one per task). Compare within a
table here, not across the two documents.

## Reproducing

    scripts/harness_compare/matrix.sh <model-label> 2

Artifacts, including per-cell ledgers, transcripts, gate logs and every
probe quoted above, are archived outside the repository under
`~/temur-eval-archive/t37-harness-compare-v2-pertask/`.
