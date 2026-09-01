# temur - Roadmap

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
cannot: 32-bit Linux, embedded and constrained systems, bare machines, and
(with local models) fully offline. Claim by claim:

**"Runs where OpenCode cannot" - true and defensible.** OpenCode is a
Bun/Node/TypeScript system: no 32-bit x86 builds, no armv7 builds, and its
"single executable" bundles embed a ~90 MB runtime. A ~5 MB musl-static ELF
with zero `NEEDED` entries runs on machines OpenCode will never boot: old x86,
armv7 industrial controllers, OpenWrt-class devices, `FROM scratch` containers,
initramfs environments, air-gapped hosts where installing a runtime is a
change-control event.

**"32-bit x86 is the niche" - weak as stated.** i686 desktop Linux is a
retro-computing audience. i686 is the *discipline* (it forced 32-bit-safe
sizes, static linking, and a tiny dependency tree), not the *market*. The
market that discipline implies is constrained and embedded Linux generally,
which is overwhelmingly ARM, and the same build recipe reaches
`armv7-musleabihf` and `aarch64-musl` nearly free (rustls/ring support both).
So the claim is "any Linux, down to 32-bit and embedded"; i686 is the proof,
not the point.

**"Fully offline with local models" - the strongest leg, currently unbuilt.**
Air-gapped, regulated, and privacy-constrained environments cannot use cloud
agents, and no mainstream harness treats offline as a first-class mode. The
honest topology: nobody runs a useful LLM *on* a 32-bit box. The realistic
deployment is temur on the constrained device where the code lives, pointed at
a llama.cpp/Ollama server elsewhere on the LAN, or both on one modern machine
with no internet. The pitch must say that.

**"Native performance, memory-safe" - true, not differentiating.** The agent
idles waiting on the model. Low RSS matters on a 128 MB device; harness speed
mostly doesn't. Say "small footprint," never "fast", and never claim fast
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
> system, down to 32-bit and embedded, that runs fully offline against local
> models.**

Decision rule: a feature is IN if it serves *constrained, offline, or
weak-model* use. OpenCode parity for its own sake is LOW priority and must
argue its way up.

## 2. Capabilities, ranked by the niche

### P0 - OpenAI-compatible provider (highest-leverage addition)
One implementation unlocks OpenAI, Groq, OpenRouter, Together, DeepSeek,
Gemini's compat endpoint, **and llama.cpp/Ollama/vLLM/LM Studio**, the last
four being the offline niche directly. It is also the first real test of the
provider abstraction. Bespoke vendor providers are retired: Gemini is
reachable through this endpoint.

**Is the current trait provider-neutral? No: it is Anthropic with a trait in
front.** `provider::mod` re-exports `anthropic::types::{ContentBlock,
ResponseMessage, Role, StopReason, Usage}` as the "neutral" vocabulary, and
they serialize 1:1 into Anthropic wire JSON. Known leak points a second
provider will hit:

1. **Tool-call/result shape.** Anthropic: `tool_use` block (`input` as JSON
   `Value`) answered by a `tool_result` block with `tool_use_id` inside a user
   message. OpenAI: `tool_calls` on the assistant message with `arguments` as
   a **string** (streamed as text fragments), answered by separate
   `role:"tool"` messages. The neutral types survive conceptually; the derived
   serialization does not: conversion moves to the provider boundary. Some
   local servers omit tool-call IDs; the provider must synthesize them.
2. **Stop reasons.** `PauseTurn`, `Refusal`, `ModelContextWindowExceeded` are
   Anthropic-specific. OpenAI's `finish_reason` set (`stop`, `length`,
   `tool_calls`, `content_filter`) maps into the neutral superset enum
   (`content_filter`→`Refusal`, `length`→`MaxTokens`, …). Document which
   variants each provider can emit.
3. **Usage accounting.** Fields are Anthropic's (`cache_creation_input_tokens`
   etc.). OpenAI reports `prompt/completion_tokens`, cached tokens nested in
   `prompt_tokens_details`, **only in the final chunk, only if
   `stream_options.include_usage` is set**, and many local servers omit it
   entirely. Usage becomes best-effort, possibly absent.
4. **SSE framing.** Anthropic: named typed events. OpenAI: uniform `data:`
   chunks plus a `data: [DONE]` terminator. Line-level SSE framing is
   shareable; event interpretation is per-provider.
5. **Thinking blocks.** `signature` / `RedactedThinking` are Anthropic
   round-trip state, kept as opaque provider passthrough; others ignore them.
6. **Auth + secret.** `x-api-key` vs `Authorization: Bearer` is trivial. Not
   trivial: **local providers need no key**, so the secret-file requirement
   becomes per-provider-optional. Key isolation rules are unchanged for any
   keyed provider.
7. **Request knobs.** `max_tokens` naming drifts (`max_completion_tokens`);
   local models want `temperature`/`top_p` and have small context windows.
   `ChatRequest` needs a few neutral optionals, mapped per provider.

### P0 - Weak-model robustness (niche-critical, not optional)
"Runs small local models well" is a pillar of the positioning, and small
models are markedly worse at tool-calling. Needed in the agent core,
provider-neutral: tolerant tool-argument handling (malformed/truncated JSON
args → schema-error `tool_result` and a retry chance, plus argument repair for
trivial cases and a bounded consecutive-failure cap); detection of tool calls
emitted as plain text (a known small-model failure); **per-model prompt
profiles**: a compact system prompt and trimmed tool descriptions for
small-context models (the OpenCode-ported prompts are Claude-sized); doom-loop
guard extensions (alternating-pair loops, empty responses). Plus a scripted
offline eval harness so "works with weak models" is measured, not claimed.

### P1 - Local/offline polish
Beyond the compat provider: keyless operation, graceful handling of absent
usage/IDs, context-window awareness (small local contexts → earlier, clearer
overflow behavior), a llama.cpp/Ollama quickstart, and an end-to-end offline
demo as the acceptance artifact.

### P1 - Session persistence
Serves the niche twice: constrained devices lose sessions (SSH drops, power
cuts), and offline work is long-lived. Plain JSON transcript save/resume;
neutral-typed history makes the format provider-independent.

### P2 - Richer editing (fuzzy-match fallback)
The deferred OpenCode-style matchers. Weak-model-relevant: small models
reproduce `old_string` imperfectly, so exact-match `edit` fails more for them,
which promotes it above pure parity, but behind the loop hardening that
decides whether weak models can drive the tools at all.

### P2 - Turn interruption
Known TUI gap (a hung turn can only be force-quit). Provider-agnostic, small
seam extension already scoped in `docs/TUI.md`.

### P3 - Multi-arch release packaging
armv7/aarch64/x86_64 musl-static builds + install docs. Cheap, and it converts
the positioning from claim to download link. x86_64-musl also lets
contributors try temur without 32-bit ceremony.

### LOW - parity for its own sake (explicitly deprioritized)
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
| T8 | Daily-driver UX (shipped as v0.2.0) | Slash commands + named-profile switching (P1); markdown rendering + TUI styling pass (P2); serve.sh background server launcher (P3) - released 2026-07-25 as v0.2.0 (private) |
| T9 | Command ergonomics (shipped as v0.3.0) | Per-profile prompt profiles (P1); /models listing + raw-model-id switching (P2); TUI command styling + Tab completion (P3); serve.sh MODEL_GGUF default (P4) - feature-complete 2026-07-25; shipped as v0.3.0 |
| T10 | Session management (shipped as v0.3.0) | Named multi-session per project (store P1); resume seam + lossy replay (P2); /sessions + /resume + /new + --resume (P3); TUI listing cell + backscroll rebuild (P4) - feature-complete 2026-07-26; shipped with T9 as v0.3.0 |
| T11 | Multi-model ergonomics (shipped as v0.4.0) | serve.sh model selection by name + candidate listing + RAM fit warn (P1); compact bash prompt file-ops hint (P2); weak-model eval indirect-tool-selection probe (P3); Ollama + LM Studio recipes + shortlist table (P4); live shortlist verification (P5) - feature-complete 2026-07-26; shipped as v0.4.0 |
| T12 | CI | Two-tier GitHub Actions: push gate (hermetic test job + release.sh artifact gate with a placeholder leak-patterns file) and dispatch-only container gate (full check.sh under podman); check.sh env-parameterized for CI, scripts otherwise unchanged - built on branch t12-ci, all three jobs verified green live (test, release-gate 4/4 artifacts gated, container-gate ALL CHECKS PASSED), merged to main 2026-07-28; on-main push trigger and container-gate dispatch verified green |
| T13 | Hosted provider verification | As built 2026-08-05, unparked once keys existed: the openai-compat provider run against the real OpenAI and Gemini endpoints, operator-typed in a separate live session so the build session never saw a key. init's key-file question says the path out loud (P1); per-model context_window in the anthropic template, read off the authenticated models API (haiku 200000, the other three 1000000; one shared constant was wrong for whichever tier it missed, and the error direction that matters is overstating, which would fire the advisory only past the real limit) (P2.5); openai-compat correctness, every item found by the live run rather than by review: assembled tool calls mean ToolUse whatever finish_reason says or fails to say (Gemini streams "stop" while attaching real calls, where its non-streaming path says "tool_calls"; the silent discard left saved sessions holding a tool_use with no tool_result), with Refusal the one exception since a filtered completion must not dispatch side effects, "length" still reporting truncation even alongside calls, and array-wrapped error bodies unwrapped so a 404 prints its message; plus hosted template repair from live evidence (openai gpt-4o with its 16384 completion cap baked, gemini gemini-3.6-flash) (P3.5); Gemini thought-signature round-trip, an optional opaque provider_state on the neutral ToolUse carried through sessions and echoed verbatim onto the wire it came from, absent and therefore invisible everywhere else (P3.6); docs, CHANGELOG, and the RUNBOOK acceptance record (P4). Live-verified per provider with caveats named: Anthropic and Gemini fully, OpenAI on gpt-4o with the gpt-5 era pending max_completion_tokens, xAI unverified for want of a key; post-build riders: 8865c65 promotes the container TUI pty stall from a three-cycle harness flake to product finding 13 on measured evidence (a 1.6 KB/s redraw spin, not an idle wait), and 6c9a3ee fixes the smoke itself, which was the cause of the stalls: blind sleeps against a 1.8s to 3.0s container startup put the scripted Enter inside the running turn, so input now goes through a held-open fifo behind readiness and turn gates under a 180s bound, gate-script only with no product change. Ships as v0.12.0 |
| T14 | Onboarding + one-shot mode | First-run quickstart replaces the raw secret error (P1); one-shot `-p` with a strict stdout-prose / stderr-chrome split, composing with --continue/--resume/--mock (P2); `temur init` guided starter config, four templates, empty-600 key files, by-path rule intact (P3); `temur doctor` read-only diagnosis with per-endpoint TCP/TLS probes (P4); docs + live keyless smoke vs local llama.cpp: init -> doctor -> live -p tool turn -> --continue -p chain, all first-attempt green (P5); interrupted one-shot exits 130 (P6) - feature-complete 2026-07-27; ships as v0.5.0 together with T12 and the stage-1 usage docs; built BEFORE T13, which awaits keys |
| T15 | Model-selection onboarding polish | As built 2026-07-28: one new provider fn `list_models_keyless(base_url, 3s timeout)` is the single network capability (unauthenticated GET of `{base}/models`, keyless openai-compat endpoints only, cannot touch key files by construction); `temur init`'s local template asks a Base URL question then offers the server's listing as a numbered picker, falling back to free text plus a baked two-model shortlist pointing at OFFLINE.md when no server answers (P1+P4); `/model <raw-id> --save` and `/model --save` persist to config.json via a surgical Value edit (unknown fields and key order survive; atomic write; profile-name save is a clean error; failed persist keeps the switch) - serde_json preserve_order enabled, with request bodies pinned byte-identical via sorted-key serialization so the pre-T1 goldens still hold (P2); doctor compares each keyless selection's model against the listing (PASS/WARN naming up to 10 served ids, advisory only; NOTE on listing failure; SKIP for keyed; --no-network skips) (P3); README/USAGE/OFFLINE docs with real transcripts, live smoke vs llama.cpp all green (P5). Rides CHANGELOG Unreleased, ships as v0.6.0 |
| T16 | Model-access footgun fixes | As built 2026-07-28: init's Anthropic template writes a curated four-profile set (fable/haiku/opus/sonnet over the current model tiers) sharing one key file, with the model question replaced by a startup-profile question (number or name, default sonnet, re-asks otherwise; effective default model stays claude-sonnet-5) (P1); `/model` no-arg appends two raw-id hint lines, the driver loop mirrors the UI's `/models` id cache into CommandCtx read-only, an unlisted raw id gets a non-blocking advisory, and ModelSwitched carries the provider so both caches drop on a provider change (P2); cross-provider hop: a claude-* raw id on a non-anthropic provider with an anthropic profile configured activates that profile in full (exact-model match, else first anthropic profile in name order) then applies the id on top when inexact, with escape hatches (cached-listing ids switch literally; no anthropic profile = local switch + hint) and `--save` composing to the hop profile's model with the site named in the notice; no new network calls anywhere (P3); riders: local template max_tokens 1024→4096, truncation notice names limit + source, sessions-autosave line in init closing text and quickstart (P4); README//model section, USAGE hop transcripts, live keyless smoke vs llama.cpp (init anthropic piped, exact + inexact hop, advisory, hop --save with semantic one-key config diff and restart proof, picker regression) all green (P5). Ships as v0.6.0 together with T15 |
| T17 | Provider onboarding bundle | As built 2026-07-29 (ladder renumber: T17 = this onboarding bundle, T18 = key guards, the former T17 model-floor seed -> T19, the former T18 context-lifecycle seed -> T20): `temur init --add <template>` merges a template into an existing config ALWAYS as named profiles (anthropic = the four-profile T16 set sharing one key file; openai/gemini/xai = one profile named after the template; local = keyless profile reusing the T15 base-URL question + picker), surgical preserve_order Value edit with atomic temp+rename, fail-closed on any profile-name collision naming every collision, startup `profile` key and all other bytes untouched, hop rule-2 hint renamed to `temur init --add anthropic sets one up` (P1); xai template (api.x.ai/v1, grok-4 free text, T13 still owns hosted live verification) in fresh + --add with README recipe in lockstep (P2); hidden key entry in the init wizards ONLY, right after a key file is created or found EMPTY: termios echo-off behind an RAII guard with SIGINT held off for the read, Enter/EOF skips, trimmed key written to the key file mode 600 + best-effort volatile buffer wipe, piped stdin reads plain for tests (placeholders only) - a deliberate NARROW amendment of T14's "init never accepts key material", contract + honest copy-limits in the RUNBOOK amendment record (P3); doctor key-rotation WARN off mtime, new config field key_rotate_warn_days (default 90, 0 off), future/unreadable mtime silent skip, advisory only (P4); README "Adding a provider" + CHANGELOG + live smoke (P5). Ships as v0.7.0 |
| T18 | Key isolation guards | As built 2026-07-29: three layers guaranteeing tools cannot reach configured keys, empty-guard invariant (keyless config = byte-identical pre-T18 behavior, no unshare, no probe, no redaction). Layer 1 KeyGuard in ToolCtx, built once at startup from every api_key_file (active + all profiles) + APP_SECRET_FILE: denies by lenient canonical path, by parent-dir prefix (sibling keys), and by dev+ino identity (hardlinks/renames), identities stat'ed once per tool execution; wired into read (before the is_binary open), write/edit (writes deny: key overwrite is destruction), grep (never reads protected files), glob (never lists them) (P1+P2). Layer 2 bash sandbox: pre_exec raw-syscall closure (no allocation after fork) does unshare(NEWUSER|NEWNS), setgroups deny before gid_map per user_namespaces(7), single-line self-maps, MS_REC|MS_PRIVATE on /, then /dev/null bind-masks over each existing key file; availability probed empirically (same sequence in a throwaway child) and cached; keys + no sandbox = bash refuses naming new config field allow_bash_without_key_sandbox (container serde default false, never disables a working sandbox) (P3). Layer 3: active-key redaction at the Registry chokepoint (Ok + Err, before the 30k truncation, len >= 8 only), key threaded from build_live_with_key with zero extra key reads, re-registered per successful /model build, cleared on keyless; doctor key-isolation count + sandbox PASS/WARN lines from the same constructions (P4). Live keyless smoke vs llama.cpp Qwen3-4B: read blocked verbatim, sandboxed cat empty, grep no hit, doctor lines, keyless bash unchanged (P5). Honest limits in RUNBOOK T18 record. Ships as v0.7.0 together with T17 |
| T19 | Model floor | As built 2026-07-29 (grounded scope: T4/T6 had already built repair+nudge+fuzzy-edit, so T19 raises the remaining harness floor per the Endor Labs harness>model evidence): context-scaled head+tail tool-output truncation (per-result cap = context_window clamped 4000..30000 chars, no window = 30000 exactly as before; true head + true tail around a one-line narrowing marker; redaction stays before truncation; cap follows /model switches via Session::build + switch_provider, the T18 redaction-key lifecycle) (P1); write read-first enforcement via a canonicalized ToolCtx read-paths set (read/edit/successful-write record; overwriting an existing unrecorded file fails naming read/edit; --continue and --resume start empty deliberately) + binary-format prompt nudge in write's prompt (served to both profiles) + bash inspection hint in read's binary denial (P2); prose tool-call EXECUTION, a flagged narrow T4-policy amendment recorded in the RUNBOOK (EndTurn + zero structured calls + exactly one candidate in a known shape + lossless-only inner JSON + registered tool + object args -> Registry::execute, result returned as plain user text since no tool_use id exists, goldens untouched; failed executions count toward NUDGE_LIMIT; config prose_tool_calls container-default true, false = detect+nudge exactly) (P3); eval tasks 8 gzip binary-nudge (gunzip validity proves no raw write) + 9 large-output tail (FINAL-LINE needle survives only via tail-keep), score /9 (P4); docs + live keyless smoke + RUNBOOK acceptance (P5). Ships as v0.8.0 together with T20 |
| T20 | Context lifecycle | As built 2026-07-29 (grounded scope: the Anthropic cache_control breakpoints turned out to exist since the initial commit, so T20 added no caching): `/compact`, fail-closed manual compaction (one summary call on the session's own model/system with tools omitted; structured-headings instruction for small local models; history replaced ONLY after a successful non-empty text response by summary + verbatim tail from the last user message holding no tool_result, merged alternation-safe as a leading text block inside the tail's first user message, summary-only when no boundary exists; estimate cleared, advisory re-armed, usage kept cumulative incl. the summary call, todos untouched, immediate autosave like /clear; replay-guarded; notice honest about the one-time cache-prefix rebuild) (P1); unified context advisory (fires once per latch period at used >= 80% of window OR window-used < max_tokens, whichever first; wording names /compact AND new-session; second trigger via the same Session::context_advisory accessor right after every seed load: --continue, --resume, /resume, one call site in main covering plain/TUI/one-shot-stderr) (P2); prefix-stability invariant tests both providers (H vs H+1: anthropic system+tools byte-identical and first |H| message elements byte-identical modulo the moving cache_control marker; compat whole-body-minus-messages byte-identical and messages(H) a byte prefix of messages(H+1); no violation found, goldens untouched) (P3); docs incl. /compact vs compact prompt-profile disambiguation + README context-lifecycle section with the llama.cpp --cache-reuse note, live keyless smoke (advisory fired live on the 80% arm, resume-time advisory fired at --continue pre-compact, live /compact 6 -> 2 messages, post-compact --continue clean), RUNBOOK acceptance (P4). Ships as v0.8.0 together with T19 |
| T21 | Bash approval mode + untrusted-host riders | As built 2026-07-30: decide_sandbox grows a fourth input (approver_available) and a fourth outcome Ask, strictly below the T18 arms (keyless never probes, a working sandbox always wins, the override runs plain and silences the ask); the Ask arm calls an interactive per-command approver with the exact command string, approve = one plain spawn (never cached), deny = fixed APPROVAL_DENIED as a normal is_error tool_result so the turn continues, default deny incl. an already-set cancel token (denies without prompting); approver installed only by interactive UIs (TUI via a ToUi::ApprovalRequest reply channel with a modal y/n/Esc prompt rendering the wrapped exact command; plain REPL y/N terminal prompt when stdin+stdout are TTYs), one-shot -p and piped stdin never install one; SANDBOX_REFUSAL reworded to lead with the interactive ask, override last; readiness-gated ScriptedSteps headless source + one-way TEMUR_TEST_SANDBOX_UNAVAILABLE probe seam power the new tests/approval.rs suite, TUI approve/deny headless e2e, and script(1) pty e2e (P1); init key-shaped mis-paste catch at the key file PATH question (no '/', >= 20 chars, [A-Za-z0-9_-]): value dropped and never stored, rotate warning, interactive re-ask, piped fail-closed; doctor refuse-arm names ask/refusal/override + README "Untrusted hosts" pointer (P2); headless key-pump flake fixed harness-only (Line steps start only when idle; busy-Enter drop untouched; 80/80 suite runs green incl. 40 under full CPU load) (P3); README "Untrusted hosts" (spend-capped throwaway keys, LiteLLM-style relay over per-profile base_url) + USAGE approval transcript + live smoke: sandboxed-no-prompt on host, GENUINE Ask arm inside a seccomp deny-unshare container vs llama.cpp Qwen3-4B (one approved, one denied, model adapted), init catch live, doctor amended arm live in the same container (P4). Ships as v0.9.0 |
| T22 | Context detection + discoverability | As built 2026-07-31: keyless llama.cpp /props probe (probe_props_context beside list_models_keyless, same no-auth-by-construction shape and 3s timeout, {root}/props with a trailing /v1 stripped, default_generation_settings.n_ctx, None on ANY problem since non-llama.cpp servers 404 it; the keyless-GET amendment now covers exactly TWO requests, RUNBOOK record) + doctor per-profile context checks (networked keyless openai-compat: PASS on match, WARN naming both values and the consequence direction on mismatch, WARN with the exact config line when unset; every provider offline: one NOTE per profile with no context_window that the advisory and tool-output scaling are off; keyed and --no-network never probed) (P1); init auto-fill (local template fresh + --add writes the detected n_ctx as context_window with a source-naming notice, baked 8192 byte-identical otherwise; anthropic template's four profiles gain context_window 200000, knowledge-based pending the operator's live /models confirmation, T16 haiku-alias precedent; openai/gemini/xai bake nothing; existing configs never rewritten) (P2); /models context enrichment on the anthropic wire (parse_models_entries sibling reads per-model max_input_tokens, 0/absent = unknown; active anthropic model with a known window: configured larger = warning, unset = hint naming the exact config line, equal/smaller = silence; T16 cache carries the windows, refreshed by the command layer, still cleared on provider change; zero new network calls, mock/fixture coverage only) (P3); docs (USAGE/OFFLINE detection story, README advisory-only + detectable note and updated anthropic recipe, CHANGELOG Unreleased) + live keyless smoke + RUNBOOK acceptance; the planned init-closing autosave line was found already shipped by T16 P4 (aac3852), verified live instead of duplicated (P4); post-build riders: addendum-2 docs claims audit + P5-follow-up em-dash sweep and AI-agent/bring-your-own-model scope wording, both prose-only. Ships as v0.10.0 |
| T23 | Launch readiness | As built 2026-07-31: prose and layout only, no Rust and no gate-script changes: root tidy (the setup and v1-plan documents to docs/SETUP.md and docs/IMPLEMENTATION_PLAN.md, every live reference updated) (P1); README rebuilt around the eval table and a two-screen path to Install (533 -> 237 lines; the ~180 target gave way to keeping the pinned install block, the eval table, and the honesty material whole), deep reference merged into docs/USAGE.md (523 -> 854 lines), compiled-string headings and the five tag-pin lines byte-identical (P2); milestone codes out of user-facing lead lines (CHANGELOG lead inversion, README prose, Cargo.toml comments; RUNBOOK record titles verbatim) (P3); "How this was built" README section; the CLAUDE.md preface is drafted but deliberately uncommitted pending operator sign-off in session (P4); scripts/bump_version.sh stage-1 four-file bump helper (refuses dirty tree / bad version / skew, prints the diff, never commits; scratch-branch tested including all three refusal paths), em-dash, stale-reference, and relative-link sweeps clean (P5); rider 80b0dc3 (operator-approved CLAUDE.md preface verbatim, README caps polish). Ships as v0.11.0 before the public flip |
| T24 | Session cost visibility | As built 2026-08-07: `/status` gains one line estimating the session's dollar cost for keyed hosted profiles, computed locally from the already-tracked session usage at per-profile configured list prices. Config: `price_input_per_mtok` / `price_output_per_mtok` on a profile, carried through `ResolvedProfile`, validated at resolve time (negative or non-finite names the field; half a pair names both, since a profile that looks priced but silently shows no estimate is worse than one that refuses to start); the base non-profile selection has nowhere to carry them, so the estimate is a profiles feature (P1). New `src/cost.rs` holds the whole computation, small and pure: input*pin + output*pout, plus Anthropic's published cache multipliers (reads 0.1x, writes 1.25x the input rate, knowledge-of-record 2026-08-07) for the anthropic provider ONLY, because that is the one wire reporting cache tokens as separate counts; on the openai-compat wire cached_tokens is a subset of prompt_tokens, so plain in/out slightly overstates instead, the safe direction for a spend-awareness number. Absent usage fields contribute zero. The line renders only when the selection is keyed (anthropic always, openai-compat with a key file), both prices are configured, and usage has been reported; otherwise it is absent entirely, with no nag (P2). The anthropic template bakes per-model USD list rates (fable 10/50, haiku 1/5, opus 5/25, sonnet 3/15, standard rate not sonnet's introductory 2/10 which lapses 2026-08-31), wizard and `--add` in lockstep through serde_json, every golden pin updated; openai/gemini/xai/local bake nothing, because a wrong price is worse than none (P3). Docs name both error directions honestly (understated where a provider omitted thinking tokens from its usage, the T13 F11 floor, since narrowed by T25 to wires that report no usage at all; overstates a cache-heavy compat session), and the wording says estimate, never bill; no live smoke, the feature is offline-computable and a keyed check rides a future operator run (P4). Ships as v0.13.0 |
| T25 | Token-cap wire name + thinking-token accounting | As built 2026-08-08, two T13-acceptance findings that both live on the openai-compat wire but point in opposite directions, request and response. F7: `max_tokens_parameter` on a profile (and on `openai_compat` for the base selection) picks which key carries the cap, exactly `"max_tokens"` (the default) or `"max_completion_tokens"`, anything else a startup error naming both; setting it on an anthropic profile is a startup error too, since that wire uses `max_tokens` natively and a silently ignored key reads as a setting that does nothing. New `MaxTokensParam` in the provider layer is parsed and validated at resolve time and carried on `ResolvedProfile`, so holding one is proof it was checked; `build_body` became a method and emits the configured name for the same u32, with exactly one of the two keys ever present and the T20 sorted-json contract handling order. Absent field = byte-identical requests for every config written before it, and no template bakes the field, because the OpenAI template defaults to `gpt-4o`, which wants the classic name (P1). F11: the wire `Usage` now parses `total_tokens`, which it had been dropping silently even though the live conformance suite already listed it as required, and the conversion folds any excess of the total over the sum of the named counts into `output_tokens`. Gemini bills thinking tokens and counts them in its total while naming them in no field: the capture of 2026-08-05 at `t13-live/evidence/f12-nostream.txt` reports 48 prompt + 19 completion against a total of 103, so 36 tokens of thinking spend were invisible. Saturating throughout, and a no-op unless a server both reports a total and reports one larger than its own parts, so OpenAI (reasoning counted inside `completion_tokens`) and llama.cpp (exact sums) are untouched; T24's estimate tightens on Gemini rather than distorting elsewhere, and the docs' understatement caveat narrows to wires that omit usage entirely (P2). Docs, the profile recipe, and the ROADMAP/CHANGELOG updates, with both claims marked offline-verified pending the live leg (P3). The operator live leg is staged in `t13-live/CHECKLIST.md`: a gpt-5 era id first WITHOUT the field, capturing the 400 body that has never been captured, then with it; and one Gemini streaming turn proving the gap arrives through `include_usage`, which the live streaming curl never exercised because it omitted `stream_options.include_usage` (P4). Rider 2026-08-10: the operator ran both legs, both claims are live-verified, and the docs drop the pending wording. F7 on `gpt-5` produced the 400 body that had only ever been described in prose, then completed the same prompt with the field set, tool call included; F11 on the streaming path recorded 28 output tokens for a turn reporting 6498 prompt + 1 completion against a total of 6526. The Gemini capture also corrected a wire assumption the fixture had modeled wrong: usage arrives on EVERY chunk, and the finish chunk carries a non-empty `choices` array, so the count is right only because assembly is last-wins rather than additive; the streaming fixture is rebuilt from that capture and the RUNBOOK gains the T25 acceptance record (P5). Ships as v0.14.0 |
| T26 | Mid-session cost advisory | As built 2026-08-11, the escalated half of dogfood item 9: T24 made session cost visible in `/status`, but only to a user who thinks to ask, and the motivating incident was a single agentic `-p` turn that reached roughly $26 across about 200 loop iterations (40M cache-read tokens, reads 73% of the bill) and was discovered only afterward by pricing the usage line by hand. The advisory fires DURING such a turn: `src/cost.rs` gains the crossing arithmetic as small pure functions (step_multiple = floor(estimate/step), used for both the latch's initial value and every crossing, so "already accounted for" is one piece of arithmetic; advisory_crossing returns only the HIGHEST multiple a jump cleared, never a burst; advisory_message pins the wording), and the new global `cost_advisory_step_usd` sets the step (absent = 5.0 by operator decision of 2026-08-11, 0 disables, negative or non-finite a startup error naming the field). Global rather than per-profile on purpose: a price is a provider property, a budget is a user preference, and it must not reset because a `/model` switch landed on a profile that forgot to repeat it (P1). Wiring reuses rather than duplicates the T24 gate: the new `CostRates` type IS the selection half of that gate, so constructing one proves a keyed priced selection is active and `/status` and the advisory read the same estimate through the same path. The session carries the rates (main.rs at startup from the resolved profile, replaced by `switch_provider`, exactly the way `context_window` travels) plus the validated step and the latch; the latch is recomputed, never zeroed, at all four points where past spend must stop being news (construction, seed load, `/clear`, provider switch) and is NOT persisted, being a pure function of usage and rates the session already holds, so the session file format is untouched. Accrual funnels through one `Session::accrue_usage`, and the advisory is polled at every point that both follows accrual and has a UI sink: the turn loop after each response (beside `context_advisory`, which is what makes it mid-turn), the interrupted-turn landing, and the command layer after `/compact`'s own summary call; the latch only moves forward, so overlapping polls are idempotent by construction. On by default WHEN PRICED was the explicit operator choice, since an opt-in spend alarm is off exactly when it is needed, and unpriced, keyless, and local configs can never see it (P2). No new surface: the TUI renders Notice already, and `-p` renders it as stderr `[!]` chrome, which is precisely the $26 scenario, with tests pinning that stdout stays exactly the answer. Docs, CHANGELOG, and this row; no live gate, the feature is offline-computable and T24's keyed live check still rides a future operator session (P3). Ships as v0.15.0 |
| T27 | Small-items bundle | As built 2026-08-12, the queued T13-acceptance list cleared in one pass, offline throughout. `Session::switch_provider` takes the `ResolvedProfile` instead of four of its fields plus derived cost rates: six positional parameters become three, the "a switch replaces the whole selection" rule becomes structural rather than conventional, and the next selection-scoped setting costs no signature change at any call site; `max_tokens_source` stays separate because it is the NAME the selection is active under, which only the caller knows. Byte-identical behavior, proven by the existing switch tests (P1). TUI trio: `Cell::TurnTail` carries the model captured at push time, so a `/model` hop no longer relabels every past turn in the scrollback (finding 2); the Refusal arm closes the tool cells its own stream opened, mirroring the interrupt path's FIFO pairing including the unnamed-block exemption but synthesizing nothing into history, since the refused output is discarded whole; and `--tui` against a pipe is a usage error naming `-p` and `--plain` instead of drawing a prompt it can never read and spinning at roughly 1.6 KB/s of redraw output (finding 13a; part (b), the check.sh readiness gate, shipped in v0.12.0) (P2). `/models` trio: the anthropic context enrichment falls back to dated listing entries when the active id is a bare alias absent from the listing (`claude-haiku-4-5` judged through `claude-haiku-4-5-20251001`), used only when unambiguous (one candidate, or several agreeing on one window) and always naming the dated id it matched so the inference is visible; a configured window SMALLER than the reported one gains a hint instead of silence, direction wording mirroring doctor's server-allocation check; and the "two ids on one line" report was NOT reproduced, deliberately not fixed blind, and the reproduction attempt is kept as a regression pin (P3). `temur doctor` gains an install-skew check: the first `temur` on PATH against the running binary, metadata and bytes only, PASS on the same file or a byte-identical copy, WARN naming both paths, both mtimes, and which is newer, never a FAIL and never executing what it finds, since running a binary discovered by searching PATH is exactly what a diagnostic must not do; plus docs and the CHANGELOG entries (P4). No live leg: every item is offline-verifiable. Ships as v0.16.0 |
| T28 | Skill compacting (section index) | As built 2026-08-12: a loaded SKILL.md is re-sent on every subsequent request forever, and one larger than the context-scaled tool-output cap (4,000..30,000 chars) was SILENTLY lossy, middle-elided by the central truncation with advice to "narrow the command, e.g. grep or head/tail" that is meaningless for a document the model asked for by name. src/skills.rs gains two pure functions over the file's bytes: minify() drops a frontmatter block holding only name:/description: (already relayed via <available_skills>), trims trailing whitespace, and collapses blank runs, all outside fenced code, which is copied byte for byte because whitespace is semantic in a heredoc or in Python, with "never grows" and "idempotent" pinned over a corpus; and scan_sections(), a hand-rolled fence-aware ATX scanner with HIERARCHICAL extents (a section carries its subsections) whose reconstruction invariant is pinned: intro plus every top-level section, concatenated in order, equals the minified document byte for byte, which is what licenses the "nothing is summarized or omitted" claim. Setext headings are deliberately not indexed, --- being ambiguous with a frontmatter delimiter and a thematic break (P1). ToolCtx gains output_cap, set by Registry::execute from its own T19 cap before dispatch, and Tool gains truncation_hint(), defaulting to today's advice byte-identically so the existing marker pins pass untouched (P1). The skill tool then has three modes: at or under the cap, today's <skill_content> with minified content; over it, <skill_index> with the intro verbatim plus every heading numbered with level and size (a 48,427-char skill yields an 846-char index, 1.7%); and an optional "section" argument fetching one part by number or heading text, matched case- and whitespace-insensitively against the scanned list ONLY, so it never reaches the filesystem and "../../etc/passwd" is an ordinary miss. Edge rulings all pinned: at-cap is full mode (an index is never itself truncated); a heading-less skill, or one whose prose before the first heading already exceeds the cap, stays full and is centrally truncated with the new section-oriented hint, because an index that does not fit is not an improvement; sectioning works on small skills too; an oversized single section is returned and truncated, never re-indexed (P2). Proven through the agent loop with a scripted provider (index, then a numbered section whose bytes are asserted identical to that section's slice of the minified body, then an answer), with prefix stability asserted across all three round trips, plus the beneficiary pin: one mid-size skill returns FULL with no context_window and INDEX at 8k, configuration alone (P3). DELIBERATE NON-CHANGES, each a design decision rather than an omission: NO persisted index or cache, because a pure function recomputed per call cannot be stale and needs no invalidation machinery; NO session-state dedup of repeat loads, which would be a layering violation (the tool would have to read conversation state) and would fight resume semantics, where a restored session legitimately re-reads what it needs; NO /compact integration, because post-compact re-fetching a section through the index is cheap and history stays append-only; and history STAYS APPEND-ONLY, which is what keeps the prompt-cache prefix valid and is pinned rather than asserted. Docs state the honest scale: minification saves 0.0% on tidy markdown and 2.2% on a sloppy SKILL.md, so the index, not the minifier, is the mechanism (P4). No live leg: every claim is offline-verifiable. Ships as v0.17.0 |
| T29 | Local-model coverage matrix | As run 2026-08-12, measurement only: no Rust changed, every finding recorded rather than fixed. Nine models through the nine-task eval on one day under identical conditions (compact profile, llama.cpp `server-b10068`, ctx 8192, `--jinja`, stock knobs, i686 musl-static binary in the i386 container), replacing a table whose two "verified" rows were seven-task records from 2026-07-26 and whose other two were undated "reported" hearsay. Scores: Qwen3-4B-Instruct-2507 9/9, Qwen2.5-Coder-3B-Instruct 8/9, Qwen2.5-Coder-1.5B-Instruct 7/9, Qwen3-1.7B 6/9, Qwen3-0.6B 4/9, Llama-3.2-3B-Instruct 1/9, Gemma-3-4B-it 0/9, Phi-4-mini-instruct 0/9, SmolLM2-1.7B-Instruct 0/9. Three of those zeros are not about the models: a three-way differential (system+tools, system only, user only) shows llama.cpp `--jinja` silently dropping the TOOLS array for gemma-3, Phi-4-mini and SmolLM2, whose bundled templates have no tool support, at HTTP 200 with no warning (28/28, 22/22, 35/35 prompt tokens against a Qwen3-1.7B control at 207/30), so those models are never told tools exist and invent shapes like `{"tool": "file_delete"}`. Llama-3.2-3B does receive the tools and fails differently, on llama.cpp's own grammar rejecting its output server-side. The largest movement was Qwen2.5-Coder-3B, 0/7 to 8/9, caused by a temur change and not a model one: T19's prose-call recovery now executes the plain-text calls that made it score zero, and the same feature's leading-token rule is why its 1.5B sibling still loses calls. P4 took the first live observation of T28's skill index, three models against an 11,674-char skill under an 8,192-char cap: Qwen3-1.7B walked the index to `section: "5"` and answered correctly with no hint, Qwen3-4B ignored the affordance and grepped the filesystem instead, and Qwen2.5-Coder-1.5B reached for `section` before ever seeing an index. The eval's own limits were measured too and are queued below rather than patched mid-matrix. Ships as v0.18.0 |
| T30 | Model floor, round two | As built 2026-08-12, four items from the T29 matrix, offline throughout. Finding 1: `detect_text_tool_call` now also scans for FENCED blocks anywhere in a message, strips each fence, and applies the checks it always applied to the whole-message body (parses as JSON or as its first balanced object, names a REGISTERED tool under "name"/"tool", carries an arguments-like key), first hit wins; a bare JSON object mid-prose WITHOUT a fence stays undetected on purpose, since prose quoting a call shape while discussing a plan is common and a fence is the only cheap evidence of intent. The EXECUTION predicate `extract_prose_tool_call` is byte-identical, so this converts silence into a retry rather than widening what runs, pinned both ways with the exact Qwen2.5-Coder-1.5B eval-task-8 shape (unit table plus a loop-level test whose file content proves the structured retry is what landed) and bounded by the same NUDGE_LIMIT (P1). Findings 8 and 6, two honest-output changes: `Base directory for this skill: <path>` becomes conditional in all three skill modes, emitted only when the skill directory holds an entry besides SKILL.md (one `read_dir` at render time, unlistable counts as bare), because two of three models observed against an over-cap skill were pulled off the index by it (Qwen3-4B grepped the directory and gave up; Qwen3-1.7B answered from section 5 and then wrote its answer INTO that directory), while a skill that ships playbooks or assets keeps the line and byte-identical output; and a successful `write` over a non-empty file appends "replaced N bytes of prior content", always and with no smallness threshold, sizes as u64, the read-first guard untouched because it permitted the eval-task-5 destruction correctly (the model HAD read the file) and what was missing was the trace. The T28 reconstruction invariant, index, and section selection are untouched; two pins that located a section payload by its first blank line now find it after the opening tag, one tightened to a byte-exact whole-wrapper assertion (P2). Finding 7, the operator-approved default flip: init's local template bakes `qwen3-4b`, the fallback shortlist leads with Qwen3-4B-Instruct-2507 as the primary recommendation and keeps Qwen3-1.7B second as the low-RAM choice (the table's primary row and its floor, deliberately not its top two rows), OFFLINE.md moves "(primary)" to the 4B row and marks the 1.7B the low-RAM floor, and every golden that baked the old default moved with it (init's README-recipe render, the picker's template-default and listing-failure fallbacks, two cli.rs pins); remaining `qwen3-1.7b` strings are arbitrary ids in unrelated fixtures and examples or historical records (P3). DELIBERATE NON-CHANGE: the matrix was NOT re-run. Every score in OFFLINE.md stays dated 2026-08-12 and describes the binary as it was measured. Finding 1 is expected to raise Qwen2.5-Coder-1.5B, whose lost calls it addresses, and that expectation is unverified until the next matrix pass measures it. UPDATE 2026-08-14: T31's dogfood re-run of that model scored 5/9 and neither confirms nor refutes the expectation (one model, one day, and the tasks that moved are not the ones the fix targets); the formal check still waits on a matrix pass. DISPOSITION 2026-08-15 (T32): NOT CONFIRMED at the score level and closed as unprovable by this instrument. The T32 P0 bridge run put the shipped v0.20.0 binary through the UNCHANGED harness on Qwen2.5-Coder-1.5B and scored 5/9, so across 0.18.0-era 7/9, 0.19.0 5/9 and 0.20.0 5/9 the score never rose; what the fixes demonstrably changed is the SHAPE of the failures, not the count. The T32 matrix pass then measured that model at 4/9 twice with only two of nine tasks passing both times, which means the run-to-run noise on this model is larger than any effect finding 1 could have had. Re-testing it needs a lower-variance instrument, not another pass. Ships as v0.19.0 |
| T31 | Model floor, round three | As built 2026-08-14, seven findings from operator dogfood day 1 plus a Qwen2.5-Coder-1.5B eval re-run, offline throughout except one live serve.sh check. H1: prose-call recovery executed a byte-identical resend as a fresh call every time, and eval task 8 has one model resending a single fenced `write` about sixty consecutive times until the context window overflowed, because NUDGE_LIMIT bounds nudges and FAILED executions only while successes are uncapped. `ProseRepeatGuard` remembers the last DISPATCHED call (executed or failed, since the failure text is fed back either way); a resend equal in tool name and argument VALUE is answered with a notice rather than run, the notice counts against the same cap so a stuck model ends its turn, and any change of name or argument resets the guard, key order included (serde_json runs with preserve_order, and IndexMap equality is order-independent). Structured `tool_use` repetition keeps the M2 doom-loop guard and is deliberately out of scope: no evidence of harm there. H3: eval task 7 died in three seconds at 31 output tokens because a fenced `{"name": "delete", "arguments": {...}}` matched neither predicate, both requiring a REGISTERED name, which is the false-positive killer and stays. `detect_unknown_tool_call` is a sibling of the detector rather than a widening of it, so every existing pin in `detect_text_tool_call` is literally unchanged; the loop names the bogus tool and lists the registry, never a hardcoded set, never executes, and is capped like every other nudge. It requires a FENCE and an arguments-like key, so the unfenced whole-message pin and the `{"name": ...}` package.json-fragment pin both hold; the registered paths keep priority (P1). H2: bash treated a `""` workdir as a path, failing the spawn with "No such file or directory (os error 2)", after which the model parroted that error text into its next call's arguments; empty or whitespace now means absent, a named-but-missing workdir still errors. D3: `read`'s binary refusal pointed every file type at one hint, so a PDF was sent toward `unzip -l`; known types now get a remedy they can run (pdftotext, unzip -l, zcat, tar -tf, "ask the user to describe it" for images) and unknown types keep the pre-T31 sentence byte-identically, pinned (P2). The doctor probe: llama.cpp `--jinja` drops the tools array for templates without tool support at HTTP 200 with no log line and no response signal, re-confirmed on `b10423-a94d563ed` on 2026-08-14 (gemma-3-4b 10/10, Phi-4-mini 4/4, SmolLM2 31/31 prompt tokens with and without tools, Qwen3-4B control moved), so this is current behavior rather than a fixed historical quirk; reported upstream 2026-08-14. Doctor sends one tiny completion twice, bare and with a single probe tool, and compares `usage.prompt_tokens`: identical WARNs naming both counts and the consequence, differing PASSes naming both, anything unusable is a NOTE and never a FAIL. `probe_prompt_tokens` is the THIRD and last keyless request doctor may make and takes a base URL and a model id and nothing else, so it cannot attach an auth header or touch a key file by construction; active selection only, openai-compat only, keyless only, absent under `--no-network`, capped at one generated token each (P3). D1: both prompt profiles gain one sentence telling the agent it can see the filesystem and to list or read before claiming it cannot, after qwen3-4b denied file access to a conversational "can you find it in the folder?" while holding file tools and used them at once for the same request phrased as an instruction; no golden pins the default prompt, so nothing rippled. D4: `serve.sh start <name>` against a running server printed OK and kept serving the OLD model, silently poisoning a measurement; it now fails naming both models and the stop-then-restart sequence when a model was REQUESTED (name argument or MODEL_GGUF), verified live against a stub container across all four paths, while the no-model-requested path is byte-identical (P4). DELIBERATE NON-CHANGES: the matrix was NOT re-run, so every score in OFFLINE.md stays dated 2026-08-12; native `tool_use` repetition stays unguarded beyond the doom loop; D2 (the prompt-only-no-call shape) and a serve.sh SERVER_ARGS knob are queued below, not built. VERIFIED 2026-08-15 (T32 P0): the T32 bridge run exercised all three H-fixes live against this milestone's own eval task set. H1 is confirmed and was WIDER than recorded here: the unbounded resend also ran on eval tasks 1 and 4 (62 and 60 consecutive executions, each ending in an HTTP 400 context overflow), both of which PASSED anyway, so the defect was invisible in the score and showed only as cost; task 8's input tokens fall from 321,207 to 11,530, a 96.4% reduction, and the bridge run has zero context overflows against three in the archive. H3 is confirmed, firing the unknown-tool notice exactly where the archived transcript had 31 output tokens of silence, though the task still fails, which is what firing was claimed to fix and no more. H2 remains UNEXERCISED: the model sent no `workdir` at all that run, so the empty-string path was never entered and task 6 moved on a model-side difference. The doctor tools-drop probe also got its first live leg (T32 P2), reproducing this milestone's three hand-measured counts exactly on a different server build. Ships as v0.20.0 |
| T32 | Eval harness round two + matrix refresh | As run 2026-08-15, the five surviving T29 queue items cleared and the whole matrix re-measured against the shipped v0.20.0 musl binary (sha256 `f09a3897...`), keyless throughout. Harness (`scripts/weak_model_eval.sh`, one commit): item 2, eval tasks 2 and 9 no longer print a literal placeholder for the model to copy, naming the value indirectly instead while the host-verified assertions stay byte-identical; item 3, the `max_tokens` literal becomes `EVAL_MAX_TOKENS` defaulting to 3072, verified offline not to trip llama.cpp's prompt-only ctx rejection at 8192; item 4, `EVAL_RUNS` (default 1) repeats the nine tasks with the server and pod built ONCE, writing `task<n>.run<r>.txt` and `results.run<r>.txt` and one score line per run; items 5 and 9, each task now gets a mounted state dir (`XDG_STATE_HOME`) and every FAILED task's work dir, state dir and results file are archived out of the temp tree before the `rm -rf` teardown, which stays. The state dir is a SIBLING of the work dir, not a child as first specified: mounted inside, the session JSON lands where the tasks' own assertions grep and three tasks would score against their own transcript. Item 9 is closed with the argument capture it asked for: Llama-3.2-3B emits a structurally perfect `edit` call carrying `"replaceAll": "false"`, the JSON string rather than the boolean, resends it verbatim, and dies to the repeat guard. Matrix: ten models, two runs each (the three tools-drop rows once, since a second 0/9 measures the same template), llama.cpp pin bumped to `server-b10438` across all three scripts and OFFLINE.md in a second commit before the runs. Scores as pairs, with a third run where the pair differed by 2 or more tasks (operator-invoked 2026-08-16): Qwen3-4B-Instruct-2507 9/9 9/9, Qwen3-4B-Thinking-2507 7/9 9/9 9/9, Qwen2.5-Coder-3B-Instruct 6/9 9/9 7/9, Qwen3-1.7B 7/9 7/9, Qwen3-0.6B 5/9 5/9, Qwen2.5-Coder-1.5B-Instruct 4/9 4/9, Llama-3.2-3B-Instruct 2/9 2/9, Gemma-3-4B-it, Phi-4-mini-instruct and SmolLM2-1.7B-Instruct 0/9. NOT comparable to the 2026-08-12 table: server build, `max_tokens` and two task wordings all changed at once. The headline result is item 4's, now with numbers: two models changed score between consecutive runs under fixed conditions (Coder-3B by 3 tasks, Thinking by 2) and two more held their score while the task set moved under them (Coder-1.5B kept 4/9 with only two of nine tasks passing twice), so a one-task difference between rows is not a real difference. The third runs sharpen that: Thinking settled at 9/9, but Coder-3B returned a THIRD distinct score (7/9) landing between the first two, so the tiebreak did not break the tie and the row is published as a triple rather than as a representative number. DELIBERATE NON-CHANGES: the task count stays 9 and the seeds stay fixed, so round two remains comparable to round three; no auto-third-run logic, the 2-task rule is the operator's to invoke; the three template-limited families stay in the table with their scores, since the fix is upstream at ggml-org/llama.cpp#27129. Ships as v0.21.0 |
| T33 | Tolerant scalar coercion | As built 2026-08-16, the two items T32 queued, offline throughout. P1: the four non-string scalar arguments in the tool schemas (`edit` `replaceAll`, `read` `offset` and `limit`, `bash` `timeout`) gain field-level serde `deserialize_with` helpers in `src/tools/coerce.rs`. Deliberately NOT a central value-walk at `parse_input`: a blind `"false"` to `false` rewrite would corrupt legitimate string fields, and an `oldString` of `"false"` must stay the four-character string, which is the milestone's no-corruption rule and is pinned by a test that edits a file whose CONTENT is `false`. Accepted, and nothing else: the strings `"true"`/`"false"` for a bool, an ASCII-digit string for a `u64`, and `"null"` for an `Option`. DELIBERATE NON-CHANGES: no float coercion, no trimming, no case tolerance, no sign handling, and `input_schema()` byte-identical for every tool, so the schema stays the contract and this is parse-time tolerance rather than a relaxation of what temur asks for; `"maybe"`, `""`, `"12.5"`, `"-3"`, `"True"` and `" true"` all still fail LOUDLY, with a message naming the accepted forms so the loop stays self-healing, and real floats and negatives keep their pre-T33 serde wording because non-string values are delegated to the ordinary `from_value` path. The `Option` fields also gain `serde(default)`, since naming a `deserialize_with` turns off serde's implicit missing-Option-is-None. P2: `EVAL_TASK_TIMEOUT` binds for the first time. Measured on podman 4.9.3 rather than inferred, a 30s container against a 5s bound: `timeout` (SIGTERM to the podman CLIENT) 32s and never bound, `timeout -s KILL` 5-8s but the container SURVIVES the client and keeps running in the pod, `podman run --timeout` 7s with conmon killing the container and `--rm` still removing it. So the bound is podman's own `--timeout`; the outer `timeout` stays as a SIGKILL backstop at cap+60s and is the only path that can orphan a container, hence a deterministic `--name` swept before and after each task. A killed task records FAIL with a `TIMEOUT@<n>s` note in a new fifth results field, forced to FAIL even where its assertion would have passed, since what is on disk came from a run that did not finish under the stated conditions; detection needs BOTH a nonzero exit AND elapsed at or past the cap. Default 300 to 1200, and `0` now disables the bound explicitly. P3, Llama-3.2-3B only (Qwen2.5-Coder-1.5B deliberately NOT re-measured: its one invalid-argument event is a range check coercion does not touch), 2 runs, same server build, ctx, profile, `max_tokens`, seeds and task wording, musl binary sha256 `1abf58ee...`. PRIMARY METRIC, re-enumerated over the archived session JSONs and validated first by reproducing T32's sixteen exactly: stringified-scalar rejections 16 to ZERO, by two independent counts (tool_result blocks, and the same signature anywhere in the session). Score 4/9 and 3/9 against 2/9 and 2/9, SECONDARY and read as two samples against two: the server-side peg-native grammar rejections are barely moved (9 to 8) and still dominate the row. HONEST RESIDUAL, queued below: every `offset` this model sent was a string, and while `"1"`, `"2"` and `"null"` now parse and run, the nineteen `"0"`s parse and then fail `read`'s 1-indexed range check, so on that subset a type rejection became a range rejection rather than a success. The re-measure also validated P2 from the other side: its slowest legitimate task took 434s, no run carried a timeout marker, and no published number changed. Granite-3.3-2B and Hermes-3 were still not on disk, so the matrix stays TEN rows. Ships as v0.22.0 |
| T34 | Template interop | As built 2026-08-18 from the verified template experiment of 2026-08-17 (archive `~/temur-eval-archive/template-experiment-2026-08-17/`), offline except keyless local llama.cpp. P1: `src/tools/skill.rs` declared `"section": {"type": ["string","number"]}`, and `SkillTool` is registered unconditionally, so that union type rode in EVERY tools array temur sent. llama.cpp re-renders the chat template on every request when no specialized handler matches the model, and the shipped Hermes-2-Pro template's `json_to_python_type()` opens with a dict lookup keyed on the schema's `type`; a list key is unhashable, so the render threw and the server answered HTTP 400 before the model saw anything. Stock Jinja2 raises identically: a latent bug in that template class which temur was unusually good at triggering. Now `"type": "string"`, with NO execute-path change, because tolerance for non-string spellings belongs at the argument boundary (T33) and `{"section": 2}` still works and was already pinned. The durable guard is a registry-wide test walking every tool's `input_schema` recursively across BOTH prompt profiles and asserting no `"type"` at any depth is an array. VERIFIED LIVE: the Hermes template that 400d on every request in the archive now PASSes 221 to 7156 prompt tokens against the same binary, so the union type was the whole defect. P2: `provider/mod.rs`'s tools-drop probe sent ONE synthetic minimal tool, so on 2026-08-17 it reported PASS (221 to 290) against exactly that server. The probe now builds the registry the way startup does (same skill-dir resolution, the selection's own resolved prompt profile) and serializes it through the SAME openai-compat mapping the provider uses; the amendment contract is unchanged and still asserted, there being no credential-shaped parameter to pass. `probe_prompt_tokens` returns a `ProbeOutcome` enum (Ok / HttpError with status and the server's own message / NoUsage / Unreachable) instead of an `Option<u64>` that discarded the diagnosis, with the message parsed by the streaming path's own `ErrorPayload` so every envelope shape is absorbed. New doctor arm, the Hermes catch: bare OK plus a with-tools HTTP error is a WARN quoting the server, since every real turn will fail identically; still never a FAIL, and a failing bare request stays the NOTE class. UNPLANNED FINDING, caught by the live smoke: sending the real definitions makes the server prefill ~24KB of tool text, measured 4814 prompt tokens at 22.6 ms/token, 106 seconds, on this CPU-only machine, which the 3s keyless listing timeout turned into a silent "no usable prompt_tokens" NOTE. The probe gets its own `TOOLS_DROP_PROBE_TIMEOUT_SECS` (300), doctor announces the probe before going quiet for it, and an unanswered with-tools request is its own NOTE naming which request went unanswered. That prefill is not invented cost: it is what the first real turn pays for the same bytes, and it warms llama.cpp's prompt cache for that turn. P3: `CHAT_TEMPLATE_FILE` in `serve.sh` and `weak_model_eval.sh` (validated at preflight, mounted read-only at a fixed destination, passed as `--chat-template-file`); unset is byte-identical to before, `offline_demo.sh` is deliberately excluded as a fixed acceptance demo on a known-good model. Loud banner at bring-up and again at the end, plus a template header on every archived results file. `serve.sh`'s already-running comparison used a bare `{{range .Mounts}}{{.Source}}{{end}}`, which concatenates every mount source and would have broken the moment a template joined the model; mounts are now selected by destination, which also extends the T31 D4 mismatch check to the template for one more inspect. DEVIATION: that template mismatch FAILs like the model mismatch rather than WARNing as the plan worded it, because a running server with the wrong template poisons a measurement exactly the way the wrong model does and the stop-then-restart remedy is identical; the planning session can reverse it in one line. P4: OFFLINE.md stops implying the three 0/9 models cannot drive tools, per-model causes recorded (Phi-4-mini's bundled template reads a per-message `tools` key and never the top-level variable, publisher report drafted 2026-08-18, not yet filed; the defect is still live in the published `tokenizer_config.json` as of 2026-08-18; SmolLM2 has no tool branch at all; gemma-3-4b unresolved), plus a separately captioned substitute-template subsection carrying Phi 4/9, SmolLM2 2/9, gemma 0/9 and stating plainly that those are NOT comparable to the native-template matrix. DELIBERATE NON-CHANGES: no eval re-run, every published matrix score is untouched and still dated 2026-08-15/16; the substitute-template numbers are one run each and published as such; the `skill` execute path, `SkillParams` and `section_ref` are unchanged. Ships as v0.23.0 |
| T35 | Small items bundle | As built 2026-08-19, four items, offline except two keyless public fetches (Anthropic's pricing page, models.dev). P1: the baked Claude Sonnet 5 list price moves 3.0/15.0 -> 2.0/10.0. T24 baked the STANDARD rate on purpose and was correct to at knowledge-of-record 2026-08-07, because the $2/$10 launch pricing was introductory through 2026-08-31 with an increase to $3/$15 scheduled for 2026-09-01; Anthropic has since recorded $2/$10 as the standard price and cancelled that increase, so the baked pair was overstating every sonnet estimate by half. Verified on the pricing page directly, not from memory or from the bundled model-reference skill, whose cached table still carried the old state. Moved in lockstep across the wizard and --add goldens, the T24 per-model price assertion, the tests/cli.rs golden and the USAGE recipe and prose; the T24 not-all-equal pin still holds (inputs 10/1/5/2, outputs 50/5/25/10). Fable, haiku, opus, all four windows and the 0.1x/1.25x cache multipliers were re-checked and are unchanged, so only sonnet moved. One unplanned site: tests/agent.rs called 3.0/15.0 "Anthropic list rates for the sonnet tier" where the values are arbitrary, so the COMMENT was corrected rather than the numbers, keeping the asserted dollar strings byte-identical. P2: `scripts/metadata_drift.sh`, a report-only models.dev cross-check, added because nothing in the repo would have caught P1. It parses the baked 5-tuples out of the ANTHROPIC_PROFILES const at run time rather than keeping a second copy of the table, since a second copy is one more thing that goes stale; a parse miss is LOUD rather than a silent pass, so it can never report a clean run while checking nothing. python3, not jq, which is not installed here. Deliberately NOT wired into check.sh or release.sh, because a gate that reaches the network fails for reasons unrelated to what it gates; the RUNBOOK publish preflight calls it by hand and records the outcome, and a DRIFT does not block a ship, it gets a recorded decision. Live run prints 4x PASS and models.dev independently confirms the corrected sonnet 2/10; the drift arm, fed the pre-P1 values, reports "input baked 3 vs models.dev 2", so it demonstrably catches the defect that motivated it. P3: the D2 promise-then-stop nudge, dequeued from the T31 list above. Detection is three ANDed conditions, an EndTurn through the recovery arm with no other predicate matching, ZERO tool calls dispatched anywhere in the turn, and one of seven fixed future-intent phrases in the last 150 CHARACTERS of the message. The tail rule is the false-positive design the queue entry asked for: position is what separates "I will now summarize:" plus a summary, a finished answer, from the same phrase as the last thing written, a stalled turn. Prose-call recovery counts as a dispatch. Bounded at one extra request by NUDGE_LIMIT, no new config. P4: three RUNBOOK acceptance headings reading "(stage 1 only, NOT released)" reworded, since all three versions shipped and the headings asserted a state that stopped being true; the records themselves are untouched. Ships as v0.24.0 |
| T36 | Futile-call loop guard | As built 2026-08-23, one item dequeued from the T33 list below, offline throughout. The three existing loop guards are each narrow by construction and the archived rotation cleared all three: the doom-loop guard needs three IDENTICAL CONSECUTIVE calls, the T4 alternating-pair guard a strict `A,B,A,B,A,B` over a six-deep window, and T31's `ProseRepeatGuard` is the prose path only. P1: a dispatched structured call is FUTILE when its per-call `{name}:{input}` fingerprint AND a u64 hash of its result text both repeat an earlier call from the same turn. That is the lack-of-PROGRESS discriminator the queue entry demanded rather than a repetition count, and it is sound because temur is single-threaded: between two calls in one turn nothing can have changed the answer unless a call in between did, and then the result differs. The result hashed is what the MODEL sees, errors included, which is what makes the nineteen byte-identical range errors count. The counter is global to the turn, not consecutive, so one timestamp-varying call in a rotation cannot launder the rest. At 6 one self-healing notice rides the same user message as the results it is about, appended AFTER `all_errored` is decided so the T4 consecutive-failure cap still sees a batch of pure tool results; at 18 the turn stops, and the gap is deliberate headroom for the notice to work. The honest false positive is a model deliberately POLLING an external condition whose answer has not changed, which is why the first response is a notice and not a stop. Tests are loop-level MockProvider runs: the archived rotation shape notices at the 6th futile dispatch, keeps executing, and stops at the 18th; changed results, reread-after-edit and a ten-file edit rotation never count; identical failures do, isolated by a rotation whose only succeeding call changes its output every time. DELIBERATE NON-CHANGES: the doom-loop, alternating-pair and `ProseRepeatGuard` paths and their pinned wordings are byte-identical, and the guard covers structured dispatches only, the prose path keeping `ProseRepeatGuard`. P2: docs. Re-simulated against the archived history the notice would have landed at call 13 of 77 and the stop at call 28, ending the turn at an estimated 13% of the recorded 440,983 input tokens. Ships as v0.25.0 |
| T37 | Harness comparison | As run 2026-08-24, offline except installing the two competitor builds. Two deliverables in `docs/COMPARISON.md`: a footprint table and a local-model differential against RELEASED opencode 1.18.21 and codex-cli 0.149.0, all three driving one pinned llama.cpp server (`server-b10438`, digest recorded) on one x86-64 host at ctx 12288. Task prompts are byte-identical BY CONSTRUCTION: `scripts/harness_compare/tasks.sh` is generated from `weak_model_eval.sh` and a drift test in `check.sh` compares raw source bytes every gate run. HEADLINE, Qwen2.5-Coder-3B: the model emits ZERO native tool calls against this server build in any transcript under any harness, writing the call as prose instead in at least three improvised shapes. temur 8/9 + 9/9 carried WHOLLY by prose-call recovery (20 and 67 recoveries, zero native calls); codex 0/9 x2; opencode 0/9 x2. Verified at the wire with no harness involved (one request, temperature 0, one tool: `finish_reason: stop` and a fenced JSON blob, where Qwen3-4B returns a native `tool_calls`), and the template is NOT the cause, which is the opposite sign from T34's Phi-4-mini finding: this template does render `tools` and does demand `<tool_call>` tags, and the model emitted the template's own placeholder syntax literally instead of complying. Qwen3-4B, where tool calls are native: temur 9/9 x2, codex 8/9 x2 (t7 reproducible, the rm written inside a prose fence), opencode 7/9 + 6/9. Methodology: FRESH SERVER PER TASK after six kernel OOM kills under a per-cell server, adopted on observed effect (anon flat) and NOT on a mechanism, which was never instrumented; ctx 12288 not the planned 16384 for the same reason; 2 runs per cell with a third on spread >= 2 (never triggered); durations exclude server-ready. Moving to per-task servers changed ONLY opencode (4/9 single run -> 7/9 + 6/9) and left temur and codex byte-identical, so the change favoured the competitor, not the home team. Also measured: prompt sizes 2761 / 7413 / 7276 tokens server-side (fresh server per harness, because prefix cache undercounts); footprint 7.5 MB / 258 MB / 184 MB, peak harness RSS 37.1 / 119.2 / 842.9 MiB, warm cold start 0.03 / 0.23 / 2.48s; and off-box connections during a keyless local-only task, where temur opened none and both competitors opened some. Home-turf disclosure leads the page and every losing cell is published. Ships as v0.26.0 |
| T38 | Recovery-disabled control run | As run 2026-08-25, one item dequeued from the T37 queue above, offline throughout. Turns `docs/COMPARISON.md`'s "without it temur scores what the others score" from an inference into a measurement. P1: a FOURTH harness value, `temur-noprose`, in `scripts/harness_compare/`. Same 0.25.0 binary (sha256 `83bc69cf...`, verified against the published x86_64 asset before the runs), same adapter, same flags, same cwd, same config template with ONE added field, `"prose_tool_calls": false`. That is T19 P3's off switch: detection and the corrective nudge stay ON, only EXECUTION of a prose-written call is removed, so exactly one thing varies. Deliberately a harness NAME rather than an env knob, because the name is what indexes the archive cell dir, the ledger line, the already-scored skip guard and the closing summary, and an env knob would have left two identical-looking temur cells distinguishable only by an operator's memory. `adapter_temur_noprose` DELEGATES to `adapter_temur` rather than copying the command line, so the two cannot drift. `matrix.sh` gains `HARNESSES`, defaulting to the T37 three in the T37 order so an unset value reproduces the published matrices unchanged; both scripts validate against a closed list and fail BEFORE a cell dir exists, before the heavy-job lock is taken and before a model loads. `results.txt` gains a comment-prefixed header carrying `prose_tool_calls=` for every temur cell. `tasks.sh` and `tests/harness_compare_drift.sh` UNTOUCHED, so every published score stays comparable. P2: same conditions as T37 (server-b10438 digest `sha256:190813e8...`, ctx 12288, `--parallel 1 --jinja`, fresh server per task, heavy-job lock held, nothing else on the box). A preflight probe established FIRST that the pinned binary honours the field rather than ignoring it, since `Config` has no `deny_unknown_fields` and a clean parse would have proven nothing. RESULT, Qwen2.5-Coder-3B: 0/9 and 0/9, spread 0, against the same binary's 8/9 and 9/9 with recovery on and codex's and opencode's 0/9 x2. The inference was right. The control is also unusually clean: all EIGHTEEN tasks failed, prose-call recovery notices were ZERO by construction and asserted over the transcripts, and native structured dispatches were ZERO, so the model never once answered a nudge with a real tool call across 36 nudges (`NUDGE_LIMIT` 2, hit in every task). A nonzero control score would have been nudge-attributable rather than noise, execution being the only other path; there is none to attribute. Not a crash and not a timeout either: the control cells are the FASTEST in the table, 7.0 and 7.0 min against temur's own 9.4 and 16.1, because nudge-twice-and-stop is less work than executing. Qwen3-4B-Instruct-2507, the cheap half and the one that could have embarrassed the design: 9/9 and 9/9, zero nudges, zero recoveries, 17 and 15 native dispatches, 12.3 and 11.8 min against recovery-on's 13.0 and 12.4. Recovery is inert where native calls work. Spread 0 in all four cells, so the third-run rule never fired; no VOID cells. The analysis script's counters were validated by reproducing T37's published 20 and 67 recoveries and zero native calls on the archived recovery-ON cells before being trusted on the new ones. DELIBERATE NON-CHANGES: no product code, no test, no task wording; the published T37 tables are untouched and the control is added beside them, not merged into them. HONEST LIMIT, stated on the page: one model emits prose calls and ignores correction, which is two behaviours at once, so the control says nothing about a model that emits prose calls but DOES respond to a nudge. Ships as v0.27.0 |
| T39 | Terminal-Bench row | As run 2026-08-24 to 2026-08-26, shipped as v0.28.0; THIS ROW WAS ADDED A CYCLE LATE, in the T40 rider, having been missed at the time. temur's first appearance on an externally authored suite: Terminal-Bench 2 driven by Harbor 0.22.0, a 16-task subset pre-registered by a mechanical rule before any score was seen, the same local Qwen3-4B and pinned llama.cpp server as the other tables, two runs each for temur, codex and opencode. Bring-up was rootless podman with user-level docker/docker-compose clients pointed at the podman socket, since Harbor shells out to the docker CLI rather than the API socket. HEADLINE: pass rate does not separate the three harnesses at this model, 1/16 vs 1/16 vs 0-to-1/16, with exactly one task solved and all three solving it. Timeouts and wall clock DO separate them and no cause is attributed to the wall clock. The first temur matrix was INVALID and is disclosed on the page rather than quietly replaced: the adapter piped each instruction into `temur --plain`, which reads one line per turn, so temur received only the first line of the 12 multi-line instructions while the competitors received the whole thing; temur's 32 cells were re-run with one-shot `-p` and the competitors' were not, because their delivery was never broken. A product finding derived from the invalid cells was withdrawn and the ROADMAP entry built on it removed with a dated correction. Two product findings survived and are dequeued by T40 below (F4 no unattended compaction, F5 a killed run loses its session); F1 (addressing an absent user) remains queued. Evidence: `~/temur-eval-archive/t39-terminal-bench/`. |
| T40 | Unattended runs | As built 2026-08-27, two product findings dequeued from the T39 queue below, both seen on both boxes, offline throughout. P1: when the context advisory would fire and `auto_compact` is on, the session compacts ITSELF at the next safe point and continues the turn instead of printing advice nobody will read. The safe point is the top of the next round-trip, after tool results are appended and before the request is built, never the advisory site, where the last response still has unanswered `tool_use` blocks. Auto-compaction gets its OWN history rule and `/compact` is untouched: `/compact`'s verbatim tail reaches back to the last plain user message, which mid-turn IS the task prompt, so the whole turn would be tail and the compaction would free nothing, which is exactly how the T39 cells died. The auto rule cuts inside the turn instead: task prompt verbatim, summary pair, last 2 round-trips, cut at a `tool_use`/`tool_result` boundary. Five invariants pinned as unit tests (prompt leads byte-identical; every retained `tool_use` has its `tool_result`; the cut is at a pair boundary; exactly K round-trips retained however long the turn; a turn with fewer than K+1 round-trips is not compacted), plus a fail-closed decline for any history that is not whole pairs. Bounded at 3 per turn, after which the existing advisory prints byte-identical to today and the request goes out as it would have, which may 400 and that is the honest outcome. Enablement is `auto_compact: Option<bool>` on the BASE config only, no profile-level field, resolved against the invocation mode: explicit wins, else one-shot `-p` true and plain REPL/TUI false. P2: the session file is written after every round-trip rather than once at turn end, so a `SIGKILL` costs at most the request in flight; the write moved into the session as an optional persist target, `save_after_turn` stays as the final save with turn-end behaviour byte-identical (pinned), and the save-failure notice stays once per PROCESS by sharing one latch instead of two. `--mock` still writes nothing. P3: `release.sh` now emits the exact `gh release create` invocation with `--title` taken from the annotated tag message, so the release title and the tag message cannot drift (v0.21 through v0.28 shipped bare titles; old releases are NOT retitled), plus docs. Tests 723 -> 744, full `check.sh` green at every phase. RIDER (same cycle, 2026-08-27): the resume seam now compacts too, using the EXISTING `/compact` rule since before a turn begins the whole pre-turn history is exactly what should fold and invariant (e) does not apply, which closes a gap where `--continue -p` over an already-full seed latched the advisory at the seam and could never auto-compact; foldability moved to the advisory site so a turn never announces a compaction it then does not perform; and the auto path got its own outcome line in round-trips and bytes, because the reused `/compact` message counts could read "7 message(s) summarized into 7" for a real fold. `/compact`'s own outcome line is unchanged. Tests 744 -> 753. |
| T40.1 | Post-ship review fixes (v0.29.1) | PATCH on T40, not a new milestone: no new capability, only defects in what T40 shipped. As built 2026-08-29 from a code review of the shipped v0.29.0 range (`5f18325..b961a83`), seven findings, all seven verified against the tree by the planning session before any was acted on, three commits. F1 (HIGH): `turn()` captured `turn_start` once and never moved it, but a fold rewrites history as `[prompt, summary, resume, tail]`, so the SECOND fold of one turn was handed a stale index, kept a `tool_result` as "the prompt", dropped the real prompt (invariant (a)) and opened history with an orphan `tool_result` whose `tool_use` it had just folded away: a hard 400 on both wires, written to the session file by the P2 saves. Only reachable when the turn does not start at the top of history, which is every resumed or later REPL turn including `--continue -p`, the invocation T39 F4 motivated auto-compaction for. Nothing caught it because the entire auto-compaction corpus compacts from an EMPTY history, where the stale index is 0 and correct by accident: fail-open family entry six, in the RUNBOOK beside T37's three and T39's two. F2: a crossing with too few round-trips to fold said nothing at all, and in one-shot `-p`, the mode where auto-compaction defaults ON, there is no later turn to carry it, so the run printed no warning and then died on the next request; the ordinary advisory now prints at turn end, latch deliberately unconsumed, which is what `docs/USAGE.md` described throughout. F3: the session-file trim notice had no latch while the save-failure notice beside it did, and P2's two writes per round-trip turned one line per turn into up to a hundred. F5: `release.sh` read the release title from `%(contents:subject)`, which for a LIGHTWEIGHT tag reports the COMMIT subject, so the guard against a non-annotated tag passed the exact mistake it exists to catch; now gated on `%(objecttype)` as well. F6: the stray zero-byte `oom` at the repo root is removed and identified as a rider-2 builder-session artifact, closing the tracked copy of the v0.26.0 residual but NOT the three 2026-08-24 sightings that predate it. F7: `Session::context_advisory()` deleted, no production callers left after the T40 rider. F4 (two full serializations and atomic writes per round-trip in `persist_now`) is NOT fixed: it is a design trade against the crash-safety that is the point of T40 P2, and it is queued above under "Queued from v0.29.1" with the measurement it needs first. Tests 753 -> 756, full `check.sh` green first try at every commit. |
| T41 | Prompt floor | As built 2026-08-30, dequeuing **F6 (T40)**, whose two candidates were "select the compact profile automatically below a `context_window` threshold" and "report the floor in `doctor`". Both built; the operator approved the default flip on 2026-08-30. P1: `prompt_profile` accepts `"auto"` | `"full"` | `"compact"` and an ABSENT field now means `"auto"`, which is the new default. Auto resolves to compact when a `context_window` is configured and strictly below 16384, to full otherwise (an unknown window included, since guessing smaller would trim descriptions on a model that never needed them); an explicit value is never second-guessed at any window. One pure function over `Option<u64>` is the only path from a window to a profile, and `PromptProfileSpec` splits spelling validation from window resolution so a typo in the GLOBAL field is still a startup error while each selection resolves against its OWN window. `ResolvedProfile` gained `prompt_profile_source` (`Explicit`/`Auto`), read only by reporting: one startup line when auto picks compact, the same line from the same function on a `/model` switch that lands there, and `/status` becoming `compact (auto)` / `full (auto)` / `compact` / `full`. This DELIBERATELY reverses the explicit-only contract pinned in three places through v0.29.1 (the `PromptProfile` doc comment, the config resolution test, `docs/OFFLINE.md`), all three changed in place. RIPPLE, stated in the commit and the docs: any config with a window below 16384 and no explicit `prompt_profile` gets compact descriptions from this release on, `temur init`'s local template included. P2: `doctor` reports the prompt floor for the active selection, an offline estimate always (system-prompt plus definition bytes over 4) and a MEASURED figure under the tools-drop gate (one extra one-token POST carrying the real system prompt and the real definitions); PASS below 40% of the window, WARN at or above, never FAIL, and a WARN on an already-compact selection points at `context_window` instead. `probe_prompt_tokens`/`tools_drop_probe_body` gained `system: Option<&str>`, with `None` byte-identical to the pre-T41 body so the T34 tools-drop baseline cannot move. `DEFAULT_SYSTEM`/`DEFAULT_SYSTEM_COMPACT` moved from `main.rs` into `src/prompt.rs` byte-identically. The estimate NOTE deliberately does NOT quote the planned "under-reads by 15-30%": measured against this checkout the estimator reads 7,240 where F6 counted 6,991, 4% HIGH, so no percentage is defensible from one calibration point. Tests 756 -> 779, full `check.sh` green first try at every phase. Ships as v0.30.0. **Three defects in it were fixed as v0.30.1; the threshold shipped here (16384) is NOT the one that stands, see T41.1.** |
| T41.1 | Post-ship review fixes (v0.30.1) | PATCH on T41, not a new milestone: no new capability, only defects in what T41 shipped hours earlier. As built 2026-08-30 from a code review of the shipped v0.30.0 range (`049085e..12cd7ec`), three findings, all three verified against the tree by the planning session before any was acted on, one commit each. R1 (MEDIUM, the reason this is a release and not a note): `PROMPT_AUTO_COMPACT_BELOW` and `doctor`'s `PROMPT_FLOOR_WARN_PERCENT` disagreed BY CONSTRUCTION. 16384 * 40% is 6,554, below the 6,991 tokens the full profile actually costs, so every window in [16384, 17478) measured (and [16384, 18100) estimated) got `full` from the auto rule and then a WARN against that same selection from `doctor` in the same run, telling the user to set `prompt_profile` to `"compact"` and undo the choice temur had just made. 16384 is exactly what `temur init` writes from a 16k llama.cpp `/props` allocation, so a fresh `init` followed by `doctor` produced the contradiction on the wizard's own output. Fixed by DERIVING the threshold from the warning line instead of picking it for roundness: 20480, the smallest round window at which the full floor sits under 40% (34% measured, 35% estimated), with the tie pinned by a test that runs the shipped estimator over the real full-profile prompt and the real registry at exactly the threshold, so changing either constant or growing the prompts fails loudly. The WARN itself was deliberately NOT softened for an `Auto` choice: a 44% floor is a 44% floor, and a report that excuses a number because the tool picked it is worth nothing. RIPPLE, same shape as T41's own: a config with a window from 16384 to 20479 and no `prompt_profile` moves from the full descriptions to the compact ones. R2 (LOW/MEDIUM): the floor probe carries the system prompt AND every definition, which makes it the largest prefill a doctor run asks for, and it runs FIRST, unannounced, where the tools-drop check right after it prints a NOTE and flushes before its own pair for exactly this reason; v0.30.0 could go silent for up to 300s with nothing said, and took the worst case from two large prefills to three. It now announces itself in the same style, on the networked path only, and the definitions the two probes share are built once per run instead of twice. R3 (LOW): `hop_switch` computed the auto-compact notice and emitted it on both success paths but dropped it in the arm where `raw_override` failed, although that arm's own comment says the activation stands and an activation swaps the prompt profile: a user who hopped onto an auto-compact profile and whose model override then failed was left on the compact prompts with only a failure notice. VERIFIED LIVE, not just reasoned: `~/temur-eval-archive/t41-live/REPORT.md` records the fixed binary against llama.cpp `server-b10438` (Qwen3-4B-Instruct-2507, ctx 12288) picking compact at 16384 and PASSing at 16%, while the installed 0.30.0 on the same config WARNs at 42% and advises compact. Tests 779 -> 783, full `check.sh` green first try at every commit. |
| T42 | Context overflow recovery | As built 2026-08-31, dequeuing all three **desktop experiment 5** items plus the **T41** startup-probe item and the **v0.30.1** `max_tokens` item, offline throughout. Exp-5 decomposed temur's five remaining context-exhausted cells out of 32 into three mechanisms, and the answer is one reactive fix, two predictive ones, and two diagnostics. P1, the headline: a send that fails with a parsed wire kind of exactly `exceed_context_size_error` is recovered ONCE and retried ONCE, where before it was terminal. Arm (a) is the ordinary fold when `auto_compact` is on and the turn already holds enough round-trips, with the F1 `turn_start` reset; arm (b) is the class a fold cannot reach at all, halving the LARGEST `tool_result` in place with the T19 head+tail elision shape and a marker addressed to the model. Arm (b) is what the `gcode-to-text` cells needed: both died on round-trip ONE, where nothing is foldable and where a fold would have kept the oversized result in the verbatim tail regardless. Only `ToolResult` blocks are candidates, so the prompt-verbatim invariant holds; below a 1,024-char floor recovery declines. At most one recovery per send, counted against the same `MAX_AUTO_COMPACTIONS_PER_TURN` bound, no re-examination of the retry, and every failure inside the path propagates the ORIGINAL provider error (the T40 fail-open discipline). Both trigger lines are STABLE, greppable markers, because desktop experiments read them. P2: the crossing check runs a second time immediately before each request, folding a chars/4 estimate of the trailing non-assistant messages into `last_context_used`, which is one round-trip stale by nature; same thresholds, same latch, so one crossing still gets one speaker. Documented honestly rather than tuned: chars/4 is an average, the gcode cells measured ~1.2 chars/token, this alone would still have missed them, and P1 is the backstop. P3: the AUTO path's summary call now carries `history[..tail_start]` instead of everything, since the tail survives verbatim in the result; `adaptive-rejection-sampler` r2 is the evidence, its summarize request rejected at 13,518 tokens against a 12,288 window. `/compact` and the resume seam keep full history, because both are fail-closed and discard everything but the summary. P4: startup runs the T22 keyless `/props` GET for an `openai-compat` selection with no key file and no configured window, outside `--mock`, and feeds the answer to the resolved selection, `session_cfg.context_window`, the T19 registry cap, and the `"auto"` prompt rule; in-memory only, a configured window is never probed over, a miss is silent. P5: a `doctor` WARN when `max_tokens > context_window`, naming both numbers and the fix, never a FAIL. DELIBERATE NON-CHANGES, all recorded: `/compact` untouched (fail-closed, discards everything but the summary); the T19 cap formula untouched (its ~4 chars/token assumption is what dense content defeats, and changing it moves every model's tool output, so it is its own milestone if ever); and NO provider-general error taxonomy, since P1 classifies llama.cpp's kind alone and a wrong classification would resend a request rejected for some other reason. Ships as v0.31.0 |

### Queued from laptop dogfooding (2026-09-01)

Two unrelated threads. The first is three operator-reported symptoms
from one interaction, all of them downstream of a single fact: the TUI
has no concept of a paste. Traced in code, NOT yet reproduced under
test. The second is output formatting, raised as explicitly low
priority and explicitly incomplete.

- **A long paste makes the TUI deaf, Ctrl+C included.** `render_loop`
  (`src/ui/tui/mod.rs:344`) does one full `terminal.draw()` and then
  handles exactly ONE event per iteration, and `CrosstermEvents::next`
  (`mod.rs:75`) is a single `poll` + `read`. Bracketed paste is never
  enabled anywhere in the tree (no `EnableBracketedPaste`, no
  `Event::Paste` arm), so a paste arrives as N discrete
  `Event::Key(Char)` events: an N-character paste costs N full-frame
  redraws, and any Ctrl+C the operator types sits BEHIND every pasted
  character in the tty buffer. Ctrl+C is not ignored, it is unreachable
  until the backlog drains, and there is no bypass, because TUI raw
  mode disables ISIG so Ctrl+C never becomes a SIGINT (`main.rs:610`,
  `signal.rs`) and the event queue is the only path in. Candidates,
  cheapest first: (a) drain every queued event per iteration and draw
  ONCE, which bounds redraws by frame rate rather than by paste length;
  (b) scan the drained batch for Ctrl+C/Esc and honor it immediately,
  discarding the rest, which makes interruption O(1) in paste size;
  (c) the real fix, enable bracketed paste and handle
  `Event::Paste(String)` as one event. (a) and (b) are contained and
  independently useful. (c) is NOT a rider: it changes what a pasted
  newline means, so multi-line input needs its own decision first.

- **A pasted block is submitted as a series of turns the operator
  cannot stop.** A pasted newline is an Enter (`app.rs:427`), and the
  Enter arm is `if !self.busy` — so newlines arriving DURING a turn are
  dropped, but the moment that turn ends the still-draining paste
  submits the next chunk, and the next. The operator's report is
  literally "breaking it down and automatically sending each chunk
  without me being able to stop it", which is one turn per pasted line,
  serially, with the interrupt keys stuck behind the remaining
  characters. The busy-Enter drop bounds concurrency, not total
  submissions, and is not the guard it looks like here. Fixed by (c)
  above, since a paste event carries no Enter semantics at all;
  wanted regardless is a decision on what a multi-line paste SHOULD do
  (one prompt with embedded newlines is the obvious answer and matches
  what the operator expected).

- **Double Ctrl+C should force-quit unconditionally.**
  `app.rs:386` disarms the force-quit latch on any key other than a
  second Ctrl+C, so characters landing between two presses defeat the
  double-press escape hatch — during a paste, that is guaranteed. The
  disarm has a real purpose (a stale first Ctrl+C should not quit the
  session minutes later) so the fix is a TIME window, both presses
  inside ~2s force-quit regardless of what arrived between them,
  not deleting the disarm. Note this is necessary but NOT sufficient on
  its own: an unconditional latch still cannot fire if neither press
  has been read yet, so it only becomes the promised escape hatch
  together with (a)/(b) above.

- **Esc-pauses-the-turn needs a regression test, not a redesign.** The
  operator was unsure whether Esc worked during the paste. Reading the
  code, it does: `app.rs:463` sets `interrupting` and returns
  `Action::Interrupt` whenever `busy`. Their own guess for what they
  saw is almost certainly right — Esc cancelled the running turn,
  `busy` went false, and the still-draining paste immediately submitted
  the next chunk, which reads as "Esc did nothing". No fix is implied
  beyond the items above, but the interaction (Esc, then queued input
  behind it) has no test and should get one, since the failure mode is
  invisible: it looks like a broken interrupt rather than a new turn.

- **LaTeX math renders as its own source.** OPERATOR-FLAGGED LOW
  PRIORITY ("nitpicky and not necessarily that important"), and the
  operator states there is more formatting feedback not yet recalled,
  so treat this bullet as the head of a list rather than the list.
  Reported shape: a solved integral came back as
  `Solve $ \int 2x \cos(x^2)\,dx $` instead of anything resembling
  `∫ 2x·cos(x²) dx`. `src/ui/tui/markdown.rs` is a real renderer (658
  lines, pulldown-cmark, CommonMark + strikethrough) but math is not
  CommonMark: `$…$`, `$$…$$`, `\(…\)` and `\[…\]` are not tagged by the
  parser, so every delimiter, backslash command and `^` passes through
  as literal text. The operator's own paste shows the degradation
  compounding — the thin-space `\,` lost its backslash in transit and
  became a stray comma before `dx`.
  Scope judgment, NOT built: real LaTeX layout is out of the question
  in a terminal, because fractions, radicals and limits on an integral
  need two-dimensional placement that the line-based renderer has no
  concept of. What is achievable is a SUBSTITUTION pass — recognise the
  delimiters, drop them, map the common commands to Unicode (`\int` ∫,
  `\alpha` α, `\cdot` ·, `\times` ×, `\leq` ≤, `\to` →, `\sum` ∑,
  `\sqrt` √) and lift simple `^`/`_` runs into superscript/subscript
  codepoints — which would carry the operator's example completely
  while degrading honestly on anything structural. Known limits to
  decide up front: Unicode super/subscripts are an INCOMPLETE alphabet,
  so the mapping must fall back to literal `x^(n+1)` rather than emit
  half a run; and the monochrome contract stated at `view.rs:7` and in
  the `markdown.rs` header (no themes, no backgrounds, no syntax
  highlighting) means this buys no colour and must stay a text
  transform.
  CHEAPER ALTERNATIVE worth pricing first: the model emits LaTeX
  because it was trained to, so one sentence in the prompt profiles
  asking for Unicode math instead of LaTeX might fix the reported case
  with no renderer work at all. It is not free — it costs prompt tokens
  on every turn, and on the operator's own laptop config the floor is
  already 2,715 tokens against an 8192 window (33%, measured
  2026-08-31), so a nudge competes directly with context. It is also
  only advisory, where a renderer pass is deterministic. Decide between
  them before building either; they are not complementary.

### Queued from desktop experiment 5 (2026-08-31)

The auto-compaction differential (`docs/COMPARISON.md`, "Same rig,
auto-compaction on", archive `~/temur-eval-archive/desktop-exp5/`) left
five context-exhausted cells out of 32, and the three mechanisms behind
them are separable. Each candidate below was a decision, not an obvious
yes.

**ALL THREE DEQUEUED by T42 (2026-08-31).** Kept here with what was
actually built against each, because the split between them turned out
to matter more than the list did: the estimator item alone could NOT
have saved the class it describes, and the recovery item is what does.

- **`context_crossing()` cannot see additions made since the last
  response.** It reads the usage the previous response reported, so a
  single large `tool_result` appended on the client side can carry the
  next request past the window with no crossing ever detected. This is
  **3 of the 5** exp-5 CTX deaths, and both `gcode-to-text` cells died
  on round-trip ONE: one capped read of `text.gcode` delivered 12,433
  characters, the request measured 13,402 tokens against a 12288-token
  window, and the value the check had to work from was the first
  response's usage, reconstructed at about 2.9k. Candidate, NOT built:
  fold a chars-based estimate of the client-side additions appended
  since the last response into the crossing check, so the check
  describes the request about to be sent rather than the one already
  answered. The related calibration fact belongs with it: T19's cap
  budgets its bytes at about 4 characters per token, and dense G-code
  measured about 1.2, so one capped read can be about 85% of a 12288
  window on its own.
  BUILT as T42 P2, with a correction the candidate did not make: a
  chars/4 estimate would still have MISSED these three cells, because
  the content that filled the window is exactly the content the divisor
  is wrong about. P2 catches the ordinary large result in time to fold
  it cheaply; what catches this class is P1, which acts after the
  server rejects the request rather than trying to predict it. The
  divisor is deliberately left at 4 rather than tuned to the gcode
  sample, since tuning to the worst case makes every ordinary turn
  compact early. The T19 cap formula itself is a DELIBERATE
  NON-CHANGE: it is now backstopped by P1, and moving it would move
  every model's tool output, which is its own milestone if ever.

- **A context-size 400 is terminal, and nothing tries to recover from
  it.** The provider error propagates out of the turn loop mid-turn;
  there is no reactive-compaction path in v0.29.1 at all. Every cell in
  the blind-spot class above died this way rather than on a detected
  crossing. Candidate, NOT built: one compact-and-retry when the error
  is specifically a context-size rejection and `auto_compact` is on.
  This is deliberately a **separate** decision from the estimator fix
  above: a better estimate would prevent most of these, a retry would
  survive the ones it still misses, and either can ship without the
  other.
  BUILT as T42 P1, and it is the headline of that milestone rather than
  a companion to the estimator fix. Two arms, not one: the
  compact-and-retry the candidate names, plus a halve-the-largest-tool-
  result arm for the case a fold cannot reach, which is the case both
  `gcode-to-text` cells actually died in. Classification is an EXACT
  match on the parsed wire kind `exceed_context_size_error` and nothing
  else. A provider-general error taxonomy is a DELIBERATE NON-CHANGE
  and stays queued below: pattern-matching other providers' overload
  wordings would resend requests rejected for entirely other reasons.

- **The summarize call is subject to the window it is trying to
  relieve.** When staleness means the crossing is first SEEN late, the
  fold itself does not fit. `adaptive-rejection-sampler` run 2 saw the
  crossing at ~11,571 of 12,288 tokens, 94% of the window; the
  summarize request was rejected at 13,518 tokens, auto-compaction
  failed open as designed, and the next request died anyway. That is
  the 1 of 5 in this class, and the 1 failure in the 12 / 11 / 1
  compaction totals. Candidate, NOT built: bound the summarize request,
  either by dropping the tail it is going to keep verbatim anyway or by
  hard-trimming what it sends, so the relief call cannot be the thing
  that overflows.
  BUILT as T42 P3, by the first of the two routes: the AUTO path
  summarizes `history[..tail_start]` only. No hard trim, which would
  have needed a policy for what to cut and would have made the summary
  a lie about what it read. `/compact` and the resume seam keep full
  history, since both discard everything but the summary.

### Queued from v0.30.1 (2026-08-30)

- **`max_tokens` larger than `context_window` makes the context
  advisory fire from the first response.** `context_crossing`'s second
  arm is `window - used < max_tokens`, which is true by construction
  whenever `max_tokens` exceeds the whole window: the advisory then
  fires on the first response of every session, at any usage, and says
  `/compact` about a window that is 33% used. Seen live on
  2026-08-30 in the v0.30.1 smoke (step 3e) with the default
  `max_tokens` 32000 against a 12288-token local window, which is a
  plausible hand-written local config: the anthropic default rides
  along because the openai-compat profile never named its own. Not a
  T41 defect and not fixed there; the arm is correct for its purpose,
  which is warning that the next response cannot fit. Candidate, NOT
  built: a `doctor` WARN when `max_tokens > context_window` for a
  selection, naming both numbers, since that configuration also means
  every request asks for more completion than the server can hold.
  Whether the advisory arm itself should be quieted in that case is a
  separate question and wants the WARN first.
  BUILT as T42 P5, exactly as scoped: WARN, never FAIL, naming both
  numbers, both consequences and the fix. The advisory arm itself is
  untouched, so that separate question stays open and now has the WARN
  it wanted first.

### Queued from T41 (2026-08-30)

- **Nothing probes the context window at REPL startup.** Auto-selection
  keys off the CONFIGURED `context_window` and nothing else, so a local
  server whose allocation was never written into the config resolves to
  `full` regardless of what it actually serves, which is the exact
  configuration most likely to need `compact`. `temur init` writes the
  detected value when the server is up at wizard time and `doctor`
  compares a configured value against `/props`, but a REPL start does
  neither. Candidate, NOT built: probe `/props` once at REPL startup on
  a keyless openai-compat selection and feed the answer to the same
  auto rule, which would make auto work on unconfigured local servers.
  The cost is a startup request on a path that today makes none, and
  the amendment contract on keyless requests (`provider/mod.rs`) would
  have to be extended to cover it, so this is a decision and not an
  obvious yes.
  BUILT as T42 P4. The contract needed no widening in the end: the
  probe is the existing `probe_props_context`, which takes a base URL
  and nothing else and therefore cannot attach auth by construction, so
  startup is a third caller of a call already covered rather than a new
  kind of request. The gate is narrower than the candidate's: keyless
  `openai-compat` AND no configured window AND not `--mock`, so the
  startup request is made only on runs that would otherwise have had no
  window at all. In-memory only, and doctor still recommends the
  explicit config line.

- **The compact-vs-full effect has never been measured on tasks.**
  T41's threshold is reasoned from F6's floor arithmetic (6,991 tokens
  is 57% of a 12288 window, 42% of 16384, 34% of 20480, 21% of 32768)
  and from the context-exhausted cells in desktop experiments 3 and 4.
  Nothing demonstrates that compact-below-threshold improves a score, or
  that 20480 (16384 through v0.30.0) is the right line rather than 12288
  or 32768. What v0.30.1 fixed is only self-consistency with doctor's
  own warning line, which is still arithmetic and still not evidence
  about finished tasks. Candidate, NOT
  built: re-measure the compact-vs-full effect on the Terminal-Bench
  subset from the desktop, after experiment 5, changing only the prompt
  profile. Until that runs, T41's default flip rests on the floor
  arithmetic alone, which is a claim about what temur SENDS and not
  about what it FINISHES. Desktop experiment 5 (2026-08-31) did NOT
  close this: it ran `prompt_profile` `"compact"` EXPLICITLY in both
  arms, varying the binary and not the profile, so this measurement
  remains open.

### Queued from T37 (2026-08-24)

- **A neutral third-party task suite (Terminal-Bench adapter).** Tier 2.
  `docs/COMPARISON.md` is run on tasks from temur's own eval suite and
  says so in its first paragraph, but a disclosure is not a fix. Until
  the same three harnesses are compared on a suite nobody in this
  repository wrote, the differential is evidence about temur's chosen
  tasks and not a capability ranking. The adapter is the work: map
  Terminal-Bench tasks onto the existing three-adapter driver, which
  already handles per-task fresh servers, model pinning and VOID
  quarantine, so the harness side is mostly done.

- ~~**The prose-call recovery path deserves its own measurement.**~~
  BUILT IN T38. T37 showed temur's entire score on Qwen2.5-Coder-3B
  resting on it, which no test asserts directly. A recovery-disabled run
  against the same model would turn "the harness is the difference" from
  an inference into a measured control, and it is cheap: one env knob
  and one cell.

  CORRECTION (2026-08-25). Two things in that last clause were wrong,
  and they are recorded because the entry was written by the milestone
  that had just finished measuring the feature. First, no new knob was
  needed in the PRODUCT at all: `prose_tool_calls` has existed since
  T19 P3, is container-default true, and `false` restores detect+nudge
  byte-identically; it is documented in `docs/USAGE.md`. What was
  missing was a knob in the comparison DRIVER, which had no way to set
  it. Second, "one cell" undercounted: the control needs the native-call
  half too, because a control that only runs where the feature carries
  the score cannot show that the feature is inert where it does not.
  Four cells, two models, two runs each.

### Queued from T31 (2026-08-14)

Found during operator dogfood day 1 and the Coder-1.5B re-run. Five of
the seven findings were built in T31; the two below were deferred with
reasons, not deprioritized silently. The first of the two is now BUILT
IN T35 and is kept here, struck through, so the queue records what
happened to it rather than losing it.

- ~~**The promise-without-a-call shape.**~~ BUILT IN T35. A turn ends
  with the model announcing what it is about to do and making zero tool
  calls, so the work never happens and nothing in the loop notices,
  because every existing guard keys off a call-shaped artifact.
  Detection here is fuzzy by nature ("I'll read the file now" is
  indistinguishable from a plan the user asked for), so it needed
  false-positive design before any code: what distinguishes an
  announcement from a summary, and what a wrong nudge costs a model
  that was answering correctly. That design is what T35 supplies: the
  distinguisher is POSITION (only the message tail is read, so a
  promise phrase followed by the actual work does not fire) ANDed with
  a whole-turn condition (zero tool calls dispatched anywhere), and the
  cost of a wrong nudge is bounded at one extra request by NUDGE_LIMIT.

- **`serve.sh` has no `SERVER_ARGS` knob.** Every llama-server flag the
  script does not already model (sampling, template overrides, slot
  count) requires editing the script. A pass-through would make the
  eval harness able to test a chat template with tool support, which is
  the remedy the tools-drop finding above points at and currently
  cannot be exercised. Deferred: it widens what `serve.sh` promises,
  and the argument-quoting rules in POSIX sh deserve their own design
  pass rather than a rider.

  PARTLY RESOLVED 2026-08-18 (T34): the template case, which is the one
  this item was actually written for, now has its own named knob,
  `CHAT_TEMPLATE_FILE`, in both `serve.sh` and `weak_model_eval.sh`.
  The general `SERVER_ARGS` pass-through stays OPEN, and my judgment is
  that it should stay open rather than be built next: every argument
  for deferring it holds unchanged, and the case for it just got
  weaker, not stronger. A single typed knob validates its input (the
  file must exist and be readable), can be compared against a running
  server, and can carry a warning that names the specific hazard it
  introduces. An opaque argument string does none of those, and the
  hazard it would have covered here was not "a flag is missing" but "a
  substitute template makes a model hallucinate confidently" - which
  only a knob that knows what it is can say. Build the next one the
  same way, one named knob per real need, until a use appears that
  genuinely cannot be named.

Also recorded: the Coder-1.5B re-run scored 5/9 and neither confirms
nor refutes T30's expectation that the preamble-then-fence fix would
raise it. The re-run was a dogfood pass, not a matrix pass: one model,
one day, and the tasks that moved are not the tasks the fix targets.
RESOLVED 2026-08-15: T32 ran both the formal bridge check and the
matrix pass, and the expectation is closed as NOT CONFIRMED and not
testable by this instrument; see the disposition on the T30 row.

### Queued from T13 acceptance (2026-08-05)

Found during hosted verification. T27 (2026-08-12) cleared this list
except for the entry below, which survives because it could not be
reproduced rather than because it was deprioritized.

- **`/models` listing renders two ids on one line.** Observed once, in
  the operator's terminal, during T13 acceptance. T27 could not
  reproduce it: the TUI transcript was rendered at every width from 4
  to 200 columns with ids built so that even a fragment of one landing
  beside another would be caught, and no row ever mixed content from
  two ids; the plain REPL prints one line per id and cannot merge
  either. The probe is kept as a regression pin
  (`models_listing_never_puts_two_ids_on_one_row`). Deliberately not
  fixed blind. Reopening this needs specifics from a live session:
  terminal emulator, exact width, and the id list that did it.

**Session cost visibility** for keyed users is built and now closed on
both halves: T24 gave `/status` the estimate, and T26 gave it a voice
of its own every $5 crossed, which was the escalated arm that dogfood
item (predating this list) had left open. The next milestone is the
planning session's call; the standing gates are T13's hosted
verification (parked on keys) and the public flip.

### Queued from T29 (2026-08-12)

Found while measuring the model matrix. NOTHING was fixed in T29 by
design: the milestone recorded findings so a later one can act on them
with the numbers already in hand. This list is now CLEARED.

T30 (2026-08-12) took four of them: 1 (the silent preamble-then-fence
shape now nudges), 8 (the base-directory line is conditional), 6 (a
write over non-empty content reports what it replaced), and 7 (the
baked default and the "(primary)" label moved to Qwen3-4B-Instruct-
2507).

T32 (2026-08-15) took the remaining five together, which is how they
were queued: items 2 through 5 all changed what a published number
means, so they had to land in the same pass that re-ran the matrix and
restated the scores. Item 2 reworded eval tasks 2 and 9 so no literal
placeholder appears; item 3 promoted `max_tokens` to `EVAL_MAX_TOKENS`
at a 3072 default; item 4 added `EVAL_RUNS` and the published table now
carries two scores per model, which turned the noise claim from an
anecdote into the table's headline caveat; item 5 archives every failed
task's work and state dirs before teardown. Item 9 needed a live model
and got one: the archived session JSON shows Llama-3.2-3B emitting a
structurally perfect `edit` call whose `replaceAll` is the string
`"false"` rather than the boolean, which the tool rejects and the model
resends verbatim until the repeat guard stops it. That capture opened
the tolerant-parsing item queued from T32, which T33 built.

### Queued from T32 (2026-08-15)

Found while re-running the matrix, both left alone deliberately at the
time: fixing either one mid-pass would have made the rows measured
before the change incomparable to the rows measured after it. This list
is now CLEARED. T33 (2026-08-16) took both: the stringified-scalar item
became field-level tolerant coercion at the argument boundary, and
`EVAL_TASK_TIMEOUT` now binds through podman's own `--timeout`.

### Queued from T33 (2026-08-16)

Both items were measured from the archive on 2026-08-17 rather than
acted on blind, and that measurement moved the effort from the first to
the second. The second is now BUILT IN T36 and is kept here, struck
through, so the queue records what happened to it rather than losing
it.

- **A stringified `"0"` offset now fails a RANGE check instead of a
  type check.** T33's coercion did what it claimed and the primary
  metric is clean (sixteen stringified-scalar rejections to zero), but
  the re-measure surfaced what was hiding behind the type error: every
  `offset` Llama-3.2-3B sent was a string, and nineteen of them were
  `"0"`. Those now parse successfully and then hit `read`'s own
  `offset must be greater than or equal to 1`, because offsets are
  1-indexed. The model is wrong about the VALUE, which is not a
  tolerance question, and temur's error already names the rule and asks
  for a rewrite. So this is queued as a MEASUREMENT rather than a
  defect: whether a model that sends `"0"` recovers from a range
  message when it did not recover from a type message is unknown, and
  the archived sessions can answer it. Do not "fix" it by silently
  reading `0` as `1`: an off-by-one that reads a different line than
  the one asked for is worse than a loud refusal, and the T33 no-
  corruption rule is the same principle one level up.

  MEASURED 2026-08-17 from the archive, and it shrinks the item. The
  nineteen events are not a pattern across the pass: they are ONE task
  in ONE run, `task8.run1`. Post-rejection the model corrected `offset`
  to `1` in about 11 of 19 and resent `"0"` unchanged in 8; the T32
  type-rejection comparison over the same tool is 3 of 5 recovered, 2
  abandoned. So the model partially recovers from BOTH messages and the
  sample is far too small to rank one against the other. What the task
  actually died of was not this at all but the loop queued directly
  below, which is where the effort belongs.

- ~~**A rotating repertoire of tool calls passes every loop guard.**~~
  BUILT IN T36. The guards were each deliberately narrow: the doom-loop guard needs
  three IDENTICAL CONSECUTIVE calls, the T4 alternating-pair guard
  needs a strict `A,B,A,B,A,B` over a six-deep fingerprint window, and
  T31's `ProseRepeatGuard` compares against the LAST dispatched call.
  A model that rotates through roughly six distinct calls satisfies
  none of those shapes and runs unbounded. Evidence, and the reason
  this is filed rather than theorised:
  `~/temur-eval-archive/llama32-coercion-2026-08-16` `task8.run1`, 77
  tool calls in one task with ZERO identical-consecutive pairs, cycling
  read offset `"0"` x19, `gunzip -c` x15, read offset `"1"` x14, `zcat`
  x9 plus `cat`, `gunzip` and `write`, ending at the context window on
  440,983 input tokens with temur's overflow-reworded truncation
  notice. This is the H1 cost/context-burn class rather than a
  correctness bug: like T31's H1 the task fails either way, and what
  the gap costs is tokens and wall clock. Note the tension before
  building anything: widening a loop guard trades against legitimate
  repetition, since a model editing ten files really does call the same
  few tools in a rotation, so the discriminator has to be lack of
  PROGRESS rather than repetition alone.

  T36 is built on exactly that discriminator. A dispatched call is
  futile when its own `{name}:{input}` fingerprint AND its result both
  repeat an earlier call from the SAME turn, which is information
  gained of zero rather than repetition: a ten-file edit rotation has
  ten distinct fingerprints, and a reread after an edit has a changed
  result, so neither ever counts. Re-simulated against the archived
  history, the 77 calls are 9 distinct fingerprints and 67 futile
  dispatches; the notice lands at call 13 and the stop at call 28.

### CORRECTION 2026-08-26

A "Queued from T39 (2026-08-25)" entry stood here for one day claiming
that temur loses Terminal-Bench tasks through turns that assert
completion while calling no tool, and that the T36 futile-call guard
could not see the shape by construction. **That entry is removed.** It
described an artifact of the T39 harness adapter, which delivered each
task instruction one line per turn, so the model was answering
fragments rather than narrating false progress. With the instruction
delivered whole the signal falls from 52% of turns to 14%. The T36
guard was never the issue.

### Queued from T34 (2026-08-18)

- **gemma-3-4b's 0/9 is still unexplained.** The other two
  tools-dropped models came off zero the moment a working chat template
  delivered the tools: Phi-4-mini 0/9 to 4/9, SmolLM2-1.7B 0/9 to 2/9.
  gemma-3-4b did not. Under the same substitute Qwen2.5 template its
  doctor probe went 55 to 213 prompt tokens, so the tools demonstrably
  ARRIVED, and it still produced zero native tool calls, zero prose
  recoveries, and spent 150-430 seconds per task generating confident
  fabricated tool results including the contents of a `README.md` that
  does not exist. So the delivery problem was real and was fixed, and
  something else is in the way. Unknown whether that something is the
  model, the encoding mismatch, or gemma's own template needing a
  different substitute; the eval archive plus one hour of probing would
  narrow it. Worth doing because the honest OFFLINE.md line for this
  row is currently "unresolved", and because it is the one case where
  the substitute-template recipe converts a silent failure into an
  expensive one.

- **The tools-drop probe's cost is now real, and unmeasured off this
  machine.** 106 seconds of prefill on one CPU-only box with one model
  at ctx 8192 and the FULL prompt profile. The compact profile sends
  roughly a third of that text, and a GPU server would not notice
  either. Nobody has measured the spread, and the 300s bound was chosen
  from a single data point with headroom rather than from a
  distribution. If a second machine or a slower model ever runs doctor
  against a local server, record what it cost.

### Queued from v0.29.1 (2026-08-29)

- **Mid-turn persistence writes the whole session twice per
  round-trip.** `persist_now` runs at the top of every turn-loop
  iteration and again after each assistant append, and each call
  serializes the entire envelope, writes a per-pid temp file and
  renames it. At the 4 MiB `session_max_bytes` default that is ~8 MiB
  of serialization and I/O per round-trip on a 32-bit target, and the
  over-cap path is worse: `session_store::save` re-serializes every
  message individually to size them and then serializes the trimmed
  envelope again, three passes over the history for one write. F4 of
  the v0.29.0 code review, and deliberately NOT fixed there: the
  crash-safety it buys is the entire point of T40 P2, and trading it
  away needs a measurement first. Measure the cost at the cap on the
  i686 target, then choose between dropping the top-of-loop write
  (redundant except immediately after a fold, which is the one case
  that changes history without appending an assistant message) and
  skipping the write when the serialized bytes are unchanged.

### Queued from T40 (2026-08-27)

- **temur's own request floor is most of a small window.** Measured
  against the local llama.cpp server (Qwen3-4B, one request per
  profile, `context_window` 12288, prompt `"x"`): the FULL prompt
  profile costs **6,991 prompt tokens** before the task is considered
  at all, and the COMPACT profile **2,763**. System prompt plus eight
  tool definitions, nothing else. At ctx 12288 the full profile
  therefore leaves ~5.3k tokens for the entire task, which is part of
  why both boxes show ctx-exhausted temur cells: the window was
  substantially spent before the first instruction. It also means a
  4096-token `context_window` is not merely tight but unusable, since
  the floor exceeds the whole window and the advisory fires on the
  empty session (seen live in the T40 smoke, run 1). Candidates, NOT
  built: select the compact profile automatically when
  `context_window` is below a threshold, and report the floor in
  `doctor` so it is visible before a run rather than inferred from a
  failure. **F6 (T40)** in the RUNBOOK; finding numbers restart per
  record, and an unqualified "F6" already means three different things
  across this file and the RUNBOOK, so cite it with its milestone.
  **DEQUEUED by T41 (2026-08-30): both candidates built.** Auto is now
  the `prompt_profile` default with a 20480 threshold (16384 through
  v0.30.0, raised in v0.30.1 to stop contradicting doctor's own WARN
  line), and `doctor`
  reports the floor with an offline estimate plus a measured figure on
  a keyless local endpoint. The two follow-ons T41 could not close (no
  startup window probe, no task-level measurement of the effect) are
  queued under "Queued from T41" above. The measurement note attached
  to F6 in the RUNBOOK carries a dated correction: the mechanism it
  offered for a 40-token gap between two readings does not exist in
  the tree.

- **The promise guard exempts the case auto-compaction lives in.** T35
  D2 nudges a turn that ends by promising work, but only when NO tool
  ran all turn: a turn that promised and never acted is suspect, one
  that worked and then promised is not. A long tool-using turn that
  ends with future-tense intent and no call therefore falls straight
  through, and that is exactly the shape of a compacted turn, which is
  long and tool-heavy by definition. First live instance is T40 smoke
  run 3b: after a clean compaction the model wrote "Now creating
  summary.txt with the complete list of files and their first lines."
  and ended the turn without the call, leaving the task unfinished
  while reporting success. 1 of 2 auto runs. Candidate, NOT built:
  extend the D2 predicate to fire on a promise in the FINAL message
  regardless of earlier tool use, which is a narrower condition than
  the current whole-turn one and should not touch the cases T35 tuned.
  Evidence: `~/temur-eval-archive/t40-smoke/README.md`.

### Queued from T39 (2026-08-26)

- ~~**An unattended agent has no way to compact.**~~ SHIPPED in T40
  (v0.29.0) as `auto_compact`, default on in one-shot `-p` only. The
  original entry follows for the record. Desktop experiment 1 saw the
  same wall on a second machine (GPU, same ctx 12288) in
  `build-cython-ext` and `gcode-to-text`, the same two tasks as the
  main box; that run is archive-only evidence
  (`~/temur-eval-archive/desktop-exp1/`) and was never a measured
  comparison, so it corroborates the finding and scores nothing.

  Desktop experiment 3 (2026-08-29). The same GPU box ran the same
  16-task subset again on Qwen3-8B with thinking held off, and
  ctx-exhausted cells went from 7 to 22 across the three harnesses
  (temur 7, opencode 14, codex 1). No cause is attributed and the
  binary there is still 0.28.0, so it is not a measurement of what
  shipped. Published as "Same rig, larger model" in
  `docs/COMPARISON.md`; evidence `~/temur-eval-archive/desktop-exp3/`.

  Desktop experiment 4 (2026-08-30). The same box and the same subset
  again, on Qwen3-Coder-30B-A3B-Instruct with a partial MoE offload,
  and ctx-exhausted went 7 to 22 to **40 of 96 cells**, now the single
  dominant outcome and spread across all three harnesses (temur 16,
  codex 14, opencode 10) and 13 of the 16 tasks. temur's own share is
  16 of its 32 cells, the largest yet. The binary is still 0.28.0
  there too. Across the ladder the pass totals are 7/96 at Qwen3-4B,
  4/96 at Qwen3-8B and 21/96 at 30B-A3B, so the model moved the score
  while ctx-exhausted grew with it. Published as "Same rig, MoE model"
  in `docs/COMPARISON.md`; evidence
  `~/temur-eval-archive/desktop-exp4/`.

  Original entry: temur watches its own
  context fill, says so, and then dies on the next request. Measured on
  Terminal-Bench with one-shot `-p`: the advisory fires at roughly
  11,942 of 12,288 tokens telling the user to run `/compact` or start a
  new session, and the run then ends on an HTTP 400 from the server,
  request 13,523 tokens against a 12,288 window. In one-shot there is
  no user to type either remedy. The advisory is correct and useless at
  the same time. Candidate: auto-compact in one-shot when the advisory
  threshold fires, or an explicit `--auto-compact`. Note the wall is
  not temur-specific, codex hits it at the same context size in 3 of
  its 32 cells, so what is missing is the unattended remedy rather than
  headroom. Evidence: `~/temur-eval-archive/t39-terminal-bench/`,
  F4 in its `PRODUCT-FINDINGS.md`.

- **T40 auto-compaction differential on Terminal-Bench.** Added
  2026-08-29. Every Terminal-Bench number published so far was taken
  with a v0.28.0 binary, which predates `auto_compact`, so the feature
  built for exactly those cells has never been measured on them.
  temur-only, v0.28.0 against v0.29.1, same GPU box, same 16-task
  subset, Qwen3-8B with thinking off, two runs each: the one variable
  is the binary. Planned as desktop experiment 5.

- ~~**A killed run loses its whole session.**~~ SHIPPED in T40
  (v0.29.0): the session is written after every round-trip, so a
  `SIGKILL` costs at most the request in flight. The candidate below
  guessed "append as the turn proceeds"; what shipped rewrites the
  whole file at each point instead, which keeps the existing atomic
  write and size-cap semantics rather than inventing an append format.

  Original entry: temur writes
  `sessions/<id>.json` at exit, so a run stopped by a hard kill leaves
  nothing: no transcript, nothing for `--continue` to resume, nothing
  to inspect afterwards. 4 of 32 Terminal-Bench cells have no session
  file and all four are the cells whose budget expired. This bit the
  measurement as well as the user: turns and tool calls are
  unavailable for exactly the cells where they would be most
  interesting. Candidate: append as the turn proceeds rather than
  writing once at exit. A SIGTERM handler alone would not be enough,
  since SIGKILL cannot be trapped. F5 in the same archive.

- **temur addresses a user who is not there.** On a single-line
  Terminal-Bench task the model ended its turn asking permission to
  proceed and received EOF instead of an answer. Same family as the
  item above it: one-shot and piped runs have no interlocutor, and
  anything deferred to a next turn never happens. Smaller than the
  other two and possibly a prompt-profile sentence rather than code.
  F1 in the same archive.

### T0 - Identity + honest gate
- Rename `opencode-rust` → `temur`: package name, `--version`, binary name,
  doc headers, RUNBOOK. Keep an MIT attribution note for the OpenCode-ported
  tool prompts.
- Skills dir: introduce `.temur/skills`; keep reading `.opencode/skills` as a
  fallback for one release.
- **Close the gate gap: the shipped artifact is currently the least-tested
  one.** The repo's build config and check.sh cover only the gnu *debug*
  target; the musl-static ship path is an undocumented build variant. check.sh
  additionally: builds `--release` for `i686-unknown-linux-musl`, asserts
  staticness via `readelf -l` (**no `INTERP`**) and `readelf -d` (**no
  `NEEDED`**), runs the test suites and mock-REPL/TUI smokes in the container
  **against the musl binary**, and runs `--version` + mock smoke in a bare
  (busybox/near-scratch) container where a dynamic binary could not even load.
- gnu-debug stays as the fast inner loop; musl-release is the acceptance path.

### T1 - Provider-neutral core
Move `ContentBlock`, `StopReason`, `Usage`, `Role`, `ResponseMessage` into
`provider::types` as plain data (serde retained only for temur's own use, e.g.
T5 persistence, never as a wire format). `anthropic::types` keeps its wire
shapes and gains explicit to/from-wire conversion; `cache_control` injection
stays inside the Anthropic provider. `Usage` becomes best-effort; `ChatRequest`
gains the neutral optionals from §2. Exit criterion: the entire existing
fixture + live-conformance suite green with **zero fixture changes**, proof
the refactor is pure re-plumbing.

### T2 - OpenAI-compatible provider
- `provider/openai_compat/`: wire types, chunk-stream SSE decoder (shared
  line-framing extracted from the Anthropic parser), transport via the
  existing `Transport` seam, retry policy reused.
- **Fixtures, same discipline as M1, no live calls from the build session:**
  (a) hand-authored chunk streams from the OpenAI API reference, critically
  tool-call `arguments` fragmented across chunks, parallel tool calls,
  `[DONE]`, final-chunk usage; (b) cross-checked against openai-python/-node
  SDK test fixtures (read-only reference; divergences resolve toward the
  SDKs); (c) **quirk fixtures modeling local servers** (llama.cpp/Ollama):
  absent usage, absent tool-call IDs, whole-call-in-one-chunk, role deltas.
  A later operator-run capture against a real local server can be frozen in as
  layer 3, mirroring `tests/fixtures/live/`.
- Config: `provider: "anthropic" | "openai-compat"` plus per-provider
  `base_url`/`model`/auth. API keys only by file path (`APP_SECRET_FILE` or a
  per-provider path, never env, never argv), explicitly optional for keyless
  endpoints. Defaults stay `anthropic` / `claude-sonnet-5`; no default flips
  without a decision.
- Write down the expected trait changes (§2's leak list) as predictions and
  diff them against what actually changed in review. This milestone is graded
  as much on what it reveals about the abstraction as on the feature. If T1
  was done well, `provider::mod` should barely change; every place it does is
  a lesson.

### T3 - Offline/local polish
Keyless startup path; tolerant degradation when usage/IDs are absent; clear
behavior at small context windows; `docs/OFFLINE.md` (llama.cpp + Ollama
quickstart, LAN topology, recommended small models). Acceptance: a scripted,
operator-run end-to-end demo: temur (musl-static, in the container) driving a
local llama.cpp server with zero internet. Running an inference server from a
build session is new system surface: plan and ask first.

### T4 - Weak-model hardening
The §2 P0 list: argument repair, text-emitted-tool-call detection, bounded
correction retries, compact prompt profile selected per model/config, guard
extensions, plus `tests/weak_model.rs`: scripted fixture streams that *are*
the misbehaviors (malformed args, hallucinated tool names, loops), asserting
the loop degrades politely.

### T5 - Session persistence
`--continue` / session files under a config dir; JSON in the neutral
vocabulary; atomic writes (power-cut-friendly, it's the niche); size-capped
with `u64` discipline. Anthropic thinking signatures round-trip opaquely.

> As-built note: session files live under the STATE dir
> (`$XDG_STATE_HOME/temur/sessions`, fallback `~/.local/state`), not the
> config dir as written above: megabyte transcripts of tool output don't
> belong in a dotfile-synced `~/.config`.

### T6 - Editing + interruption
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
> in one message: history stays wire-valid and the driver-loop save
> persists it. Exclusions: plain-REPL interruption (blocked main thread,
> SIGINT would need a new dependency) and fully stalled streams: ureq
> timeouts are whole-phase deadlines, not idle timeouts (verified in
> ureq 3.3.0 source), so they cannot implement cancel-polling and
> double-Ctrl+C force-quit remains that escape hatch. Found en route:
> `sh -c` forks its command, so bash kill/timeout now kill the process
> GROUP via the sh *builtin* kill (minimal images ship no kill binary).
> **Fuzzy edit:** OpenCode's line-trimmed and block-anchor matchers
> ported behind exact-match-first; within a matcher ≥2 candidates is an
> error demanding more context (stricter than OpenCode, never a guess);
> `replaceAll` stays exact-only; no Levenshtein scoring at all. Fuzzy
> successes are marked in the tool output; the prompts still demand
> exactness.

### T7 - Multi-arch packaging
Release builds for `i686-musl`, `armv7-musleabihf`, `aarch64-musl`,
`x86_64-musl`; per-target readelf gates; checksums; an install page that leads
with the one-liner. ARM targets get build-level verification even if hardware
smoke-testing waits for hardware.

**As-built (2026-07-22).** All four targets ship from the one proven recipe:
rust-lld against rustup's self-contained musl; per-target CC for ring's
C/asm (host gcc for the x86 pair, Ubuntu cross-gcc for the ARM pair) with
`-U_FORTIFY_SOURCE -fno-stack-protector` on ARM. The planned fallback ladder
(clang, vendored musl-cross toolchains, cross-rs) was never needed. Fortify
data point: an aarch64 build *without* those CFLAGS also links: musl
provides the `__stack_chk_*` symbols ring's objects reference, and gcc
13.3/ring 0.17.14 emits no fortify `__*_chk` at all, so the flags are
defense-in-depth against toolchain drift, not currently load-bearing.
Verification matrix: i686 = the unchanged full `check.sh`; x86_64 = native
full test suite + host smokes + TUI pty + bare amd64-busybox proof; ARM =
build-level gates (ELF class/machine, static, no INTERP/NEEDED, armv7
VFP-args tag) plus qemu-user smokes (`--version`, live `tls-probe` through
ring's real ARM asm, mock REPL). qemu is not hardware (scheduling, timing,
and kernel-interface behavior differ), so the ARM hardware smoke stays an
open follow-up until hardware exists; `scripts/release.sh` gates a release
on all of the above plus the leak grep and checksum staging.

### v0.1.1 - post-release review fixes (as-built, 2026-07-23)

A high-effort adversarially-verified code review of the shipped T6+T7
range found 10 defects; v0.1.1 fixes all ten. CONFIRMED correctness:
**F1** block_anchor bound the nearest closing anchor with no middle check
(silent mis-splice - now: exact-expected-offset preference + a ≥½
order-preserving middle-similarity guard on the nearest fallback; match
correctly or refuse); **F2** the installer's GNU-only `sha256sum -c`
flags broke busybox/Alpine, the core musl audience (now portable
awk-extract + string compare, unlisted artifact = hard fail; tested in
busybox itself via `scripts/install_test.sh`); **F3** the fuzzy splice
kept the model's indentation verbatim, corrupting nested Python-style
blocks (now: uniform leading-whitespace delta re-applied to newString,
inconsistent delta refuses); **F4** plain-REPL Ctrl+C orphaned bash
children (now: `src/signal.rs` SIGINT handler, plain mode only, no
SA_RESTART; first press = cooperative interrupt via the shared token,
second press = exit 130; new direct dep `libc`, FFI-only; closes the T6
exclusion); **F5** an Esc racing a stream error swallowed the error AND
the streamed partial (now: providers return the partial under cancel,
the agent's notice carries a surviving real failure). PLAUSIBLE hazards:
**F6** thinking-only interrupted landings persisted a message that 400s
on replay (now: push nothing); **F7** Enter+Esc coalescing could drop
the interrupt via turn-entry `cancel.clear()` (now: the clear moved to
submission (TUI Submit arm / plain REPL post-read_input), documented
invariant on `Session::turn`). Cleanups: **F8** matchers precompute
spans/trims once; **F9** one private `Session::build` behind new/resume;
**F10** `INTERRUPT_MARKER` const + one synthesis helper. Every fix
carries regression tests; gates: full `check.sh` per phase, installer
matrix (host + busybox), SIGINT black-box matrix, full `release.sh`.

### T8 - Daily-driver UX (shipped as v0.2.0)

Post-v0.1.1 direction (operator-decided): daily-dogfooding ergonomics,
landed as independently gated pieces with no per-piece release: T8
shipped as v0.2.0. All feature pieces (P1–P3) landed 2026-07-25; the
close-out (version bump, docs, full release.sh + installer gates,
annotated tag, private GitHub release with closing gate) ran the same
day, repeating the v0.1.1 PRIVATE release flow. Close-out as-built: the
bump touched exactly the six pinned sites (Cargo.toml/Cargo.lock,
install.sh VERSION, three README pin groups); no product code and no
dependency changes rode along. The PUBLIC one-liner gate remains the
one open release item, deferred to the visibility flip (RUNBOOK).

**T8-P1 (as-built, 2026-07-25): slash commands + named-profile model
switching.** Config gains `profiles` (nickname → provider/model/base_url/
api_key_file/max_tokens/context_window) plus a startup `profile`
selector; every profile is validated eagerly at startup, so `/model` can
only fail on credential/IO, and absent profiles are byte-identical to
pre-T8 behavior (error strings unit-asserted). Any leading-`/` input
line is command-space, intercepted between turns and never reaching the
model or history: `/help`, `/status`, `/model [name]`, `/clear`,
`/thinking [on|off]`. One live construction path (`provider::build_live`)
serves startup AND switches, credentials read by path at activation
time, never cached across switches; `/model` builds the new provider
first and mutates the session only on success (atomicity proven through
the real path with an unreadable key file and with `APP_SECRET_FILE`
unset); `/clear` persists the emptied session immediately so
`--continue` resumes empty. Command feedback travels as `AgentEvent`s:
`Notice` text plus `ModelSwitched`/`ThinkingChanged`/`SessionCleared`
chrome signals (`ThinkingChanged` is a small scope addition over the
plan so TUI footer chrome cannot go stale). TUI: commands echo as dim
`Cell::Command` lines, recallable via ↑, and never claim the title, a
User cell, or the busy spinner. Mutating commands are disabled under
`--mock`/`--capture-sse`. Deliberately punted from this piece: the
`scripts/check.sh` container-suite-list edit and the `tests/sigint.rs`
fold-in that would ride with it (RUNBOOK note stands, since closed by
T8-P2's check.sh hygiene pass).

**T8-P2 (as-built, 2026-07-25): markdown rendering + monochrome styling
pass + check.sh hygiene.** Landed as its own gated sub-phases. (1)
check.sh hygiene, the milestone's sanctioned check.sh edit: every
host-side product invocation now runs with isolated
`XDG_CONFIG_HOME`/`XDG_STATE_HOME` in the run's temp dir, so the
operator's real config can no longer break the host TUI pty smoke (the
T8-P1 neutral-XDG workaround is retired), and `tests/sigint.rs` joined
the container suite list on both paths. (2) Markdown rendering:
`src/ui/tui/markdown.rs`, a pure `render(text, width) → Vec<Line>` over
pulldown-cmark 0.13 (default features off, strikethrough the only
extension; lock delta: pulldown-cmark + unicase only, no *-sys crates;
stripped i686-musl release grew ~260 KiB to ~5.6 MiB). Applies ONLY to
assistant prose in the TUI; the plain REPL and all other cell types are
byte-identical. Streaming re-parses the accumulating cell per frame;
unclosed fences render as code until the closer arrives. Documented
limitations: severed fences across tool-split cells re-parse per cell
(styling inverts, nothing lost), table/footnote/tasklist extensions off,
no syntax highlighting. (3) Styling: the accent contract formalized
(DIM/BOLD/ITALIC/UNDERLINED + Red errors / Yellow notices / Cyan
accents, bringing the pre-existing cyan uses in-contract) and the
running-tool line dimmed to match its finished form. Deviation from the
plan sketch: none of substance; soft breaks reflow as spaces (CommonMark
semantics), pinned by test.

**T8-P3 (as-built, 2026-07-25): `scripts/serve.sh` - background
llama.cpp server launcher.** Operator infrastructure for the
third-party inference server (the roadmap's server/multi-client-mode
exclusion is about temur-the-binary serving clients; temur gains no
server behavior). Command surface is `start|stop|status` only. It
inverts `offline_demo.sh`'s sealed-pod bring-up for one-window use:
plain `podman run -d` with a loopback-only published port
(`127.0.0.1:8080`, matching the default openai-compat `base_url`),
container-side bind `0.0.0.0`, and no exit trap: the server survives
script exit. Same pinned image and never-pull preflight as the demos;
host-side `/health` wait (30×2s) fails closed by removing the dead
container. Scripts + docs only: no product code, no new dependencies.
Deviation from the plan sketch: the container-name knob is
`CONTAINER_NAME`, not `NAME`: live testing caught WSL exporting
`NAME=<hostname>`, which silently overrode the default.

### T9 - Command ergonomics (as-built, 2026-07-25)

**T9-P1 (as-built, 2026-07-25): per-profile prompt profiles.**
`ProfileConfig.prompt_profile` (`"full"`/`"compact"`, validated eagerly
per profile at startup; absent = the global setting, itself defaulting
to full) resolves into `ResolvedProfile.prompt_profile`. main's inline
system-prompt assembly became ONE `rebuild_system(profile)` closure:
startup and switches share it, the config `system_prompt` override wins
in either profile, skills section and `{cwd}` captured once. A `/model`
switch onto a profile with a different prompt profile calls the new
infallible `Session::set_prompt` + `Registry::set_profile` AFTER the
provider build succeeds, so switch atomicity now covers system + tool
descriptions too (description-swap-only contract unchanged, re-pinned
by test both directions). `/status`'s thinking line gained
`· prompt: full|compact`.

**T9-P2 (as-built, 2026-07-25): `/models` + `/model <raw-id>`.** A
machine-readable `commands::COMMANDS` table (name / arg-hint / help)
now feeds `/help`, the TUI hint, and completion; `parse` stays the
authority on argument shapes. `/models` lists model ids from the ACTIVE
provider via injected `provider::list_models_live`: ureq GET
(anthropic `{base}/v1/models` with x-api-key by path, openai-compat
`{base}/models` with Bearer only when keyed), 64 KiB body cap, non-2xx
a clean status-naming error; `parse_models_json` is a separate pure fn
(both wire shapes share the `data[].id` envelope) unit-tested offline.
New `AgentEvent::ModelsListed` renders in both UIs; the TUI caches ids
for completion. `/model <arg>` with no matching profile is now a raw-id
switch on the active selection (only the model replaced; profile name,
limits, and prompt profile kept: names win on collision, making a
shadowed raw id unreachable by design); build-first atomicity and the
replay guards extend to both new paths. Raw ids are not validated
offline: a bad id is the provider's own error on the next turn.

**T9-P3 (as-built, 2026-07-25): TUI command styling + Tab
completion.** TUI-only; the plain REPL is byte-identical. Command-space
input renders cyan (windowed slice; within the existing accent
contract). The idle status row live-hints `/`-lines from the COMMANDS
table: unique-or-exact prefix match shows that command's row (exact
wins so `/model` isn't drowned by `/models`, the one deviation from
the plan sketch, which said only "unique"), several matches list names,
none nudges to /help. Pure `commands::complete()` returns full-line
candidates (command names; `/model` args = profile names then cached
`/models` ids, deduped; `/thinking on|off`; nothing else); `App` owns a
cycle-in-place Tab state: Tab applies/advances, BackTab reverses,
wraps; end-of-input only; no-op while busy; any other key invalidates;
the force-quit disarm behaves as for any key. `SessionInfo.profiles`
threads names in; a headless e2e injects a real Tab through the
render loop.

**T9-P4 (as-built, 2026-07-25): serve.sh MODEL_GGUF default.** With
`MODEL_GGUF` unset, `start` defaults it when `MODELS_DIR`
(default `$HOME/models`) holds EXACTLY one `.gguf` (POSIX `set --`
glob, no-match safe under `set -eu`), printing the chosen path; zero or
several files extend the existing FAIL with the searched dir and count.
Sibling scripts stay explicit-only.

### T10 - Session management (as-built, 2026-07-26)

Named multi-session, list + commands only (no picker, no modal input).
FORMAT_VERSION and the FNV-1a digest are untouched; pre-T10 files load
unchanged as each project's default session, and default-session files
written by T10 stay byte-identical to the pre-T10 shape (`name` is
`#[serde(default)]` + skip-when-`None`, pinned by goldens both ways).

- **Store (P1).** Default session keeps EXACTLY the old
  `{base}-{hash}.json` name; a named session is `{stem}-{name}.json`,
  name sanitized to `[A-Za-z0-9._-]` (disallowed chars DROPPED, so
  `"///"` errors instead of becoming `"---"`), capped at 32.
  `list_sessions` reads cwd/name/message-count from INSIDE each file
  and derives a display title from the first user prompt at list time
  (never stored); unreadable files list as `(unreadable)` entries
  rather than aborting the listing. `resolve_session_key`: exact name
  in the current project → globally-unique name → unique file-name
  prefix (how default sessions are addressed); ambiguity and misses
  are errors listing candidates with cwds. Pure, table-tested.
- **Ordering decision (defended).** `/sessions` sorts by filesystem
  mtime, newest first, `UNIX_EPOCH` fallback, file-name tie-break.
  This does NOT weaken the clock-less invariant: that invariant is
  about the FORMAT (a format depending on a clock lies on RTC-less
  hardware: nothing in the file or its name carries a timestamp,
  before or after T10). mtime is filesystem metadata that exists
  whether or not we read it, is read only at list time, and decides
  display order alone: no load/save/resume path consults it. On a
  clock-less device every mtime collapses to the epoch fallback and
  the listing degrades to stable name order; nothing else changes.
  Precedent: `tools/glob.rs` has sorted hits mtime-desc since v1.
- **Seam + replay (P2).** `Session::load_seed` joins the T8
  between-turns seam (same INVARIANT block): replaces
  history/usage/todos/context estimate, re-arms the context warning.
  `replay_items` flattens saved history into
  `User`/`Assistant`/`Tool{name}` items, deliberately lossy (tool
  output/args and thinking never replay; tool-result messages,
  including interrupt markers, produce nothing). New events mirror T9
  shapes: `SessionsListed { lines, keys }`, `SessionLoaded { items,
  notice }`. `--continue` now renders backscroll through the same
  event.
- **Commands + CLI (P3).** `/sessions`, `/resume <key>`, `/new <name>`
  (all replay-guarded). `/resume` is atomic the way `/model` is:
  resolution, load, and prepare_seed all run BEFORE any mutation;
  then load_seed + persist-path redirect + name bookkeeping together.
  Same-session keys no-op; cross-project resume warns that ToolCtx.cwd
  stays the current directory; the dropped-prompt rule reuses the T5
  notice. `/new` never writes a file: the first turn's save creates
  it. `--resume <key>` resolves at startup, is mutually exclusive with
  `--continue`, and is rejected under `--mock`; saves record the live
  session name; `/status` gained `· session: <name or (default)>`.
- **TUI (P4).** `Cell::Sessions` renders the listing notice-style;
  `SessionLoaded` folds as SessionCleared-then-rebuild (title claimed
  by the first replayed prompt: the header finally stops reading
  "new session" after a resume); replayed tools are `⚙ name`
  one-liners via `ToolCell::replay` (no body to box; FIFO pairing
  untouched). Input line survives a resume untouched.

Plain-REPL compatibility: every pre-T10 output shape is byte-identical
except the deliberate `/status` session-file line extension; the resume
summary line kept its exact `[!]`-notice rendering (now emitted from
the SessionLoaded arm, after the new backscroll lines).

### v0.3.0 - T9+T10 close-out (as-built)

T9 and T10 ship together as v0.3.0 after operator dogfooding, with the
close-out split in two stages: stage 1 (bump, CHANGELOG, docs, full
release.sh + installer gates to staged artifacts) runs first, and the
tag + private GitHub release are held until operator dogfood sign-off
(a procedural change vs v0.2.0, where the tag followed the gates the
same day). Close-out as-built: the bump touched exactly the six pinned
sites (Cargo.toml/Cargo.lock, install.sh VERSION, three README pin
groups); CHANGELOG.md was introduced (retroactive 0.1.0..0.2.0 plus the
unreleased 0.3.0 entry) as the source for release bodies from v0.3.0
on; all repo markdown was rewritten without em-dashes
(operator-decided 2026-07-26), with a byte-exact carve-out for verbatim
quotes of immutable artifacts (tag annotations and quoted program
output); no product code and no dependency changes rode along. The
PUBLIC one-liner gate remains the one open release item, deferred to
the visibility flip (RUNBOOK).

### T11 - Multi-model ergonomics (as-built, 2026-07-26)

Theme: make switching between local models routine instead of a
hand-edited chore, and measure (not assert) which small models can
actually drive the tools.

**T11-P1 (as-built, 2026-07-26): serve.sh selection.** `start` takes an
optional model name resolved against the basenames of
`$MODELS_DIR/*.gguf`, case-insensitively: an exact basename match
("name" or "name.gguf") wins outright, else a unique substring match
selects, and zero or several matches fail while listing every candidate
with a human-readable size (matches marked when ambiguous). Precedence
is explicit: `MODEL_GGUF` plus a name argument is a hard error (choose
one), `MODEL_GGUF` alone keeps its pre-T11 meaning, the lone-gguf
auto-default remains the no-argument path, and its zero/many failures
now list candidates instead of a bare count. The RAM fit check is WARN
only, by design: need = gguf file size + CTX x 128 KiB (a deliberately
generous per-token allowance for f16 KV plus compute buffers at the
CPU-only single-slot defaults) vs `MemAvailable`; warn-only because
mmap'd weights can run undersized (slowly), the estimate is coarse, and
a hard gate would turn an estimate into a false blocker. All byte math
is in awk (POSIX sh integer width is not guaranteed); `MEMINFO` is an
env knob so the check is testable against a fake meminfo file. A
running server keeps its model; switching is stop then start.

**T11-P2 (as-built, 2026-07-26): compact bash prompt hint.** Dogfood
finding: qwen3-1.7b executed tools when told which to use, but on
"delete the file" claimed it had no delete tool (while listing shell
execution among its capabilities); a direct "run rm via bash" worked.
The registry genuinely has no delete/move/chmod tool; bash is the
intended path, and the compact bash description never said so. Fix: one
appended sentence in `src/tools/prompts/compact/bash.txt` naming
delete/move/copy/chmod (rm, mv, cp, chmod) as bash's job. The full
profile stays untouched (Claude-class models have not shown the gap),
caps hold with margin (766 of 1000 chars; the swapped-profile total
stays far under the 8 KiB budget), and FORMAT_VERSION plus the FNV
digest are unaffected (prompt text is not part of the session format).

**T11-P3 (as-built, 2026-07-26): indirect-tool-selection probe.**
weak_model_eval.sh task 7 (indirect-delete): pre-seed a scratch file,
prompt "Delete the file obsolete.tmp in the current directory", naming
neither bash nor rm. PASS requires BOTH the file gone (host-verified)
and a bash rm call in the transcript, a deliberate, documented widening
of the filesystem-only scoring rule: the probe measures tool SELECTION,
and end state alone cannot prove which tool acted. SCORE becomes N/7.

**T11-P4 (as-built, 2026-07-26): backend recipes + shortlist.**
OFFLINE.md gained a fleshed-out Ollama recipe (profile JSON, /models
note, the num_ctx trap kept) and a new LM Studio recipe (GUI-loads-the-
model caveat, port 1234 profile, /models as the id-discovery path, and
an honest WSL2-to-Windows-host orientation: mirrored networking makes
localhost work, classic NAT needs the gateway-route host IP plus
listen-on-all-interfaces plus a firewall allowance; docs only, nothing
scripted). The models table became a shortlist with file size, est. RAM
at 8k ctx (the serve.sh warning's own arithmetic), tool calls, indirect
selection, and a status column that distinguishes "verified <date>"
(full eval run) from "reported" (earlier observation): P5 fills the
verified rows from live runs.

**T11-P5 (as-built, 2026-07-26): live shortlist verification.** Two
candidates downloaded from the unsloth Hugging Face repos into
`$HOME/models` (now genuinely multi-gguf, files kept): Qwen3-4B-
Instruct-2507 Q4_K_M and Qwen2.5-Coder-3B-Instruct Q4_K_M. Each cycle
exercised serve.sh name selection live, then ran the full seven-task
eval in its own pod. Results: Qwen3-1.7B 7/7 (the P2 hint in play; the
indirect probe that motivated it now passes), Qwen3-4B-Instruct-2507
7/7 and faster per task than the 1.7B, Qwen2.5-Coder-3B-Instruct 0/7
with a finding worth keeping: it chose the right tool every time
(including bash rm on the indirect probe) but emitted only prose JSON,
never structured tool calls, on llama.cpp server-b10068 with --jinja,
so it fails on wire format, not reasoning. Details and verbatim
transcripts: RUNBOOK "T11 acceptance".

### v0.4.0 - T11 close-out (as-built)

T11 ships alone as v0.4.0 (operator decision: a feature milestone takes
a minor bump), repeating the v0.3.0 two-stage procedure: stage 1 (bump,
CHANGELOG claim, docs, full release.sh + installer gates to staged
artifacts) runs first, and the tag + private GitHub release are held
until operator dogfood sign-off. Close-out as-built: the bump touched
exactly the six pinned sites (Cargo.toml/Cargo.lock, install.sh
VERSION, three README pin groups); no product code and no dependency
changes rode along. The PUBLIC one-liner gate remains the one open
release item, deferred to the visibility flip (RUNBOOK).

### v0.5.0 - T12 + T14 close-out (as-built)

T12 (CI) and T14 (onboarding + one-shot) ship together as v0.5.0,
plus a stage-1 usage-docs pass (docs/USAGE.md with transcripts from
real local-model runs, the docs/SETUP.md audience note, README links to
USAGE.md and TUI.md). Same two-stage procedure as v0.3.0/v0.4.0:
stage 1 (docs, bump, CHANGELOG staging, close-out records, full
release.sh + installer gates) stops at gated LOCAL artifacts with no
tag, no push, no release; stage 2 runs as a separate prompt after
operator sign-off. T13 (hosted provider verification) is explicitly
NOT in this release: it stays parked until API keys exist, and the
hosted init templates remain spec-written, live-unverified (the README
says so where they are offered). Close-out as-built: the bump touched
exactly the six pinned sites via `cargo update -p temur --offline`
(temur entry only); no source, test, or dependency changes rode along;
the visibility decision is deferred to stage 2, and the PUBLIC
one-liner gate stays deferred to the visibility flip (RUNBOOK).

## 4. Invariants (every milestone)

- Ships musl-static: `readelf` shows **no INTERP, no NEEDED** (gated from T0).
- Pure-Rust TLS: rustls + ring only. Every new dependency is vetted for C code
  or OpenSSL before adoption; the `cargo tree` gates stay; anything pulling
  `*-sys` crates that break musl/32-bit is rejected or feature-gated off.
- 32-bit discipline: `u64` for sizes/offsets; no large-allocation assumptions.
- Two-identity key isolation: secrets by path only, never env, argv, logs, or
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
- **Scope/maintenance: becoming a worse OpenCode.** The pull is toward the
  LOW list, where temur competes on OpenCode's terms with a fraction of the
  hands and loses. The §1 decision rule is the backlog filter; small surface
  is a feature of the product, not a deficit.

## 7. Build order

**First:** T0 (days, and the gate gap genuinely matters), then T1→T2→T3→T4 as
one arc. That arc *is* the differentiation: after it, the one-liner is fully
true and demonstrable: before it, the offline pitch is a slide. T5–T7 follow
by usability pull.

**Not building:** bespoke vendor providers (the compat endpoint covers them),
LSP, MCP (revisit post-T7 at the earliest), IDE/web UI, server mode, plugin
system, sub-agents, any async-runtime migration (blocking ureq is correct for
this niche), and any dependency that compromises musl-static purity for
convenience.

**Where the story is weak, kept visible on purpose:** i686 desktop alone is a
retro hobby, not a market (ARM/embedded is the market; i686 is the proof);
Go-based single-binary agents narrow the binary-profile advantage, so the moat
is the combination with offline + weak-model competence; and local-model
tool-calling quality may set a floor we don't control. The ladder front-loads
exactly the milestones that test those three uncertainties.
