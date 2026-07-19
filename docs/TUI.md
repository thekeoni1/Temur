# TUI (milestone B) — design notes, seam assumptions, known limits

The TUI is a second `Ui` implementation (`src/ui/tui/`) over the unchanged
`AgentEvent` stream; the agent core was not modified for it. Layout is a
behavioral port of OpenCode's session view (header band / scrollback with
sticky bottom / prompt / status row / footer), monochrome-adapted.

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

## KNOWN LIMITATION — no turn interruption (deferred; needs a core seam)

Esc-to-interrupt (OpenCode behavior) requires a cancellation path in
`Session::turn`/`Provider::stream` that v1 deliberately does not have; a
TUI-only milestone could not add it without touching the agent core. The
v1.x consequence: **a hanging or unwanted turn cannot be interrupted, only
force-quit** — Ctrl+C during a turn arms a confirm, a second Ctrl+C
restores the terminal and exits the whole app (code 130). Session history
is lost (no persistence yet, also deferred). Framed for prioritization:
*turn interruption needs a small core seam extension* (a cancel flag the
turn loop checks between stream events and tool calls) — a trade-off to
weigh when tuning the post-v1 milestone order; see ROADMAP.md.

Also deferred, noted during the port: input queuing while a turn runs
(OpenCode queues prompts; we disable Enter and show a hint), tool output
in `ToolEnd` (OpenCode shows capped bash output; our seam carries only a
title), mouse support, themes, multi-pane layouts.

## Keys

Enter send · ↑/↓ input history · PgUp/PgDn scroll (End of scroll
re-sticks to bottom) · Home/End/←/→/Backspace/Delete edit · Ctrl+C clear
input, or quit when empty; twice during a turn force-quits · Ctrl+D quit
(empty prompt) · `exit`/`quit` as a line also quits.

## Offline test strategy (all in `check.sh`, host + container)

1. Fold tests: `AgentEvent` sequences → transcript cells.
2. Frame snapshots via `TestBackend` at several sizes (wrap, scroll,
   resize, busy row, tool forms, footer degradation).
3. Headless seam e2e: real `TuiUi` (threads/channels) + real `Session`
   over `ReplayTransport` fixtures, scripted keys, final-frame asserts.
4. PTY smoke in `check.sh`: real binary under `script(1)`/`podman -t`,
   asserting alt-screen enter/leave and transcript content end-to-end.
