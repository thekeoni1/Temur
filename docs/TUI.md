# TUI (milestone B) — design notes, seam assumptions, known limits

The TUI is a second `Ui` implementation (`src/ui/tui/`) over the unchanged
`AgentEvent` stream; the agent core was not modified for it. Layout is a
behavioral port of OpenCode's session view (header band / scrollback with
sticky bottom / prompt / status row / footer), monochrome-adapted.

## Style contract (T8-P2, formalized)

Default terminal colors, restyled only with the DIM/BOLD/ITALIC/UNDERLINED
modifiers plus exactly three named accents: **Red** (tool errors),
**Yellow** (notices/warnings), **Cyan** (accents: the turn-tail ▣ mark and
inline code in markdown). No themes, no backgrounds, no borders, no other
colors — the transcript inherits whatever palette the terminal runs.

## UI selection

- Default: TUI when **stdin and stdout are both TTYs**, plain line REPL
  otherwise. So every piped/scripted invocation (mock e2e in `check.sh`,
  operator transcripts) gets the plain REPL with no flag needed.
- `--tui` / `--plain` force the choice (mutually exclusive). They compose
  with `--mock` and `--capture-sse`.
- `tui-probe` subcommand: standalone terminal prove-it (alt screen + raw
  mode + key input, 10s auto-quit), the TUI analogue of `tls-probe`.

## Architecture

- `app.rs` — pure state (transcript cells, input editor, scroll); folds
  `AgentEvent`s; injected clock (`now_ms`) so tests are deterministic.
- `view.rs` — pure draw of `App` into a ratatui frame.
- `wrap.rs` — deterministic greedy word-wrap (unicode-width).
- `mod.rs` — runtime: the terminal lives on a dedicated **render thread**
  (std `mpsc`, no async) so resize/scroll/Ctrl+C stay responsive while
  `Session::turn` blocks the agent thread. `Ui::event` sends events over a
  channel; `Ui::read_input` blocks on a reply channel.
- Terminal restoration is triple-covered: normal path after the render
  loop, a chained panic hook (restores before the panic message prints),
  and `TuiUi::Drop` (joins the render thread), so agent-thread errors also
  restore the screen.

## KNOWN SEAM ASSUMPTION — FIFO tool pairing (load-bearing)

`AgentEvent::ToolStart` carries no call id, so the TUI pairs each
`ToolEnd` with the **oldest still-running tool cell** (FIFO). This is
sound today because the core streams `tool_use` blocks in order and
executes them **sequentially in that same order** (`agent/mod.rs` turn
loop). **It breaks if tool execution ever becomes out-of-order or
concurrent.** The remedy at that point is a seam extension, not TUI
heuristics: add a call id to `ToolStart`/`ToolEnd` (the provider already
has `tool_use.id`) and pair by id. Tested in `tests/tui.rs`
(`fifo_pairing_matches_parallel_tools_in_order`); an unmatched `ToolEnd`
is appended rather than dropped, so a future mismatch degrades visibly
instead of silently.

## BEHAVIOR CHANGE (milestone B) — provider errors in the plain REPL

Provider-level failures used to be `eprintln!` (stderr) from `main.rs`;
they now go through the UI seam as `AgentEvent::Notice`, which the plain
REPL prints as `  [!] provider error: …` on **stdout** (the TUI renders a
notice block; stderr writes would corrupt the alternate screen). Operator
scripts that filtered stderr for provider errors must watch stdout
instead. Everything else about the plain REPL's output is unchanged from
v1.

## Turn interruption (T6) — as built

Esc during a running turn interrupts it cooperatively: the render thread
holds a clone of the session's `CancelToken` (passed into `TuiUi::new` —
never a `Session` reference) and sets it; the blocking agent stack polls
the token at its natural pause points — each received SSE frame, each
retry-backoff slice (≤200 ms), before each tool call in a batch, and
every ≤200 ms of a running `bash` (whose process group is killed on
cancel — no orphaned children).

Landing rules (agent core, `Session::turn`): completed text, signed
thinking, redacted thinking, and fully-parsed `tool_use` blocks are kept;
a `tool_use` still mid-JSON (`input_raw`), unsigned thinking, and unknown
blocks are dropped. Kept `tool_use` blocks are answered immediately in
ONE user message of synthesized `[interrupted by user]` error results —
they are never executed — so every `tool_use` id is answered in the next
message and the landed history is wire-valid for both providers. An
interrupt that arrives before any content lands nothing: history ends
with the plain user prompt and the resume seam's dangling-prompt rule
drops it on `--continue`. The driver-loop save runs after the landing,
so an interrupted session resumes cleanly.

FIFO pairing is preserved: every tool cell the stream opened gets a
`ToolEnd{is_error: true}` (kept and dropped alike) before the
`turn interrupted` notice and the normal `TurnComplete`. The status row
shows `esc interrupt` in the busy hint and `interrupting…` once Esc is
pressed, until the turn lands. Esc while idle is a no-op; a second Esc is
idempotent; Esc participates in the any-key-disarms rule for the
force-quit prompt.

**Plain-REPL interruption (F4, v0.1.1 — closes the T6 exclusion).** The
plain REPL now interrupts too: a minimal SIGINT handler (`src/signal.rs`,
`libc` sigaction WITHOUT `SA_RESTART`, installed only in plain mode) sets
a process-global flag that `CancelToken::is_set` ORs in, so the first
Ctrl+C lands the running turn through exactly the same cooperative
checkpoints as a TUI Esc — bash group-kill included, session saved by the
driver loop as usual. A second Ctrl+C while the flag is still set
force-quits with exit 130 (async-signal-safe `_exit`); the flag is
cleared at each submission together with the token (the F7 invariant), so
the two-press escape hatch re-arms every turn. TUI raw mode never
generates SIGINT — TUI Ctrl+C semantics are untouched.

**Remaining exclusion.** A FULLY stalled TCP stream (no frames arriving
at all) cannot observe the token — ureq timeouts are whole-phase
deadlines, not idle timeouts, and would kill legitimate long streams.
The no-`SA_RESTART` choice lets a blocked raw read return EINTR (and F5
treats a read error under cancel as a graceful stop), but Rust's buffered
readers retry EINTR internally, so this is opportunistic, not guaranteed:
the force-quit paths (TUI double-Ctrl+C arm+confirm; plain second
Ctrl+C) remain the documented escape hatch, both exiting 130.

Also deferred, noted during the port: input queuing while a turn runs
(OpenCode queues prompts; we disable Enter and show a hint), tool output
in `ToolEnd` (OpenCode shows capped bash output; our seam carries only a
title), mouse support, themes, multi-pane layouts.

## Keys

Enter send · ↑/↓ input history · PgUp/PgDn scroll (End of scroll
re-sticks to bottom) · Home/End/←/→/Backspace/Delete edit · Esc
interrupt the running turn · Ctrl+C clear input, or quit when empty;
twice during a turn force-quits · Ctrl+D quit (empty prompt) ·
`exit`/`quit` as a line also quits.

## Offline test strategy (all in `check.sh`, host + container)

1. Fold tests: `AgentEvent` sequences → transcript cells.
2. Frame snapshots via `TestBackend` at several sizes (wrap, scroll,
   resize, busy row, tool forms, footer degradation).
3. Headless seam e2e: real `TuiUi` (threads/channels) + real `Session`
   over `ReplayTransport` fixtures, scripted keys, final-frame asserts.
4. PTY smoke in `check.sh`: real binary under `script(1)`/`podman -t`,
   asserting alt-screen enter/leave and transcript content end-to-end.
