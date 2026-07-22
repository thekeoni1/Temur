# temur — Roadmap

> Adopted 2026-07-18. Supersedes the post-v1 milestone set (A–E). Builds on v1 +
> v1.x as shipped: agent loop, seven tools + skill tool, Anthropic provider
> (fixture-tested, live-verified), line REPL + ratatui TUI, static musl build
> proven. ~2k LOC core, pure-Rust dependency tree.
>
> Decisions folded in: bespoke vendor providers (old milestone C, Gemini) are
> retired in favor of one OpenAI-compatible provider; the niche is stated as
> constrained/embedded Linux including ARM, with i686 as the discipline rather
> than the market; the neutral-types refactor (T1) lands standalone before the
> second provider (T2).

## 1. Positioning

The pitch: a single static binary with no runtime, running where OpenCode
cannot — 32-bit Linux, embedded and constrained systems, bare machines, and
(with local models) fully offline. Claim by claim:

**"Runs where OpenCode cannot" — true and defensible.** OpenCode is a
Bun/Node/TypeScript system: no 32-bit x86 builds, no armv7 builds, and its
"single executable" bundles embed a ~90 MB runtime. A ~5 MB musl-static ELF
with zero `NEEDED` entries runs on machines OpenCode will never boot: old x86,
armv7 industrial controllers, OpenWrt-class devices, `FROM scratch` containers,
initramfs environments, air-gapped hosts where installing a runtime is a
change-control event.

**"32-bit x86 is the niche" — weak as stated.** i686 desktop Linux is a
retro-computing audience. i686 is the *discipline* — it forced 32-bit-safe
sizes, static linking, and a tiny dependency tree — not the *market*. The
market that discipline implies is constrained and embedded Linux generally,
which is overwhelmingly ARM, and the same build recipe reaches
`armv7-musleabihf` and `aarch64-musl` nearly free (rustls/ring support both).
So the claim is "any Linux, down to 32-bit and embedded"; i686 is the proof,
not the point.

**"Fully offline with local models" — the strongest leg, currently unbuilt.**
Air-gapped, regulated, and privacy-constrained environments cannot use cloud
agents, and no mainstream harness treats offline as a first-class mode. The
honest topology: nobody runs a useful LLM *on* a 32-bit box. The realistic
deployment is temur on the constrained device where the code lives, pointed at
a llama.cpp/Ollama server elsewhere on the LAN — or both on one modern machine
with no internet. The pitch must say that.

**"Native performance, memory-safe" — true, not differentiating.** The agent
idles waiting on the model. Low RSS matters on a 128 MB device; harness speed
mostly doesn't. Say "small footprint," never "fast" — and never claim fast
compilation anywhere: Rust compiles slowly. The advantage is entirely the
shipped artifact.

**The competitor that isn't OpenCode.** Go-based agents (e.g. Crush) also
cross-compile near-static single binaries, including 386 and ARM. temur's
edges over Go are degree, not kind: smaller binaries, lower RSS, no GC, true
zero-`NEEDED` musl static. The moat is therefore not the binary alone but
**binary + offline + runs-weak-models-well as one story**. Any leg alone is a
rationalization; the three together are a defensible niche.

**Who chooses temur:** embedded/industrial developers working on-device;
operators of air-gapped or regulated environments; router/NAS/homelab users;
anyone dropping one file into a minimal container or rescue image; anyone who
wants an auditable, small-dependency-tree agent they can vendor and build
themselves.

### One-line positioning

> **temur is a dependency-free single static binary coding agent for any Linux
> system — down to 32-bit and embedded — that runs fully offline against local
> models.**

Decision rule: a feature is IN if it serves *constrained, offline, or
weak-model* use. OpenCode parity for its own sake is LOW priority and must
argue its way up.

## 2. Capabilities, ranked by the niche

### P0 — OpenAI-compatible provider (highest-leverage addition)
One implementation unlocks OpenAI, Groq, OpenRouter, Together, DeepSeek,
Gemini's compat endpoint, **and llama.cpp/Ollama/vLLM/LM Studio** — the last
four being the offline niche directly. It is also the first real test of the
provider abstraction. Bespoke vendor providers are retired: Gemini is
reachable through this endpoint.

**Is the current trait provider-neutral? No — it is Anthropic with a trait in
front.** `provider::mod` re-exports `anthropic::types::{ContentBlock,
ResponseMessage, Role, StopReason, Usage}` as the "neutral" vocabulary, and
they serialize 1:1 into Anthropic wire JSON. Known leak points a second
provider will hit:

1. **Tool-call/result shape.** Anthropic: `tool_use` block (`input` as JSON
   `Value`) answered by a `tool_result` block with `tool_use_id` inside a user
   message. OpenAI: `tool_calls` on the assistant message with `arguments` as
   a **string** (streamed as text fragments), answered by separate
   `role:"tool"` messages. The neutral types survive conceptually; the derived
   serialization does not — conversion moves to the provider boundary. Some
   local servers omit tool-call IDs; the provider must synthesize them.
2. **Stop reasons.** `PauseTurn`, `Refusal`, `ModelContextWindowExceeded` are
   Anthropic-specific. OpenAI's `finish_reason` set (`stop`, `length`,
   `tool_calls`, `content_filter`) maps into the neutral superset enum
   (`content_filter`→`Refusal`, `length`→`MaxTokens`, …). Document which
   variants each provider can emit.
3. **Usage accounting.** Fields are Anthropic's (`cache_creation_input_tokens`
   etc.). OpenAI reports `prompt/completion_tokens`, cached tokens nested in
   `prompt_tokens_details`, **only in the final chunk, only if
   `stream_options.include_usage` is set** — and many local servers omit it
   entirely. Usage becomes best-effort, possibly absent.
4. **SSE framing.** Anthropic: named typed events. OpenAI: uniform `data:`
   chunks plus a `data: [DONE]` terminator. Line-level SSE framing is
   shareable; event interpretation is per-provider.
5. **Thinking blocks.** `signature` / `RedactedThinking` are Anthropic
   round-trip state — kept as opaque provider passthrough; others ignore them.
6. **Auth + secret.** `x-api-key` vs `Authorization: Bearer` is trivial. Not
   trivial: **local providers need no key**, so the secret-file requirement
   becomes per-provider-optional. Key isolation rules are unchanged for any
   keyed provider.
7. **Request knobs.** `max_tokens` naming drifts (`max_completion_tokens`);
   local models want `temperature`/`top_p` and have small context windows.
   `ChatRequest` needs a few neutral optionals, mapped per provider.

### P0 — Weak-model robustness (niche-critical, not optional)
"Runs small local models well" is a pillar of the positioning, and small
models are markedly worse at tool-calling. Needed in the agent core,
provider-neutral: tolerant tool-argument handling (malformed/truncated JSON
args → schema-error `tool_result` and a retry chance, plus argument repair for
trivial cases and a bounded consecutive-failure cap); detection of tool calls
emitted as plain text (a known small-model failure); **per-model prompt
profiles** — a compact system prompt and trimmed tool descriptions for
small-context models (the OpenCode-ported prompts are Claude-sized); doom-loop
guard extensions (alternating-pair loops, empty responses). Plus a scripted
offline eval harness so "works with weak models" is measured, not claimed.

### P1 — Local/offline polish
Beyond the compat provider: keyless operation, graceful handling of absent
usage/IDs, context-window awareness (small local contexts → earlier, clearer
overflow behavior), a llama.cpp/Ollama quickstart, and an end-to-end offline
demo as the acceptance artifact.

### P1 — Session persistence
Serves the niche twice: constrained devices lose sessions (SSH drops, power
cuts), and offline work is long-lived. Plain JSON transcript save/resume;
neutral-typed history makes the format provider-independent.

### P2 — Richer editing (fuzzy-match fallback)
The deferred OpenCode-style matchers. Weak-model-relevant — small models
reproduce `old_string` imperfectly, so exact-match `edit` fails more for them —
which promotes it above pure parity, but behind the loop hardening that
decides whether weak models can drive the tools at all.

### P2 — Turn interruption
Known TUI gap (a hung turn can only be force-quit). Provider-agnostic, small
seam extension already scoped in `docs/TUI.md`.

### P3 — Multi-arch release packaging
armv7/aarch64/x86_64 musl-static builds + install docs. Cheap, and it converts
the positioning from claim to download link. x86_64-musl also lets
contributors try temur without 32-bit ceremony.

### LOW — parity for its own sake (explicitly deprioritized)
LSP integration, MCP support, IDE plugins, web UI, server/multi-client mode,
plugin ecosystem, bespoke per-vendor providers, sub-agents, themes. Each adds
dependency and maintenance surface (several threaten the static/musl
constraint) and none serves constrained/offline/weak-model use. MCP is the
only one likely to earn reconsideration later; it still loses to everything
above today.

## 3. Milestone ladder

Each milestone independently shippable, gated by `check.sh` (as upgraded in
T0). Order: identity and gate integrity → abstraction validation → the niche
payload.

| # | Milestone | Ships |
|---|-----------|-------|
| T0 | Identity + honest gate | Rename to `temur`; check.sh tests the shipped musl-static path with readelf assertions |
| T1 | Provider-neutral core | Neutral types owned by `provider::`, Anthropic converts at its own boundary; zero behavior change |
| T2 | OpenAI-compatible provider | Second provider behind the trait, fixture-tested; config-selected |
| T3 | Offline/local polish | Keyless mode, quirk tolerance, context awareness, llama.cpp/Ollama docs + offline demo |
| T4 | Weak-model hardening | Tolerant parsing, retry/correction, prompt profiles, offline eval harness |
| T5 | Session persistence | JSON save/resume in the neutral vocabulary |
| T6 | Editing + interruption | Fuzzy-match edit fallback; cancellable turns |
| T7 | Multi-arch packaging | armv7/aarch64/x86_64 musl-static releases, install story |

### T0 — Identity + honest gate
- Rename `opencode-rust` → `temur`: package name, `--version`, binary name,
  doc headers, RUNBOOK. Keep an MIT attribution note for the OpenCode-ported
  tool prompts.
- Skills dir: introduce `.temur/skills`; keep reading `.opencode/skills` as a
  fallback for one release.
- **Close the gate gap — the shipped artifact is currently the least-tested
  one.** The repo's build config and check.sh cover only the gnu *debug*
  target; the musl-static ship path is an undocumented build variant. check.sh
  additionally: builds `--release` for `i686-unknown-linux-musl`, asserts
  staticness via `readelf -l` (**no `INTERP`**) and `readelf -d` (**no
  `NEEDED`**), runs the test suites and mock-REPL/TUI smokes in the container
  **against the musl binary**, and runs `--version` + mock smoke in a bare
  (busybox/near-scratch) container where a dynamic binary could not even load.
- gnu-debug stays as the fast inner loop; musl-release is the acceptance path.

### T1 — Provider-neutral core
Move `ContentBlock`, `StopReason`, `Usage`, `Role`, `ResponseMessage` into
`provider::types` as plain data (serde retained only for temur's own use, e.g.
T5 persistence — never as a wire format). `anthropic::types` keeps its wire
shapes and gains explicit to/from-wire conversion; `cache_control` injection
stays inside the Anthropic provider. `Usage` becomes best-effort; `ChatRequest`
gains the neutral optionals from §2. Exit criterion: the entire existing
fixture + live-conformance suite green with **zero fixture changes** — proof
the refactor is pure re-plumbing.

### T2 — OpenAI-compatible provider
- `provider/openai_compat/`: wire types, chunk-stream SSE decoder (shared
  line-framing extracted from the Anthropic parser), transport via the
  existing `Transport` seam, retry policy reused.
- **Fixtures, same discipline as M1 — no live calls from the build session:**
  (a) hand-authored chunk streams from the OpenAI API reference — critically
  tool-call `arguments` fragmented across chunks, parallel tool calls,
  `[DONE]`, final-chunk usage; (b) cross-checked against openai-python/-node
  SDK test fixtures (read-only reference; divergences resolve toward the
  SDKs); (c) **quirk fixtures modeling local servers** (llama.cpp/Ollama):
  absent usage, absent tool-call IDs, whole-call-in-one-chunk, role deltas.
  A later operator-run capture against a real local server can be frozen in as
  layer 3, mirroring `tests/fixtures/live/`.
- Config: `provider: "anthropic" | "openai-compat"` plus per-provider
  `base_url`/`model`/auth. API keys only by file path (`APP_SECRET_FILE` or a
  per-provider path — never env, never argv), explicitly optional for keyless
  endpoints. Defaults stay `anthropic` / `claude-sonnet-5`; no default flips
  without a decision.
- Write down the expected trait changes (§2's leak list) as predictions and
  diff them against what actually changed in review. This milestone is graded
  as much on what it reveals about the abstraction as on the feature. If T1
  was done well, `provider::mod` should barely change; every place it does is
  a lesson.

### T3 — Offline/local polish
Keyless startup path; tolerant degradation when usage/IDs are absent; clear
behavior at small context windows; `docs/OFFLINE.md` (llama.cpp + Ollama
quickstart, LAN topology, recommended small models). Acceptance: a scripted,
operator-run end-to-end demo — temur (musl-static, in the container) driving a
local llama.cpp server with zero internet. Running an inference server from a
build session is new system surface — plan and ask first.

### T4 — Weak-model hardening
The §2 P0 list: argument repair, text-emitted-tool-call detection, bounded
correction retries, compact prompt profile selected per model/config, guard
extensions — plus `tests/weak_model.rs`: scripted fixture streams that *are*
the misbehaviors (malformed args, hallucinated tool names, loops), asserting
the loop degrades politely.

### T5 — Session persistence
`--continue` / session files under a config dir; JSON in the neutral
vocabulary; atomic writes (power-cut-friendly — it's the niche); size-capped
with `u64` discipline. Anthropic thinking signatures round-trip opaquely.

> As-built note: session files live under the STATE dir
> (`$XDG_STATE_HOME/temur/sessions`, fallback `~/.local/state`), not the
> config dir as written above — megabyte transcripts of tool output don't
> belong in a dotfile-synced `~/.config`.

### T6 — Editing + interruption
Port OpenCode's fuzzy matchers (whitespace-tolerant, block-anchor) behind the
existing exact-match-first behavior, with offline table-driven tests; add the
cancel flag/seam from `docs/TUI.md` so a turn can be interrupted without
killing the process.

> As-built notes (2026-07-21): both halves landed offline-gated.
> **Interruption:** a session-owned `CancelToken` (`Arc<AtomicBool>`),
> polled once per received SSE frame in both provider drive loops, in
> ≤200 ms slices of retry backoff and bash waits, and per tool call in a
> batch; TUI Esc sets it. Landing keeps completed content, drops mid-JSON
> `tool_use` (`input_raw`) and unsigned thinking, and answers kept
> `tool_use` blocks with synthesized `[interrupted by user]` error results
> in one message — history stays wire-valid and the driver-loop save
> persists it. Exclusions: plain-REPL interruption (blocked main thread,
> SIGINT would need a new dependency) and fully stalled streams — ureq
> timeouts are whole-phase deadlines, not idle timeouts (verified in
> ureq 3.3.0 source), so they cannot implement cancel-polling and
> double-Ctrl+C force-quit remains that escape hatch. Found en route:
> `sh -c` forks its command, so bash kill/timeout now kill the process
> GROUP via the sh *builtin* kill (minimal images ship no kill binary).
> **Fuzzy edit:** OpenCode's line-trimmed and block-anchor matchers
> ported behind exact-match-first; within a matcher ≥2 candidates is an
> error demanding more context (stricter than OpenCode — never a guess);
> `replaceAll` stays exact-only; no Levenshtein scoring at all. Fuzzy
> successes are marked in the tool output; the prompts still demand
> exactness.

### T7 — Multi-arch packaging
Release builds for `i686-musl`, `armv7-musleabihf`, `aarch64-musl`,
`x86_64-musl`; per-target readelf gates; checksums; an install page that leads
with the one-liner. ARM targets get build-level verification even if hardware
smoke-testing waits for hardware.

## 4. Invariants (every milestone)

- Ships musl-static: `readelf` shows **no INTERP, no NEEDED** (gated from T0).
- Pure-Rust TLS: rustls + ring only. Every new dependency is vetted for C code
  or OpenSSL before adoption; the `cargo tree` gates stay; anything pulling
  `*-sys` crates that break musl/32-bit is rejected or feature-gated off.
- 32-bit discipline: `u64` for sizes/offsets; no large-allocation assumptions.
- Two-identity key isolation: secrets by path only — never env, argv, logs, or
  the repo; the builder session never holds a live key; no live API calls from
  build sessions.
- Green `check.sh` (host + container, gnu-debug + musl-release) before a
  milestone closes.

## 5. Release hygiene

- The repo stays clean-by-default for public release: no company names, no
  personal names, no employer-specific workflow references in code, docs,
  fixtures, commit messages, or example configs. Fixture and doc text uses
  neutral examples.
- Operator-machine paths (runtime dir, secret dir, build target dir) are
  machine configuration, not project content: repo references to them go
  through config or variables so the repo stays relocatable.
- History is clean as of the initial temur commit; a leak grep (names, company
  terms, key-shaped strings) runs as a pre-release step from T0 onward.

## 6. Risks

- **Provider-abstraction leakage (T1/T2).** The "neutral" layer may still
  think in Anthropic shapes (string-only tool results, mandatory usage).
  Mitigation: T1 lands first and separately; T2's review diffs predicted vs
  actual trait changes. A third provider may force another pass; two data
  points beat zero.
- **Weaker-model reliability.** The floor may simply be low: some small models
  cannot drive seven tools no matter how tolerant the loop is. Mitigation: the
  eval harness makes the floor measurable; docs recommend known-good models
  rather than promising "any model works." Failure mode to avoid: quietly
  tuning everything for Claude while claiming local-model support.
- **Offline integration friction.** Local servers' OpenAI compatibility is
  approximate and drifts (tool-call template bugs, absent fields). Mitigation:
  quirk fixtures are first-class and grow with each reported incompatibility;
  degrade politely (missing usage ≠ error).
- **Static-linking constraints vs new dependencies.** Every future crate is a
  potential C dependency in disguise (compression, git bindings, SQLite).
  This is the standing tax of the niche. The forbidden-dep and readelf gates
  make violations loud; prefer pure-Rust or minimal hand-rolled
  implementations (the skills frontmatter parser set the precedent).
- **Scope/maintenance — becoming a worse OpenCode.** The pull is toward the
  LOW list, where temur competes on OpenCode's terms with a fraction of the
  hands and loses. The §1 decision rule is the backlog filter; small surface
  is a feature of the product, not a deficit.

## 7. Build order

**First:** T0 (days, and the gate gap genuinely matters), then T1→T2→T3→T4 as
one arc. That arc *is* the differentiation: after it, the one-liner is fully
true and demonstrable — before it, the offline pitch is a slide. T5–T7 follow
by usability pull.

**Not building:** bespoke vendor providers (the compat endpoint covers them),
LSP, MCP (revisit post-T7 at the earliest), IDE/web UI, server mode, plugin
system, sub-agents, any async-runtime migration (blocking ureq is correct for
this niche), and any dependency that compromises musl-static purity for
convenience.

**Where the story is weak — kept visible on purpose:** i686 desktop alone is a
retro hobby, not a market (ARM/embedded is the market; i686 is the proof);
Go-based single-binary agents narrow the binary-profile advantage, so the moat
is the combination with offline + weak-model competence; and local-model
tool-calling quality may set a floor we don't control. The ladder front-loads
exactly the milestones that test those three uncertainties.
