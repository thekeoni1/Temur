# opencode-rust — Roadmap

## Framing (REVISED 2026-07-03 — supersedes the original brief)

opencode-rust is a **general OpenCode clone for 32-bit (i686) Linux**, usable for
**any** task. It is *not* tied to one organization's workflow, and its acceptance is
*not* gated on any single workflow.

Specifically: the formerly-planned "Tier-2" task (a skill-driven workflow that uses an
external tabular-data CLI to reproduce a report as XLSX) is **no longer a blocking
acceptance gate**. It is one useful end-to-end test among many, to be run whenever its
inputs are available. **Nothing is blocked waiting on it.**

Design rule (unchanged): seams where breadth lands (provider, UI, tool registry); no
speculative generality inside them.

## Status: v1 complete and live-verified (2026-07-03)

M0–M6 are done. The Tier-1 live smoke passed under the operator/appsvc path (coherent
streamed responses, full tool round-trips, correct on-disk effects, no provider
errors), and the M6 close-out froze 8 live SSE captures into `tests/fixtures/live/`
with a strict conformance suite over them. The live reconciliation required
**fixture/test changes only — zero runtime code changes**.

| # | Milestone (v1) | Status |
|---|---|---|
| M0 | Scaffold + TLS prove-it gate (rustls/ring + webpki-roots on i686, host + container) | ✅ |
| M1 | Wire types + SSE parser + fixture suite (cross-checked vs official SDK fixtures) | ✅ |
| M2 | Anthropic provider (transport seam, retry, cache_control) — offline-tested | ✅ |
| M3 | Tool set: read/write/edit/bash/glob/grep/todo with ported OpenCode prompts | ✅ |
| M4 | Agent core: turn loop, tool dispatch, pause_turn/refusal handling, guards | ✅ |
| M5 | Line REPL + wiring + `--mock` replay; container e2e smoke | ✅ |
| M6 | Tier-1 live handoff: staged release binary, RUNBOOK, live smoke, capture freeze + strict conformance | ✅ |

Current defaults (deliberate, unchanged until a decision says otherwise): model
`claude-sonnet-5`, thinking OFF, JSON config, blocking ureq + rustls(ring) +
webpki-roots.

## Layered architecture (as built)

```
┌───────────────────────────────────────────────────────────────┐
│ UI            ui::Ui trait — line REPL now, ratatui later     │
├───────────────────────────────────────────────────────────────┤
│ Agent core    agent::Session — history, turn loop, guards,   │
│               usage accounting; emits AgentEvent              │
├──────────────────────────┬────────────────────────────────────┤
│ Tools                    │ Provider                           │
│ tools::Tool + Registry;  │ provider::Provider trait over      │
│ prompts ported from      │ neutral types; anthropic/: wire    │
│ OpenCode (MIT)           │ types, SSE, transport seam, retry  │
├──────────────────────────┴────────────────────────────────────┤
│ Plumbing      config (JSON), errors, logging, secret-by-path  │
└───────────────────────────────────────────────────────────────┘
```

## General-tool milestone set (post-v1; replaces the old "Tier-2 = done" finish line)

> **Ordering is an OPEN priorities call** — adoption-speed (E, A, B first) vs
> capability-depth (D, C first) has not been decided. Do not assume a sequence;
> ask before committing to one.

| ID | Milestone | Contents |
|----|-----------|----------|
| A | Real-world usability hardening | Longer/messier multi-step tasks; big-file behavior (caps, offsets, memory on 32-bit); error-path polish; context-growth behavior over long sessions |
| B | Richer TUI (ratatui) | ✅ DONE 2026-07-03. Second `Ui` impl (render thread + std mpsc, no async); OpenCode-shaped session view; plain REPL kept for piped/scripted use (auto-selected on non-TTY, `--plain`/`--tui` to force); `tui-probe` diagnostic; 4-layer offline tests incl. pty smokes in check.sh. Zero agent-core changes. Design notes + seam assumptions: `docs/TUI.md` |
| C | Second provider (Gemini) | Via the existing `Provider` trait seam; additive, no core changes expected |
| D | Capability features | Smarter edit matching (OpenCode-style fallbacks); thinking-on for hard tasks (config exists, default off); session persistence; general robustness; **turn interruption** (known v1.x UX gap from milestone B: a hanging turn can only be force-quit, not interrupted — needs a small agent-core seam extension, a cancel flag checked in the turn loop; see docs/TUI.md. Priority is the project owner's call) |
| E | Packaging + install/handoff | So other people on 32-bit systems can run it: build/install story, config docs, operator-independent setup guidance |
| — | CLI-workflow end-to-end test | Non-blocking; run when inputs/skill are available, as one of several real-task validations under (A) |

Standing constraints (unchanged from v1): pure-Rust TLS (rustls/ring, no OpenSSL);
32-bit-safe sizes; secrets by path only, never in the build environment; every change
verified by `scripts/check.sh` on host **and** in the `i386/debian:stable` container;
no live Anthropic calls from build sessions — live runs go through the operator/appsvc
path (see `docs/RUNBOOK.md`).
