# Using temur day to day

This guide assumes temur is installed (README "Install" and
"Quickstart"). It walks one real interactive session, then the full
command, session, context, and configuration reference, the one-shot
scripting recipes, skills, and the key-isolation model.

> **Capture note.** Every transcript below is from a real run, captured
> 2026-07-28 against a local llama.cpp server (image `server-b10068`)
> serving Qwen3-4B-Instruct-2507 Q4_K_M with the compact prompt profile,
> in a scratch directory `/home/dev/demo` (sections that state their own
> capture setup inline, like "/compact", differ only as stated). Input was piped, where a
> terminal would echo the typed line after `>`; the transcripts show the
> input inline exactly as a terminal session displays it. The startup
> version banner (`temur <version> (model=..., thinking=...)`) is
> omitted so this document does not go stale on version bumps.

## A worked interactive session

Start temur with no arguments. On a terminal you get the TUI (markdown
rendering, Tab completion, a status row; see [TUI.md](TUI.md)); when
stdin or stdout is piped you get the plain line REPL shown here. Both
render the same underlying events, so everything below applies to both.
`--tui` and `--plain` force the choice, with one limit: the TUI needs a
real terminal on both stdin and stdout, so `--tui` against a pipe is a
usage error naming the two alternatives rather than a window that can
never read your input. Use `-p "..."` for piped one-shot input, or
`--plain` for the line REPL.

A small real task, followed by `/status`:

```
> Create a script greet.sh that prints a greeting to the current user, then run it to show it works.
  → write
  ✓ write: /home/dev/demo/greet.sh
  → bash
  ✓ bash: chmod +x greet.sh && ./greet.sh
The script `greet.sh` has been created and successfully executed. It prints a greeting to the current user, "Hello, dev!".
  (turn: 8372 in / 101 out, cache read 8328 write — — session: 8372 in / 101 out, cache read 8328 write —)
> /status
  [!] profile: (none — base config)
  [!] provider: openai-compat · model: qwen3-4b
  [!] thinking: off · max_tokens: 1024 · prompt: compact
  [!] context: ~2872 of 8192 tokens used
  [!] session file: /home/dev/.local/state/temur/sessions/demo-9bc590dd6def5c8d.json · session: (default)
> bye
```

What each kind of line means:

- `> ` is the input prompt. Lines starting with `/` are commands
  (`/help` lists them all); they never reach the model.
- `  → write` announces a tool call starting.
- `  ✓ write: /home/dev/demo/greet.sh` is that tool call finishing;
  the text after the name is the tool's own one-line summary of what it
  did. A failed call shows `✗` instead, and failure is not fatal: the
  error text goes back to the model, which adjusts and retries.
- Unprefixed text is the assistant's reply (the TUI renders it as
  markdown; the plain REPL prints it raw).
- `  (turn: ... session: ...)` closes every turn with token usage:
  this turn's input/output and cache read/write, then the running
  session totals.
- `  [!]` marks notices: `/status` output, warnings (for example
  `[!] context: ~3495 of 4096 tokens used; /compact frees the window
  by summarizing the conversation, or start a new session` when a
  small context window fills up; see "/compact" below), and safety
  stops such as `[!] stopped: two tool calls alternated 3 times in a
  row` (the doom-loop guard; both examples are from real runs).
- A row of dots (`.`) is streamed thinking activity, shown as a
  passive indicator. Only the anthropic provider uses thinking, and it
  is off by default (`/thinking on` flips it for the session).

To leave: `exit`, `quit`, or Ctrl+D (EOF); temur prints `bye`. Ctrl+C
during a turn interrupts the turn, not the program (details in
[TUI.md](TUI.md), "Turn interruption").

## Command reference

Inside a session, any input line starting with `/` is a command: it
never reaches the model or the history (which also means a literal
message starting with `/` cannot be sent):

- `/help` - list commands
- `/status` - profile, provider, model, thinking, prompt profile,
  context use, an estimated session cost when the active profile is
  keyed and priced (see "Cost estimate"), session file
- `/model` - list profiles, then two hint lines saying what a
  non-profile argument does · `/model <name>` - switch profiles
  mid-session · `/model <model-id>` - switch the model WITHIN the
  active provider (profile names win on collision; endpoint,
  credentials, limits, and prompt profile stay; a bad id surfaces as
  the provider's error on the next turn; if the id is absent from the
  last `/models` listing an advisory notice says so, without blocking).
  Exception - the cross-provider hop: a `claude-*` id on a
  non-anthropic provider with an anthropic profile configured switches
  to that profile instead (the exact-model match, else the first
  anthropic profile by name), then applies the id on top when it is
  not the profile's own model; the notice names the profile. An id the
  active provider actually listed in `/models` always switches
  literally, and with no anthropic profile a hint notice explains the
  hop. · `/model <model-id> --save` - the same switch, persisted to
  config.json on success (a surgical edit: your key order and unknown
  fields survive; when a profile is active - including one a hop just
  activated - the save site is that profile's `model` and the notice
  names it) · `/model --save` - persist the currently active model;
  `--save` with a profile name is an error (the startup profile stays
  the hand-edited `profile` key)
- `/models` - list model ids from the active provider (live GET; ids
  feed `/model` Tab completion in the TUI)
- `/clear` - wipe the session; the empty state is persisted immediately,
  so quitting and `--continue` resumes empty
- `/compact` - one model call summarizes the conversation, then the
  session continues from that summary plus the last user-initiated
  exchange kept verbatim (fail-closed: any error, interrupt, or empty
  summary leaves history untouched; the compacted state is persisted
  immediately, like `/clear`)
- `/sessions` - list every saved session, all projects: name (or
  `(default)`), the directory it was recorded in, message count, file
  name, and a title derived from its first prompt; the active session
  is starred
- `/resume <session>` - switch to a saved session by name or file-name
  prefix; the saved history renders into the transcript as backscroll
- `/new <name>` - start a fresh named session for this project (the
  file is created on the first turn)
- `/thinking` · `/thinking on|off` - show or flip adaptive thinking for
  this session (only the anthropic provider uses it)

Under `--mock`/`--capture-sse` the state-mutating commands, and
`/models`, which is a live network GET, report themselves unavailable
to keep replays deterministic.

In the TUI (the default on a terminal; design notes and key bindings in
[TUI.md](TUI.md)), assistant replies render as
markdown (headings, emphasis, lists, quotes, links, and code blocks
behind a dim gutter) in the same monochrome, default-terminal-color
style; the plain REPL prints raw text unchanged. TUI command
ergonomics: `/`-input renders in the cyan accent, the status row shows
a live hint for the command being typed, and Tab cycles completions
in place (command names; profile names and `/models`-cached ids after
`/model`; `/sessions`-cached session keys after `/resume`; `on|off`
after `/thinking`) with BackTab reversing.

### /clear vs /new vs /resume

Every live run saves the conversation after each turn (the "Sessions"
section below has the full model). Three commands manage it; pick by
what you want to keep:

- `/clear` wipes the current session's history in place and persists
  the empty state immediately. Use it when the current thread is done
  or has gone off the rails and you will not want it back. When the
  context advisory starts firing but the thread IS worth keeping,
  `/compact` (next section) preserves a summary instead.
- `/new <name>` leaves the current transcript on disk and starts a
  fresh named session for this project. Use it when switching to a
  different piece of work you may want to return to; the old session
  stays resumable.
- `/resume <session>` switches to a saved session (by name or file-name
  prefix; `/sessions` lists everything, and the same key works at
  startup as `temur --resume <key>`). The saved history renders into
  the transcript as backscroll. Use it to pick an earlier thread back
  up, in this project or another (resuming another project's session
  warns that tools still run in the current directory).

## Sessions

Every live run saves the conversation after every round-trip, under
`$XDG_STATE_HOME/temur/sessions/` (fallback
`~/.local/state/temur/sessions/`; state, not config, because transcripts
carry tool output and grow to megabytes). Each working directory has a
**default session**, plus any number of **named sessions** created with
`/new <name>` (names keep `[A-Za-z0-9._-]` and cap at 32 chars). A plain
start uses the default session; `temur --continue` resumes it.

`/sessions` lists everything saved, across all projects, newest first.
`/resume <key>`, or `temur --resume <key>` at startup, switches to a
saved session: a key is a session name (a name in the current project
wins; a globally-unique name works from anywhere; a duplicated one is an
error listing the candidates) or a file-name prefix, which is how
default sessions are addressed. Resuming renders the saved history into
the transcript as backscroll (prompts, replies, and tool names - tool
output and arguments are not replayed) and redirects saving to the
resumed file. Resuming another project's session warns that tools still
run in the current directory. A failed `/resume` (unknown key,
ambiguous key, unreadable file) changes nothing.

The saved history is provider-neutral, so a session recorded against
one provider resumes against another. Saves are atomic (write, fsync,
rename) and the FORMAT contains no timestamps, so a power cut at any
instant leaves the previous complete file, resumable on a clock-less
device.

The save happens *within* a turn, not only at the end of one. An
agentic turn can run for many minutes across many tool calls, and until
v0.29.0 a hard kill during one lost all of it: the file was written
once, after `turn()` returned. Now the session is written after each
assistant message (before its tools run, which is where a long turn
spends its time) and again before each following request, so a
`SIGKILL` costs at most the single request that was in flight. This
matters most where nobody is watching: 4 of 32 Terminal-Bench cells in
T39 had no session file at all, and they were exactly the cells whose
budget expired, so the runs that most needed inspecting were the ones
with nothing to inspect. A `SIGTERM` handler would not have helped;
`SIGKILL` cannot be trapped. Replay runs (`--mock`) still write
nothing. The `/sessions` listing order (newest first) comes from
filesystem mtimes, which is display-only metadata read at list time: on
a clock-less device every file sorts equal and the listing falls back
to name order, and nothing else depends on it. Past the size cap the
file drops its oldest exchanges, always cutting at a message boundary
that keeps the remainder replayable; the in-memory conversation is
never trimmed. Two processes in one directory don't corrupt anything:
last complete writer wins. To start over, `/new` a fresh name or delete
the file from the sessions dir.

## Context lifecycle

With a `context_window` configured, temur tracks an advisory estimate
of context use (the last response's reported input+output tokens) and
warns once per session when the conversation gets tight: at 80% of the
window, or when the remaining room is smaller than `max_tokens`,
whichever comes first. The advisory names both remedies: `/compact`
summarizes the conversation and continues in a fraction of the window;
a new session starts clean. The same advisory also fires immediately
at `--continue`/`--resume`/`/resume` when the restored session is
already past the threshold.

That estimate is one round-trip behind by nature: it is what the last
response reported, so a large tool result appended since is invisible
to it. The check therefore runs a second time immediately before each
request goes out, adding a rough four-characters-per-token estimate of
everything appended since. It is an average, and dense content defeats
it (G-code measured about 1.2 characters per token in one experiment),
so it catches the ordinary large result rather than every one. The
backstop for the rest is further down: temur also recovers *after* a
server rejects an over-sized request. Either way one crossing produces
exactly one line, never two.

### Auto-compaction for unattended runs

The advisory assumes a reader. One-shot `-p` has none: the estimate
crosses the threshold, temur prints advice nobody will act on, and the
next request is rejected by the server for exceeding the window. That
is how T39's Terminal-Bench cells died on two different machines, and
`auto_compact` is the answer:

```json
"auto_compact": true
```

When it is on and the advisory would fire, the session compacts itself
at the next safe point and carries on with the turn. The default
follows the invocation, because the question is whether anyone is there
to act on the advice:

| Mode | Default | Why |
| --- | --- | --- |
| one-shot `-p` | on | nobody can type `/compact` |
| plain REPL | off | the advisory plus `/compact` already work |
| TUI | off | same |

An explicit `true` or `false` wins in every mode: `true` enables the
same mechanism interactively, `false` restores advisory-only behaviour
in one-shot. It is a base-config key, deliberately not a per-profile
one, because whether an unattended run may spend a summary call to
survive is a property of how temur was invoked and not of which model
answered, so a `/model` switch must not change it.

Auto-compaction keeps a different shape from `/compact`, for a reason
worth stating. `/compact`'s verbatim tail runs back to the last plain
user message, which *mid-turn* is the task prompt itself, so the whole
turn would be tail and the compaction would free nothing. Auto-
compaction instead cuts inside the turn:

```
[ the task prompt, verbatim ] + [ summary of the work so far ] + [ the last 2 round-trips ]
```

The prompt survives byte-identical because in a one-shot run it is the
only statement of the task, and a model handed a paraphrase of its
assignment does the wrong job. The cut always lands on a
`tool_use`/`tool_result` boundary, so no tool call is ever separated
from its result. A turn with fewer than three completed round-trips has
nothing to fold and is left alone. Such a crossing is not reported the
moment it happens, since the very next round-trip may be able to fold
it; if the turn ends and nothing ever folded, the ordinary advisory
prints then, and the once-per-session latch is left open so a later
turn can still compact.

On resume it works differently, and deliberately. When
`--continue`/`--resume` (or `/resume`) restores a session that is
already past the threshold, there is no turn to cut inside yet, so the
whole restored history is what folds and the ordinary `/compact` rule
is used instead. Resume is also the cheapest moment to do it: no
provider cache prefix is warm, so the one-time rebuild `/compact`
normally pays for is not paid at all.

A successful compaction reports what it did in round-trips and bytes:

```
[!] context: ~11942 of 12288 tokens used; compacting automatically
[!] compacted: 9 round-trip(s) summarized, 2 kept, ~48211 -> ~9820 bytes
```

Those byte figures are measured, not promised. Folding a single short
round-trip can cost more than it saves, and the line will say so.

It is bounded at three compactions per turn; a fourth crossing prints
the ordinary advisory and lets the request go out as it would have,
which may still be rejected, and that is the honest outcome. A failed
summary call names the error and continues uncompacted. Compaction
happens between round-trips, never in the middle of one, so a response
whose tool calls are still unanswered is never cut.

### When the server rejects the request anyway

Prediction is not enough on its own. A single capped tool result of
dense content can take one request past the window with no crossing
ever detected, and on the first round-trip of a turn there is nothing
to fold even if it were. So a rejection is treated as recoverable
rather than fatal: when a request comes back as a context-size
rejection, temur recovers once and retries once, and says which it did.

```
[!] context overflow: the server rejected the request; compacting and retrying
[!] compacted: 3 round-trip(s) summarized, 2 kept, ~40118 -> ~9204 bytes
```

```
[!] context overflow: the server rejected the request; truncating the largest tool result and retrying
[!] truncated the largest tool result: 12433 -> 6216 chars
```

The first line is the ordinary fold, taken when auto-compaction is on
and the turn has enough round-trips for it. The second is for the case
a fold cannot reach: the largest tool result in the conversation is cut
to half its size in place, keeping its head and tail with a marker in
the middle saying what happened, so the model can see its own earlier
read got shorter and re-read a narrower range. Only tool results are
ever cut. The task prompt and the model's own messages are never
touched, and a result already under about a thousand characters is left
alone, because it is not what filled the window.

Bounded, like everything else here: at most one recovery per request,
counted against the same three-per-turn limit as auto-compaction, and a
retry that is rejected again propagates rather than looping. Anything
that goes wrong inside the recovery reports the server's original
error, not one of temur's own.

Requests are append-only by design (pinned by a prefix-stability test
suite), which is what makes provider prompt caching effective: the
anthropic provider marks cache breakpoints (system+tools, plus a
moving one at the end of history), and against local llama.cpp the
same append-only shape makes prefix KV reuse work for free (start the
server with `--cache-reuse 256` to keep prompt processing incremental
across turns). `/compact` deliberately invalidates that warm prefix
once, in exchange for a small history from then on; per-turn trimming,
which would invalidate it on every turn, is deliberately absent.

### /compact: summarize and keep going

The advisory above needs a configured `context_window`: with none
there is no estimate to judge, and it never fires. A real sequence
against a local llama.cpp server
(Qwen3-4B, `context_window` 4096, `max_tokens` 512): three verbose
answers crossed 80% and the advisory fired, the session was quit, and
`--continue` re-warned at load, before any turn:

```
  [!] resumed session: 6 messages, ~9423 tokens in / 1099 out
  [!] context: ~3911 of 4096 tokens used; /compact frees the window by summarizing the conversation, or start a new session
>   [!] compacted: 6 message(s) summarized into 2; the next request rebuilds the provider's cached prefix (one-time cost)
```

Where should the `context_window` number come from? For a local
llama.cpp server the truth is the server's own context allocation (its
`-c` flag), not the model card, and temur reads it from the server's
`/props` endpoint: `temur init` writes the detected value into a fresh
local config when the server is up, **startup asks the same question**
when a keyless local selection has no `context_window` configured at
all, and `temur doctor` compares a configured value against the same
source, warning in both directions
(configured larger than the allocation means this advisory fires too
late and requests can fail at the real limit; smaller is safe but
early) and naming the exact line to add when the value is missing.
Non-llama.cpp servers answer nothing useful at `/props` and stay
silent, and doctor NOTEs any profile with no `context_window` at all.

The startup probe is what stops an unconfigured local server from
running the whole session blind, with no advisory, no auto-compaction
and an unscaled tool-output cap. It runs only for an `openai-compat`
selection with no key file and no configured window, never under
`--mock`, and it is the same unauthenticated GET `init` and `doctor`
make. On an answer it says so once and nothing is written to disk:

```
[!] context window 12288 detected from the server (/props); the context advisory, auto-compaction, and the tool-output cap now use it
```

If the detected window also puts the selection below the `"auto"`
prompt-profile threshold, the ordinary profile line follows it. A
configured `context_window` is authoritative and is never probed over,
and a server that is down, or is not llama.cpp, is silent: behaviour is
then exactly what it was before. Adding the explicit `"context_window"`
line to the config is still worth doing, and doctor still says so.

One more thing doctor now checks here: a `max_tokens` larger than the
`context_window` it runs against draws a WARN naming both numbers. That
configuration makes the advisory's second arm (`window - used <
max_tokens`) true from the first response of every session, so temur
recommends `/compact` about a window that is barely touched, and
underneath that every request reserves more completion than the server
can hold. It is a WARN and never a FAIL: it is live-able, and it is
exactly what a hand-written local config falls into when it names a
window but lets the default cap ride along.
On an anthropic profile the truth is the per-model `max_input_tokens`
the models API reports, and the `/models` command already receives it:
after a listing, a configured window larger than the reported value
draws a warning, a smaller one draws a hint (safe, but the advisory
fires earlier than it needs to), and a missing one draws a hint naming
the exact config line. Equal is silent. The listing carries dated ids
only, so a profile on a bare alias like `claude-haiku-4-5` is matched
against listing entries that are the alias plus a date suffix, and the
notice names the dated id it matched so the inference is visible. That
match is made only when it is unambiguous: if several dated entries
disagree about the window, temur says nothing rather than guess.
Doctor never calls the authenticated models API; the hosted check
rides only the `/models` request you make yourself.

`/compact` makes ONE model call (the session's own model and system
prompt, tools omitted) asking for a structured summary: goal, state,
decisions, files touched, next steps. On success the history becomes
that summary plus a verbatim tail, the last user-initiated exchange
(from the last user message that is not a tool result through the end),
so recent work stays byte-exact and a tool call is never split from its
result. The summary rides INSIDE the tail's first user message as a
leading `[conversation summary (compacted)]` block, and the compacted
state is saved immediately, like `/clear`. It is fail-closed: a
provider error, Ctrl+C (works like interrupting a turn), or an empty
summary leaves the history exactly as it was and says so.

Two honest costs, both deliberate. First, the provider's cached prompt
prefix (and a local server's reused KV state) was built on the old
history, so the request after a `/compact` re-processes its now-short
prompt from scratch; that one-time cost is why temur never trims
per-turn, which would pay it on every turn. Resuming is the exception:
at `--continue`/`/resume` nothing is warm yet, so compacting right
after the resume-time advisory throws away nothing. Second, the model
writing the summary is the session's own; a small local model writes a
rougher summary than a hosted one, which the structured headings exist
to keep useful.

Naming note: `/compact` is unrelated to the `"compact"` value of
`prompt_profile` in config.json. That knob picks the SIZE of the tool
prompts and system prompt served to small models (see "Prompt profiles"
below); `/compact` is a command that shrinks the conversation history. A
session can use either, both, or neither.

## Configuration reference

Config lives at `~/.config/temur/config.json`; README "Configure"
shows the minimal keyless starter and `temur init` writes any of the
recipes below for you. The default provider is `anthropic` (model
`claude-sonnet-5`); any API key is read from a file path at startup,
never from env or argv.

The Anthropic template writes a curated profile set over the current
model tiers, every profile reading the same key file, and asks which
profile to start on (default `sonnet`, keeping `claude-sonnet-5` as the
effective default model):

```json
{
  "profiles": {
    "fable":  { "provider": "anthropic", "model": "claude-fable-5",
                "api_key_file": "/home/you/.secrets/temur-anthropic-key",
                "context_window": 1000000,
                "price_input_per_mtok": 10.0, "price_output_per_mtok": 50.0 },
    "haiku":  { "provider": "anthropic", "model": "claude-haiku-4-5",
                "api_key_file": "/home/you/.secrets/temur-anthropic-key",
                "context_window": 200000,
                "price_input_per_mtok": 1.0, "price_output_per_mtok": 5.0 },
    "opus":   { "provider": "anthropic", "model": "claude-opus-5",
                "api_key_file": "/home/you/.secrets/temur-anthropic-key",
                "context_window": 1000000,
                "price_input_per_mtok": 5.0, "price_output_per_mtok": 25.0 },
    "sonnet": { "provider": "anthropic", "model": "claude-sonnet-5",
                "api_key_file": "/home/you/.secrets/temur-anthropic-key",
                "context_window": 1000000,
                "price_input_per_mtok": 2.0, "price_output_per_mtok": 10.0 }
  },
  "profile": "sonnet"
}
```

The baked `context_window` values are per model, not one shared number:
haiku serves 200k of input where the other three serve 1M. They are
knowledge as of 2026-08-04, read once off the authenticated models API,
not detected at init time, because `init` never makes an authenticated
call. Both `/models` and `doctor` check them against the live wire, so
if a tier's real limit moves you will see it there. A config written by
an older version keeps whatever it was written with; nothing rewrites an
existing profile, so re-run `temur init` into a scratch config (or edit
the numbers by hand) if you want the current values.

The baked prices are per model too, USD per million tokens at
Anthropic's standard list rate, knowledge as of 2026-08-19. They feed
the `/status` cost estimate and nothing else; see "Cost estimate" below.
Sonnet's 2.0/10.0 is the standard rate: it launched as an introductory
rate through 2026-08-31, and Anthropic has since recorded it as the
standard price and cancelled the increase to 3.0/15.0 that had been
scheduled for 2026-09-01. Nothing re-checks list prices against
the wire, so treat them the same way as the windows: edit them if they
move, and only the anthropic template bakes any at all.

The hosted OpenAI-compatible templates share one shape and differ only
in endpoint and default model; the xAI one, for instance (OpenAI:
`https://api.openai.com/v1` / `gpt-4o`; Gemini:
`https://generativelanguage.googleapis.com/v1beta/openai` /
`gemini-3.6-flash`):

```json
{
  "provider": "openai-compat",
  "openai_compat": { "base_url": "https://api.x.ai/v1",
                     "model": "grok-4",
                     "api_key_file": "/home/you/.secrets/temur-xai-key" }
}
```

The OpenAI template is the one exception to that shape: it also writes
`"max_tokens": 16384`, because gpt-4o caps completions there and rejects
anything larger, while temur's default is 32000. The others accept the
default and bake nothing.

The OpenAI, Gemini, and Anthropic paths were verified against the real
endpoints on 2026-08-05, with two follow-up legs on 2026-08-10; xAI
was not, for want of a key. Three things are worth knowing before you
hit them:

- **gpt-5 era model ids need one extra field.** They reject
  `max_tokens` and require `max_completion_tokens`. Set
  `max_tokens_parameter` on the profile and temur sends that name
  instead, carrying the same value:

  ```json
  {
    "profiles": {
      "gpt5": {
        "provider": "openai-compat",
        "model": "gpt-5",
        "base_url": "https://api.openai.com/v1",
        "api_key_file": "~/.secrets/temur-openai-key",
        "max_tokens_parameter": "max_completion_tokens"
      }
    }
  }
  ```

  The field is openai-compat only, takes exactly `"max_tokens"` (the
  default) or `"max_completion_tokens"`, and anything else is a
  startup error. No template bakes it, because the OpenAI template
  defaults to `gpt-4o`, which wants the classic name. Leaving it out
  sends exactly the request temur has always sent.

  Live-verified on `gpt-5` on 2026-08-10, including a tool call.
  Without the field, the first turn fails and the symptom you will
  see is the provider's own 400:

  ```
  provider error: api error (HTTP 400) invalid_request_error:
  Unsupported parameter: 'max_tokens' is not supported with this
  model. Use 'max_completion_tokens' instead.
  ```

  (wrapped for the page; temur prints it as one line.) With the field
  set as above, the same prompt completes normally, and the server
  raises no other objection.
- **A hosted profile has no `context_window`,** so the context
  advisory and the context-scaled tool-output caps are off for it and
  `/status` says "window size unknown". `init` never makes an
  authenticated call, so it cannot detect one; set the value by hand
  if you want the advisory.
- **`/status` still reads as a floor on wires that omit usage
  entirely.** A server that reports no usage object contributes
  nothing to the session total, and no amount of arithmetic recovers
  it. Where a server DOES report a `total_tokens` larger than the
  counts it names, temur now folds that difference into the output
  count, which is where an unreported thinking spend belongs and how
  it is priced. That is what closed the old Gemini undercount: it
  bills thinking tokens and counts them in its total while naming
  them nowhere. Servers whose total already equals the sum of its
  parts, OpenAI and llama.cpp among them, are unaffected.
  Live-verified on the streaming path on 2026-08-10: a Gemini turn
  reporting 6498 prompt and 1 completion token against a total of
  6526 was recorded as 28 output tokens, the 27-token gap folded in.
  Before the fix the same turn would have counted 1.

Gemini needed two fixes before its tool calling worked at all, both
shipped: its streaming responses report `finish_reason` "stop" while
attaching real tool calls, and it requires the opaque thought
signature on each call to be echoed back on the following request.
Model ids in its listing all carry a `models/` prefix, and the bare
form works on the wire; note that appearing in that listing is no
guarantee an id is usable, since retired ids stay listed and 404 for
new accounts.

Two more optional keys: `sessions_dir` overrides where saved sessions
live (default: the state dir, see "Sessions" above), and
`session_max_bytes` caps the saved session file's size (default 4 MiB,
minimum 64 KiB).

`temur doctor` verifies a config without side effects, one
PASS/WARN/FAIL line per check: config parse and the same validation as
startup, key files by metadata only (present, non-empty by size, mode
600, WARN on group/other bits, a rotation reminder once a key file is
older than `key_rotate_warn_days`), whether the `temur` on your PATH is
the binary that is running, sessions dir writability, one
TCP-connect/TLS-handshake reachability probe per endpoint, and, for
keyless local endpoints only, whether each configured model and
`context_window` matches what the server itself reports (unauthenticated
GETs; mismatches are WARNs, since servers alias ids). `--no-network`
skips the probes and those checks. Running `temur` with no config at
all prints quickstart pointers instead of a raw credential error.

For the active selection, again on a keyless local endpoint only,
doctor also checks whether the server actually renders your tool
definitions. llama.cpp's `--jinja` mode drops the tools array outright
when the model's chat template has no tool support: HTTP 200, nothing
in the log, nothing in the response, and an agent whose tools simply
never fire. Doctor sends the same one-token completion twice, once
bare and once carrying the tool definitions this session would really
send, and compares the reported prompt tokens:

```
WARN: the server at http://127.0.0.1:8080/v1 appears to drop tool definitions for "gemma-3-4b" (prompt_tokens 10 with and without temur's tools): the chat template has no tool support, so tool calls can silently never happen
```

Identical counts mean the array went nowhere. Differing counts PASS,
naming both. A server that reports no usable token counts is a NOTE,
never a FAIL.

There is a third answer, and it is the reason the probe carries the
real definitions rather than a toy one: a server that answers the bare
completion and then rejects the request the moment tools are attached.

```
WARN: the server at http://127.0.0.1:8080/v1 rejected temur's tool definitions for "local-gguf" (HTTP 400: Unable to generate parser for this template. Error: Object key of unhashable type: Array): every turn that sends tools will fail the same way
```

That is a chat template that cannot render what temur sends, quoted in
the server's own words. It is still a WARN, never a FAIL, but unlike
the drop it will not be silent in use: every turn dies there.

The two extra requests are capped at one generated token each, go to a
local keyless endpoint, and are skipped entirely under `--no-network`.
See OFFLINE.md for which models this hits.

Under the same gate, and capped the same way, doctor also measures the
prompt floor: how much of the context window this selection spends
before the first instruction. That check has an offline half that runs
everywhere, `--no-network` included. See "The prompt floor" above.

The install check answers a question that costs real debugging time
after a rebuild: is the `temur` your shell runs the one you just built?
Doctor compares the first `temur` on your PATH against the binary
running the check, by metadata and bytes only. The same file, or a
byte-identical copy at another path, PASSes. A different build WARNs,
naming both paths, when each was last modified, and which is newer, so
you know whether to reinstall (`scripts/install.sh` installs to
`~/.local/bin`) or to rebuild. It is never a FAIL, because keeping a
second copy is a legitimate setup, and it runs offline like the checks
above. Nothing found on PATH is ever executed: a diagnostic tool that
runs a binary it found by searching directories would be a worse
problem than the one it reports, so the comparison is contents-only,
and doctor never asks the other copy for its version.

### Adding a provider

`temur init --add <local|anthropic|openai|gemini|xai>` merges a
template into your EXISTING config as named profiles, leaving every
other setting, the startup `profile` key included, untouched:
`anthropic` adds the four-profile set above sharing one key file;
`openai`, `gemini`, and `xai` each add one profile named after the
template; `local` adds a keyless `local` profile through the same
base-URL question and model picker as the fresh wizard. A name
collision with any existing profile aborts the whole merge with the
file untouched. Afterwards `/model <name>` switches to the new
profile; set `"profile": "<name>"` in config.json to make it the
startup default.

For keyed templates the wizard (fresh or `--add`) creates the key
file empty (mode 600), then offers a hidden paste prompt: input is
never echoed, Enter skips, and a pasted key is written only to the
key file. A non-empty existing key file is never prompted for or
touched. As a rotation reminder, `temur doctor` WARNs when a key
file has not changed in `key_rotate_warn_days` days (optional config
field; default 90, `0` disables); re-running `temur init --add`
re-prompts after you rotate the key at the provider.

### Named profiles and in-session switching

Define named profiles (nicknames bundling provider + model + endpoint +
key file + limits) and switch between them from inside a session with
`/model <name>`, no quit-and-edit-JSON round trip:

```json
{
  "profiles": {
    "local":  { "provider": "openai-compat", "model": "qwen3-1.7b",
                "max_tokens": 4096, "context_window": 8192 },
    "sonnet": { "provider": "anthropic", "model": "claude-sonnet-5",
                "max_tokens": 32000 }
  },
  "profile": "local"
}
```

Optional `profile` picks the startup profile; omit it and the base
provider/model fields apply exactly as before profiles existed. Profile
fields: `provider` (`"anthropic"` or `"openai-compat"`), `model`
(required), and optional `base_url` (default: the provider's own default
endpoint), `api_key_file` (path to a key file: openai-compat profiles
without one are keyless, anthropic profiles without one fall back to
`APP_SECRET_FILE`), `max_tokens` (default: the global value),
`context_window`, `prompt_profile` (`"auto"`, `"full"`, or `"compact"`
for THIS profile; default: the global `prompt_profile`, which itself
defaults to `"auto"` - `"auto"` resolves against THIS profile's own
`context_window`, so one config can hold a small local server and a
large hosted model and get the right answer for each; switching between
profiles swaps the system prompt and tool descriptions accordingly, and
an explicit `system_prompt` still wins in either profile), and the
price pair `price_input_per_mtok` / `price_output_per_mtok` (see "Cost
estimate"). Every
profile is validated at startup, so `/model` can
only fail on a credential/IO problem, and a failed switch leaves the
session untouched. History continues across a switch (it is stored
provider-neutrally), and each save records whichever provider/model is
active at that moment.

### Prompt profiles

`prompt_profile` picks the SIZE of what temur sends before your first
word: the tool descriptions and the default system prompt. `"full"` is
the stock OpenCode-ported set, sized for Claude-class windows;
`"compact"` is hand-trimmed for small local models. The tool set,
order, and input schemas are identical in both, and an explicit
`system_prompt` in config wins over either default.

It takes three values, and an absent field means `"auto"`, which is the
default:

| Value | Effect |
| --- | --- |
| `"auto"` (or absent) | `compact` when `context_window` is set and below 20480, `full` otherwise (an unconfigured window included) |
| `"full"` | the stock prompts, at any window |
| `"compact"` | the trimmed prompts, at any window |

Anything else is a startup config error naming all three spellings. An
explicit value is never second-guessed; the threshold applies to
`"auto"` alone.

When auto picks compact, temur says so once at startup and nowhere
else:

```
  [!] prompt profile: compact (context_window 12288 is below 20480; set prompt_profile to "full" to override)
```

Nothing is printed when auto picks full. A `/model` switch onto a
profile whose window lands it on compact prints the same line.
`/status` names both the profile and where it came from:

```
thinking: off · max_tokens: 32000 · prompt: compact (auto)
```

`(auto)` means the rule chose it; a bare `prompt: compact` means your
config did.

**Changed in v0.30.0.** Through 0.29.x this field was explicit-only and
temur never inferred a profile from `context_window`. If your config
sets a window below 20480 and no `prompt_profile`, you now get the
compact descriptions where you used to get the full ones; add
`"prompt_profile": "full"` to keep the old behavior. What an explicit
value means is unchanged.

**Changed again in v0.30.1.** The threshold was 16384 in v0.30.0, which
put it below temur's own full-profile floor: a 16384 window got `full`
from the rule and then a `doctor` WARN telling you to make it compact.
20480 is the smallest round window where the full floor stays under
that WARN line (34% measured, 35% estimated). If your window is between
16384 and 20479 and you have no `prompt_profile`, v0.30.1 moves you
from the full descriptions to the compact ones.

### The prompt floor

The floor is what a turn costs before the conversation starts. Measured
live on 2026-08-29 (llama.cpp `server-b10438`, Qwen3-4B-Instruct-2507
Q4_K_M, `context_window` 12288, one request per profile, the reported
input-token count):

| Prompt profile | Floor | Left of a 12288 window |
| --- | --- | --- |
| `full` | 6,991 tokens | ~5,297 |
| `compact` | 2,763 tokens | ~9,525 |

That is the reason auto-selection exists: on the full profile a 12288
window is 57% spent before the model reads the task, and at a
`context_window` of 4096 the floor exceeds the whole window.

Your own number will differ. The floor moves with the length of your
cwd path and the number of installed skills, both of which ride in the
system prompt, so `temur doctor` reports it for the ACTIVE selection
rather than quoting the table above:

```
PASS: prompt floor (estimate): ~2459 tokens; window 12288; 20% of the window
NOTE: that estimate is prompt bytes divided by 4, which is not tokenization: expect it to be off by some percent in either direction. A networked run against a keyless openai-compat server reports a measured figure instead. Reference measurement (2026-08-29, llama.cpp, Qwen3-4B-Instruct-2507): 6,991 tokens for the full profile, 2,763 for the compact one.
NOTE: the prompt floor moves with the length of the cwd path and the number of installed skills, both of which ride in the system prompt
```

The estimate is offline and always runs. On a keyless openai-compat
endpoint with network checks enabled, doctor also asks the server that
will actually serve the session, with one more one-token request
carrying the real system prompt and the real definitions, and reports
`prompt floor (measured): N tokens` instead. A measurement always wins
over the estimate. When the measurement cannot be taken, doctor names
the outcome and falls back rather than letting the estimate stand under
a line that promised a measurement:

```
NOTE: prompt floor measurement inconclusive: the server at http://127.0.0.1:8080/v1 did not answer within 300s (a slow local server may need longer to prefill the system prompt and every definition); the figure below is the estimate
```

That request is the largest prefill a doctor run asks for, and on a
CPU-only local server it can take minutes, so doctor says what it is
waiting for before it waits:

```
NOTE: measuring the prompt floor against the server; on a CPU-only server this is a large prefill (up to 300s)
```

The tools-drop probe that follows announces its own pair the same way.
Neither line appears under `--no-network`, where nothing is sent.

The verdict is on whichever number is in hand: PASS below 40% of the
window, WARN at or above it, never a FAIL.

```
WARN: prompt floor (estimate): ~7240 tokens; window 12288; 58% of the window is spent before the task starts; set prompt_profile to "compact" or raise context_window
```

If the active profile is already compact and the floor is still over
the line, the WARN says so and points at `context_window` instead of at
a knob that is already turned. With no `context_window` configured
there is nothing to divide by, so the line is a NOTE carrying the
number alone.

A WARN at exactly 20480 with no `prompt_profile` set is not a
contradiction: the auto threshold is pinned by a test so temur's own
full-profile floor stays under the WARN line on a baseline install, but
your floor also carries your installed skills, any `system_prompt`
override and your real cwd, none of which the binary controls. A
skills-heavy install can therefore get `full` from the rule and still
be told to make it compact. Setting `"prompt_profile": "compact"` is
the right answer there; the report is measuring what you actually run.

### Cost estimate

Give a profile a price pair and `/status` adds one line:

```
  [!] cost: ~$0.42 this session (estimate, configured list rates)
```

It is an estimate for awareness, not a bill. temur multiplies the token
counts the provider already reported for this session by the list
prices YOU configured, entirely offline; it never asks any provider what
you owe, and no provider offers an API that would answer. Two decimals
once there is a cent to show, four below that, so a small real spend
never renders as `$0.00`.

The two fields are per profile, in the key's billing currency (USD for
the values `temur init` bakes), and per MILLION tokens:

```json
"opus": { "provider": "anthropic", "model": "claude-opus-5",
          "api_key_file": "/home/you/.secrets/temur-anthropic-key",
          "price_input_per_mtok": 5.0, "price_output_per_mtok": 25.0 }
```

At those rates, a session that reported 400k input and 30k output
tokens estimates at 400000/1e6 * 5.0 + 30000/1e6 * 25.0 = $2.00 + $0.75
= `~$2.75`. Set both or neither: half a pair would silently disable the
estimate, so it is a startup error naming both fields, as is a negative
rate.

The line is absent, with no nag, whenever it could not be honest:

- an unpriced profile (nothing to compute; add the two fields),
- a keyless profile (a local server bills nobody; anthropic profiles
  are always keyed, openai-compat ones only with an `api_key_file`),
- a session that has not reported any usage yet.

The anthropic template bakes prices for its four profiles; no other
template bakes any, because no other provider's rates were verified,
and a wrong price is worse than none. The base (non-profile)
configuration has nowhere to carry a price pair, so the estimate is a
profiles feature: put your hosted selection in a profile to get it.

Both error directions, plainly:

- **It can UNDERSTATE.** The estimate can only count tokens the
  provider reported, and some providers do not report all of them.
  Gemini omits thinking tokens from its usage, so its session total is
  a floor and so is any figure derived from it (the same limit noted
  under the hosted-template caveats above). A provider that reports
  nothing at all shows no line, which is honest rather than free.
- **It can OVERSTATE.** On the OpenAI-compatible wire, cached prompt
  tokens are reported as a SUBSET of the prompt tokens already counted,
  and the discount for them is not modeled, so a cache-heavy compat
  session estimates a little high. That is the deliberate direction for
  a spend-awareness number.

Anthropic is the one wire that reports cache tokens as separate counts,
so the estimate does model its published cache multipliers there: cache
reads at 0.1x and cache writes at 1.25x the input rate (the 5-minute
TTL temur uses). Those multipliers, like the baked prices, are
knowledge as of 2026-08-07 and nothing re-checks them.

### The mid-session advisory

`/status` only answers when you think to ask, and the spend worth
knowing about is the spend you did not think to check. So the same
estimate also speaks up on its own, every `$5` it crosses:

```
  [!] cost: this session has crossed $5.00 (estimate: ~$6.12 at configured list rates); set cost_advisory_step_usd to change the step or 0 to disable
```

One turn can be hundreds of provider round-trips, so the check runs
after EVERY response inside a turn, not once per prompt. A jump that
clears several steps at once says so once, at the highest step crossed,
rather than printing a line per step it flew past.

The step is one global field, beside `max_tokens` and the rest:

```json
"cost_advisory_step_usd": 5.0
```

Absent means $5.00. `0` disables the advisory entirely. Negative or
non-finite is a startup error naming the field. It is deliberately not
a per-profile setting: a price is a property of the provider, but a
budget is a property of you, and it should not reset because a `/model`
switch landed on a profile that forgot to repeat it.

The advisory rides the estimate's own gate, so it appears exactly where
the `/status` line appears and nowhere else: a keyless, unpriced, or
local selection never sees it, whatever the step says. It is a notice
like any other, which means in `temur -p` it goes to stderr with the
rest of the chrome and never touches the prose on stdout.

Money already spent never fires. The session starts latched at
whatever its usage already comes to, and re-latches whenever that
number is no longer new news: on `--continue` / `--resume` / `/resume`,
on `/clear` (which zeroes usage, so the next `$5` is new money again),
and on a `/model` switch (rates changed, so past spend is re-measured
against the new ones). Resuming a session that already spent $40 is
silent until it spends its way past $45.

## Picking and keeping a model

Two conveniences (T15) remove the "type a model id blind, keep it by
editing JSON" round trip. Both are real transcripts against a local
llama.cpp server (keyless; the listing GET init and doctor make there
is unauthenticated and never touches key files).

`temur init`'s local template asks where the server lives, then offers
what it actually serves, numbered:

```
Template [1]: Base URL [http://127.0.0.1:8080/v1]: Models on http://127.0.0.1:8080/v1:
  1) /model.gguf
Model (number or id) [/model.gguf]: 
Wrote /tmp/t15-demo/config/temur/config.json
```

A number picks from the listing; free text still works for anything
else. With no server reachable the question falls back to free text
after a one-line note, plus a short baked shortlist of known-good small
models (the full table stays in [OFFLINE.md](OFFLINE.md), section
"Recommended small models").

`temur doctor` now also compares each configured model against the
server's listing, the most likely new-user misconfig. A mismatch is a
WARN, not a FAIL, because servers alias ids (Ollama tags, llama.cpp
path names):

```
WARN: model "qwen3-bogus" is not in the server listing at http://127.0.0.1:8080/v1 (server lists: /model.gguf; advisory only, servers may alias ids)
doctor: 5 pass, 1 warn, 0 fail
```

And a raw-id `/model` switch can persist itself: `--save` writes the
model into config.json after the switch succeeded (a surgical edit;
your key order and any unknown fields survive), so the next start picks
it up:

```
temur 0.5.0 (model=/model.gguf, thinking=false)
>   [!] switched model to qwen3-1.7b (openai-compat · profile settings kept)
  [!] saved model qwen3-1.7b to /tmp/t15-demo/config/temur/config.json
> bye
```

```
temur 0.5.0 (model=qwen3-1.7b, thinking=false)
>   [!] profile: (none — base config)
  [!] provider: openai-compat · model: qwen3-1.7b
```

`/model --save` (no id) persists whatever is currently active. `--save`
with a profile name is a clean error: the startup profile is the
`profile` key in config.json, which stays a hand edit.

## Switching providers by model id (the T16 hop)

Typing a `claude-*` model id while a local (or any non-anthropic)
provider is active used to set that id on the local server and fail on
the next turn - routinely misread as "/model seems broken". Now, when
an anthropic profile is configured (the Anthropic init template writes
a set of four), that input hops to it: a full profile switch, so the
profile's key file, endpoint, and limits apply. Real transcript against
a keyless llama.cpp server, config with a `local` profile plus the
anthropic set:

```
temur 0.5.0 (model=/model.gguf, thinking=false)
>   [!] "claude-opus-5" is an anthropic model - switched to profile "opus" (anthropic, claude-opus-5)
>   [!] profile: opus
  [!] provider: anthropic · model: claude-opus-5
```

An id no anthropic profile carries exactly still hops - to the first
anthropic profile by name - and applies the id on top; `--save` then
persists it to that profile's `model` and the notice names the site.
The same session shows the `/models`-listing advisory on a typo'd raw
id (the switch stands; a wrong id surfaces as the provider's error):

```
temur 0.5.0 (model=/model.gguf, thinking=false)
>   1 model id(s) from the provider:
    /model.gguf
>   [!] switched model to bogus-id (openai-compat · profile settings kept)
  [!] note: "bogus-id" is not in the last /models listing; the switch stands — a wrong id surfaces as the provider's error on the next turn
>   [!] "claude-opus-4-8" looks anthropic - hopped to profile "fable" (its key file and limits apply), model claude-opus-4-8
  [!] saved model claude-opus-4-8 to profile "fable" in /tmp/t16-demo/config/temur/config.json
> bye
```

The restart shows the persisted model in the profile listing, with the
new hint lines after it:

```
temur 0.5.0 (model=/model.gguf, thinking=false)
>   [!] fable — anthropic · claude-opus-4-8
  [!] haiku — anthropic · claude-haiku-4-5
  [!] local — openai-compat · /model.gguf (active)
  [!] opus — anthropic · claude-opus-5
  [!] sonnet — anthropic · claude-sonnet-5
  [!] /model <name> switches profiles; any other argument is a raw model id on the ACTIVE provider
  [!] /models lists what the active provider serves; /model <id> --save persists the switch
```

Two escape hatches keep the hop out of the way when it would be wrong:
an id the active provider itself listed in `/models` always switches
literally (proxies legitimately serve `claude-*` ids over
openai-compat), and with no anthropic profile configured the raw
switch happens as before plus a hint that an anthropic profile enables
the hop.

## One-shot scripting with -p

`temur -p "<prompt>"` runs exactly one full agentic turn (tool calls
included) and exits. The contract that makes it scriptable:

- **stdout carries only the assistant's prose.** All tool and status
  chrome, and any `--continue`/`--resume` backscroll, goes to stderr.
- **The exit code reports the outcome:** 0 for a completed turn, 1 for
  a provider or startup error, 130 when interrupted with Ctrl+C (the
  shell convention for SIGINT).
- Live one-shots save the session exactly like interactive runs, so
  `--continue -p` chains work. The save happens after every round-trip,
  so a killed one-shot still leaves a resumable transcript of the work
  it got through.
- **Auto-compaction is on by default here**, and only here: a one-shot
  run has nobody to act on the context advisory, so it compacts itself
  and continues rather than dying on the next request. Set
  `"auto_compact": false` to restore advisory-only behaviour. See
  [Auto-compaction for unattended runs](#auto-compaction-for-unattended-runs).

Redirect stdout and the chrome stays on your terminal. Real run:

```
$ temur -p "Read greet.sh and describe in one sentence what it does." > answer.txt
  → read
  ✓ read: /home/dev/demo/greet.sh
  (turn: 5530 in / 42 out, cache read 5446 write — — session: 5530 in / 42 out, cache read 5446 write —)
$ cat answer.txt
The `greet.sh` script displays a greeting message that includes the current username.
```

In a shell script, branch on the exit code and keep only stdout:

```sh
#!/bin/sh
summary=$(temur -p "Summarize the TODO comments under src/")
case $? in
  0)   printf '%s\n' "$summary" ;;
  130) echo "interrupted, no summary" >&2; exit 130 ;;
  *)   echo "temur failed" >&2; exit 1 ;;
esac
```

Chaining: `--continue -p` resumes this directory's default session and
runs one more turn on top of it. The resumed backscroll (the `> `
prompt echoes, prior replies, `⚙` tool one-liners, and the
`[!] resumed session` summary) goes to stderr, so stdout is still only
the new turn's answer. Real run, continuing the session from the
previous example:

```
$ temur --continue -p "Run it once more to confirm it still works, and tell me exactly what it printed."
> Read greet.sh and describe in one sentence what it does.
  ⚙ read
The `greet.sh` script displays a greeting message that includes the current username.
  [!] resumed session: 4 messages, ~5530 tokens in / 42 out
  → bash
  ✓ bash: bash /home/dev/demo/greet.sh
The script printed: "Hello, dev!".
  (turn: 5752 in / 36 out, cache read 5707 write — — session: 11282 in / 78 out, cache read 11153 write —)
$ echo $?
0
```

(Everything except the line `The script printed: "Hello, dev!".` is
stderr.) `--resume <key> -p` works the same way against any saved
session. `-p` is mutually exclusive with `--tui` and with the `init`
and `doctor` subcommands.

Interruption, demonstrated for real by sending SIGINT to a running
one-shot after three seconds:

```
$ temur -p "Count the files in this directory." & pid=$!
$ sleep 3; kill -INT $pid; wait $pid; echo "exit=$?"
  → bash
  ✓ bash: find . -type f | wc -l
  [!] turn interrupted
  (turn: 2715 in / 27 out, cache read 2714 write — — session: 2715 in / 27 out, cache read 2714 write —)
exit=130
```

Nothing landed on stdout: an interrupted one-shot never emits a
partial answer as if it were complete.

## Skills

A skill is a reusable instruction file the model loads on demand:
`<skill-dir>/<name>/SKILL.md`, with optional playbooks and assets
beside it (the same layout other CLI agents ship skills in, so
existing skills drop in unmodified).

**Where temur looks.** In order, deduplicated, first hit wins:

1. any `:`-separated directories in the `TEMUR_SKILLS_DIR` environment
   variable (which overrides the `skills_dir` config key, same format);
2. `<cwd>/.temur/skills`, then the legacy `<cwd>/.opencode/skills`;
3. `~/.temur/skills`, then the legacy `~/.opencode/skills`.

The defaults are always searched, even when an override is set. Skills
are enumerated once at startup, so restart the session after installing
one.

**How they surface.** Each installed skill's `name` and `description`
are advertised to the model in an `<available_skills>` block in the
system prompt; when a task matches a description, the model calls the
`skill` tool, which returns the full SKILL.md plus the skill's base
directory (so relative references to playbook or asset files resolve).
If no skills are installed, nothing is advertised. A SKILL.md whose
frontmatter opens with `---` but never closes is skipped at startup
with a warning on stderr.

**A minimal working skill.** The one used for the transcript below:

```
$ cat .temur/skills/commit-style/SKILL.md
---
name: commit-style
description: House rules for writing commit messages in this repo
---

# Commit message style

- Subject line: imperative mood, lower-case, no trailing period, max 50 chars.
- Body: explain WHY, wrapped at 72 columns.
- Reference the issue number when one exists, as "refs #N".
```

`name` (falling back to the directory name) and `description` are the
only frontmatter keys read; the description is what the model sees when
deciding whether to load the skill, so write it as a trigger condition.

Real run, one-shot (stdout was exactly the last line; the rest is
stderr chrome):

```
$ temur -p "Load the commit-style skill, then draft a commit message for adding greet.sh that follows it."
  → skill
  ✓ skill: skill: commit-style
  → write
  ✓ write: /home/dev/demo/CHANGELOG.md
  (turn: 8511 in / 59 out, cache read 8344 write — — session: 8511 in / 59 out, cache read 8344 write —)
add greet.sh script
```

The model loaded the skill and the answer follows its rules
(imperative, lower-case, no trailing period). It also chose to write a
CHANGELOG.md on its own, which is a fair reminder that instructions in
a skill shape but do not fence a turn: state what you do NOT want in
the prompt or the skill.

### Skills too large for one tool result

A tool result is capped (see "The weak-model floor" below: 30,000
characters, or less when `context_window` is set). A skill bigger than
that used to be middle-elided like any other oversized output, which
loses the middle of a document the model asked for by name and then
advises it to "narrow the command, e.g. grep or head/tail", which is
advice about a shell pipeline.

Such a skill now comes back as a section index instead. This is the
tool's verbatim output for a 48,427-character SKILL.md, produced by
running the tool over a generated fixture rather than captured from a
live model session (unlike the transcripts elsewhere in this guide,
which are real runs):

```
<skill_index name="widget-cli">
This skill is 48365 chars, over this session's 30000-char tool output limit, so it is returned as a section index instead of being cut off in the middle. Nothing is summarized and nothing is omitted: every section listed below is available in full. Fetch one with {"name": "widget-cli", "section": "<number or heading>"}, using either the number or the heading text.

Drive the widget CLI with these instructions.

Sections:
1. ## Authentication (16597 chars)
2. ### Token file layout (8294 chars)
3. ### Rotating a token (8233 chars)
4. ## Deploying (15611 chars)
5. ### Staging (7684 chars)
6. ### Production (7867 chars)
7. ## Troubleshooting (16110 chars)
8. ### Common errors (8050 chars)
9. ### Getting logs (7988 chars)
</skill_index>
```

That index is 773 characters, 1.6% of the file it describes. A
follow-up call with `{"name": "widget-cli", "section": 5}` returns
Staging's 7,684 characters in full. Numbers and heading text both work,
matching
ignores case, surrounding whitespace, and a leading `#` run, and
section extents are hierarchical: asking for `## Deploying` brings its
`### Staging` and `### Production` subsections with it, so a fetch
never ends mid-thought. When two sections share a heading, the first is
returned along with the numbers that reach the others.

**Nothing is cached, and the index cannot go stale.** The index is a
pure function of the file's bytes, recomputed on every call. Edit a
SKILL.md and the next call describes the edited file, because nothing
from the previous call was kept: there is no stored index, no
invalidation rule, and therefore no way for the two to disagree. This
is why the feature adds no config keys and no session state.

**What actually does the work.** The tool also minifies a SKILL.md
before returning it: a frontmatter block holding only `name:` and
`description:` is dropped (the model already has both from
`<available_skills>`), trailing whitespace goes, and blank runs
collapse, all of it outside fenced code, which is copied byte for byte
because whitespace is semantic in a heredoc or in Python. Be clear
about the scale of that: measured on this repo's own markdown it saves
**0.0%**, because tidy files have nothing to remove; on a SKILL.md with
frontmatter and loose spacing it saved 2.2%; on the 48k skill above it
removed 62 characters, 0.1%. Minification is a rounding error, and it
is kept only because it is free and lossless. The section index is the
mechanism: 48,427 characters become a 773-character index plus exactly
the sections the task asks for.

**A skill names its directory only when it has one worth naming.**
Every mode used to open with `Base directory for this skill: <path>`.
Watching three local models work an over-cap skill showed that line
doing harm: one went to grep the directory instead of asking for a
section and gave up, and another answered correctly from section 5 and
then wrote its answer into the skill directory rather than the working
directory. It is now emitted only when the skill's directory holds at
least one entry besides its SKILL.md, which is exactly when the path
points at something (a `playbooks/` directory, a template, a script).
The fixture above is a lone SKILL.md, which is why it names no path.

Two cases deliberately keep the old behavior, because an index would
not help: a skill with no headings at all, and one whose prose before
the first heading already exceeds the cap. Both are returned whole and
truncated centrally, now with advice to fetch a section rather than to
run grep.

## The weak-model floor (T19)

Three behaviors keep small local models productive; all of them are
also active on hosted models.

**Tool output keeps both ends.** A tool result larger than the
per-result cap is elided in the MIDDLE, not cut at the end, so build
errors and log tails survive. The marker between the kept halves
reads:

```
(output truncated: showing the first 4096 and last 4096 of 31532 chars; narrow the command, e.g. grep or head/tail, to see the elided middle)
```

The cap scales to the model: with a configured `context_window` it is
that many chars, clamped to 4,000..30,000 (derivation: a quarter of
the window in tokens, at roughly 4 chars per token). No configured
window keeps the 30,000-char cap. The cap follows `/model` switches.

**write is read-first.** Overwriting an existing file the session has
not seen fails with:

```
<path> exists but has not been read in this session. Read it first, or use edit for targeted changes.
```

Reading the file, editing it, or having successfully written it
earlier in the session all count as "seen". New files are unaffected.
`--continue` and `--resume` deliberately start with an empty read
set: the file may have changed on disk while temur was away, so a
resumed session must re-read before overwriting.

**A write that destroys content says so.** The guard above is about
files the session has not seen; it says nothing about a file the model
read a moment ago and then overwrote with something shorter, which is
a real thing weak models do (one read three files, then replaced the
30-byte file holding the answer with an 8-byte one and reported
success). Any successful write over a non-empty file now names what is
gone:

```
Overwrote /work/beta.txt (8 bytes, replaced 30 bytes of prior content)
```

Always, with no smallness threshold, and never for a new or previously
empty file. It is a fact in the result the model has to read past, not
a permission check: `write` still replaces exactly what it is told to.

**Prose tool calls are recovered.** When a model writes its tool call
as plain text instead of using the tool interface, and that text is
one unambiguous call (a single `<tool_call>` block or the whole
message as a JSON object, parsing losslessly, naming a real tool),
temur executes it and feeds the result back as plain text, announcing
it with a notice:

```
  [!] prose-call recovery: executed the write tool call the model wrote as plain text
```

Ambiguous or truncated shapes are never executed; they get the
corrective nudge instead. Set `"prose_tool_calls": false` in
config.json to turn recovery off and restore nudge-only behavior.

A sentence of preamble before a fenced call is one of those shapes:
`I'll create the file now.` followed by a fenced JSON object is not
executed, deliberately, because "the whole message is the call" is
what makes a prose call unambiguous. It used to get nothing at all,
neither execution nor nudge, so the turn simply ended in silence; a
model that narrates before it calls (Qwen2.5-Coder-1.5B does) lost
those calls without a trace. The nudge now fires there, so the model
gets a retry prompt and one more chance at the tool interface. A bare
JSON object mid-prose with no fence around it stays silent on purpose:
prose that quotes a call shape while discussing a plan is common, and
the fence is the only cheap evidence the model meant it as a call.

**A prose call is executed once, not once per resend.** Recovery
executes a call the model wrote as text, and a model can write the
same call again, and again. One of them wrote a single fenced `write`
about sixty consecutive times; each resend was a fresh successful
execution, so nothing stopped it and the turn ran until the context
window overflowed. A resend that is byte-identical to the call just
dispatched is now answered instead of run:

```
  [!] prose-call recovery: the write call repeated verbatim; not executed again
```

The model is told the call was already made and its result is above.
Any change of tool name or argument resets this, so a model making
progress never notices it, and the answers are capped like every other
nudge, so a model that will not move on ends its turn. Structured tool
calls have had their own doom-loop guard since M2 and are unaffected.

**A call to a tool that does not exist gets named.** A fenced call to,
say, `delete` used to match nothing, because both the executor and the
nudge require a REGISTERED tool name, and the turn ended in silence
three seconds in. temur now says which tool does not exist and lists
the ones that do, from the live registry:

```
  [!] the model called a tool that does not exist ("delete"); listed the available tools
```

It never executes anything, it is capped like the other nudges, and it
requires both a fence and an arguments key, so a `{"name": ...}`
package.json fragment in a code block still says nothing.

**A turn that promises work and then stops gets one nudge.** A model
that ends its turn with "Please wait while I analyze it" and makes no
tool call has stopped without starting: nothing runs between turns, so
the promise never resolves and you wait on a model that is no longer
doing anything.

```
  [!] the model promised work without calling a tool; asked it to act or answer
```

The check is narrow on purpose. It fires only when the turn made ZERO
tool calls anywhere, and only when one of a few fixed phrases ("please
wait", "one moment", "I will now", and a handful more) lands in the
LAST part of the message. That last-part rule is what separates "I will
now summarize:" followed by an actual summary, which is a finished
answer, from the same words as the final thing written, which is a
turn that stalled. A genuine answer that happens to end on one of those
phrases costs one extra request and nothing more, since the nudge is
capped like every other one.

**Tool calls that keep re-fetching what you already have get stopped.**
The guards above are narrow on purpose, and a model can slip between
all of them by ROTATING: call A, then B, then C, then A again, forever.
No two consecutive calls are identical, no two alternate, and the turn
runs until the context window ends it. One archived run did that for 77
calls and 440,983 input tokens.

Counting repeats would be the wrong fix, since a model editing ten
files really does call the same few tools over and over. What temur
counts instead is FUTILE calls: a call that repeats an earlier call
from the same turn and gets back a byte-identical result. Nothing
changed between the two, so the second one learned nothing. At six of
those the model is told once, in the tool results themselves, that what
it is re-fetching is already in front of it:

```
  [!] 6 tool calls this turn repeated earlier calls with unchanged results; asked the model to use what it already has
```

At eighteen the turn ends:

```
  [!] stopped: 18 tool calls this turn repeated earlier calls with unchanged results
```

Rereading a file you just wrote is never futile, because the result
changed. Neither is a call with different arguments, however similar it
looks. A failing call counts exactly like a succeeding one, since an
identical error message is just as uninformative the second time. The
honest false positive is the opposite case: if you ask a model to POLL
for something outside temur, waiting on a file another process writes
or a server coming up, an unchanged answer is the point. That is why
six calls buy a notice and not a stop, and why the gap to eighteen is
as wide as it is.

**An empty `workdir` means "not specified".** A model that filled
bash's optional `workdir` in with `""` used to get `failed to spawn
shell: No such file or directory (os error 2)`, and then parroted that
error text back into its next call's arguments. Empty or whitespace
now falls back to the working directory; a workdir that names a real
but missing path still fails, loudly.

**Binary refusals suggest the right tool.** `read` refuses binary
files, and now points at a remedy per type instead of one generic
hint: `pdftotext` for a PDF, `unzip -l` for an archive, `zcat` for a
gzip, and "ask the user to describe it" for an image, since temur
cannot see images. Unknown binary types keep the general suggestion to
inspect with `file`, `unzip -l` or `strings`.

## Key isolation

Tools run in the same process, as the same user, as temur itself, so
file modes alone cannot keep the model away from API keys: anything the
key-owning user can read, a shell command could too. Three layers close
that hole, on by default whenever any key file is configured:

- **File guard** (read, write, edit, glob, grep). Every configured
  `api_key_file` (the active selection and every named profile) plus the
  `APP_SECRET_FILE` path is protected. A tool path is denied when it
  resolves to a protected file (symlinks and not-yet-existing write
  targets are canonicalized first), when it lies under a protected
  file's parent directory (a secrets directory holds sibling keys), or
  when it shares the file's device and inode identity (hardlinks,
  renames). grep never reads a protected file, glob never lists one,
  and writes are denied too: overwriting a key is destruction and a
  poisoning vector.
- **bash sandbox.** With keys configured, every bash command runs in an
  unprivileged user namespace plus a private mount namespace where each
  existing key file is bind-masked with `/dev/null`: inside the shell
  the key path reads as empty and writes to it are discarded, while the
  host file stays untouched. On kernels without unprivileged user
  namespaces, an interactive session (the TUI, or the plain REPL on a
  real terminal) asks you to approve each bash command before running
  it unsandboxed, showing the exact command; the default answer is no,
  and nothing is remembered between commands. Non-interactive runs
  (one-shot `-p`, piped stdin) refuse to run bash instead. Setting
  `allow_bash_without_key_sandbox` to `true` in `config.json` accepts
  running bash unsandboxed WITHOUT asking, for non-interactive use;
  that is a real risk (an unsandboxed shell can read anything you can),
  the other layers still apply, and a working sandbox is always used
  when available, silencing both the ask and the override.
- **Redaction.** The ACTIVE provider's key, the one credential temur
  has actually read, is scrubbed from every tool result (successes and
  errors, before output truncation), so even an unexpected leak path
  cannot echo it back verbatim.

The invariant: a keyless config behaves byte-identically to earlier
releases. No guard, no namespace, no probe, no redaction.

Honest limits: the identity check knows a key's identity only while the
file exists at its configured path, so a hardlink made beforehand
escapes it if the key file itself is later removed; redaction covers
the active key only (inactive profiles' keys are never read, so there
is nothing to redact them with); a masked write inside the bash sandbox
is discarded silently rather than reported; and the parent-directory
rule means a key file placed in a broad directory (a home directory, a
project root) blocks tool access to that entire directory. Keep key
files in their own directory, as `temur init` sets up.

`temur doctor` reports the guard count and the sandbox availability,
and warns when bash would need approval or refuse.

## Bash approval mode (T21)

With key files configured, bash normally runs inside the key sandbox
("Key isolation" above). On a kernel that denies unprivileged user
namespaces (locked-down containers and playgrounds, commonly), the
sandbox cannot start, and an interactive session asks you about each
bash command instead of refusing. The prompt shows the exact command;
`y` runs that one command unsandboxed, anything else denies it. A
denial goes back to the model as an ordinary tool error, so the turn
continues and the model can adapt. Nothing is remembered: the next
command asks again.

A real transcript (plain REPL inside a container whose seccomp policy
denies `unshare`, keyed profile with a placeholder key file, local
llama.cpp serving Qwen3-4B):

```
> Use the bash tool to run exactly this command: echo live-approved > /smoke/home/live-marker.txt
  → bash
  [?] bash approval needed: the key sandbox is unavailable on this host,
      so this command would run with NO key isolation:
        echo live-approved > /smoke/home/live-marker.txt
      run it? [y/N]   ✓ bash: echo live-approved > /smoke/home/live-marker.txt
The command `echo live-approved > /smoke/home/live-marker.txt` was executed successfully, and the file `/smoke/home/live-marker.txt` has been created or updated with the content "live-approved". If you need further actions or verification, let me know!
>   → bash
  [?] bash approval needed: the key sandbox is unavailable on this host,
      so this command would run with NO key isolation:
        echo live-denied > /smoke/home/deny-marker.txt
      run it? [y/N]   ✗ bash: bash
The command to write "live-denied" to `/smoke/home/deny-marker.txt` was not executed, as the user declined to run it. Let me know if you'd like to proceed with any other actions!
```

(The `y` and `n` answers were typed at the `[y/N]` prompts; in the
raw pty capture their echo lands with the piped input block rather
than inline, so they do not appear beside the prompts above.) In the
TUI the same
question appears in the input area with the command wrapped below it,
answered with a single `y`, `n`, or Esc keypress.

The rules, precisely:

- A working sandbox always wins: no prompt, ever, when the sandbox
  runs. Keyless configs never prompt either (there is nothing to
  guard).
- Only interactive sessions ask: the TUI, and the plain REPL when
  stdin and stdout are a real terminal. One-shot `-p` and piped runs
  never ask; with keys guarded and no sandbox they refuse bash, and
  the refusal names both this mode and the config override.
- `allow_bash_without_key_sandbox: true` silences the ask entirely
  and runs bash unsandboxed without asking; it exists for
  non-interactive use on sandbox-less hosts and is a real risk. See
  "Untrusted hosts in practice" below for safer patterns
  (spend-capped throwaway keys, a LiteLLM-style relay).

## Untrusted hosts in practice

Ephemeral playgrounds, throwaway VMs, and shared machines deserve more
suspicion than your own workstation: anything that reaches the host
root user, a snapshotting hypervisor, or another user with your file
access can read whatever key you place there, and temur's key isolation
only guards against the MODEL, not against the host.

- **Never place a primary key on a host you do not control.** Use a
  dedicated key with a spend cap, rotate it on a schedule, and revoke
  it when the machine goes away. `temur doctor` warns when a key file
  has not been rotated in `key_rotate_warn_days` (default 90).
- **The durable pattern is a relay you control.** Run a small
  OpenAI-compatible proxy (LiteLLM is the common choice) on a machine
  you trust, holding the real provider key. Point the playground
  profile's `base_url` at the relay and give the playground only a
  revocable virtual key with its own budget. The existing
  `openai-compat` provider and per-profile `base_url` support this
  unchanged; the untrusted host never sees the real credential, and
  killing the virtual key ends its access without touching anything
  else.
- **Locked-down kernels.** Playground containers often deny
  unprivileged user namespaces, so the bash key sandbox cannot start.
  Interactive sessions then ask per-command approval (see "Bash
  approval mode" above); for non-interactive use on such a host,
  either accept `allow_bash_without_key_sandbox` (with a throwaway
  key only) or leave bash refusing and rely on the other tools.
- **Paste carefully.** `temur init` never accepts a key at the file
  PATH question; a key-shaped answer there is dropped with a warning
  to rotate, because the value reached the terminal. Keys go in only
  at the hidden prompt, or into the key file with your editor.

## Where the other guides are

- TUI design, markdown rendering, key bindings, turn interruption:
  [TUI.md](TUI.md).
- Local/offline model serving (llama.cpp, Ollama, LM Studio, WSL2
  topology), recommended small models, the compact prompt profile:
  [OFFLINE.md](OFFLINE.md).
- Install, quickstart, and the starter config: the README.
