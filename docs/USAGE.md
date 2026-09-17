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
> version banner (`temur <version> (model=..., thinking=...)`, followed
> for an openai-compat profile by its endpoint label) is
> omitted so this document does not go stale on version bumps.
>
> These transcripts predate the approval default. They were captured
> before mutating tool calls started asking first, so no `write`, `edit`
> or `bash` call in them shows an approval step. In an interactive
> session today each of those calls is preceded by one approval
> exchange, shown once under "Approval mode" below; nothing else about
> the transcripts changes. The `-p` examples differ by more than a
> prompt: a one-shot run refuses mutating calls outright unless
> `--allow-mutations` is passed, so reproducing the `-p` transcripts
> that call bash needs that flag. See "Approval mode" for both rules.

## A worked interactive session

Start temur with no arguments. On a terminal you get the TUI (markdown
rendering, Tab completion, a status row; see [TUI.md](TUI.md)); when
stdin or stdout is piped you get the plain line REPL shown here. Both
render the same underlying events, so everything below applies to both.
`--tui` and `--plain` force the choice, with one limit: the TUI needs a
real terminal on both stdin and stdout, so `--tui` against a pipe is a
usage error naming the two alternatives. Use `-p "..."` for piped
one-shot input, or `--plain` for the line REPL.

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
during a turn interrupts the turn and leaves the program running
(details in [TUI.md](TUI.md), "Turn interruption").

### Pasting, interrupting, and the way out

A paste is one prompt. A paste of any length arrives in the input
line as text, each newline drawn as a dim return glyph, and nothing is
sent until you press Enter, which submits the whole block as a SINGLE
prompt with its line breaks intact. Only trailing whitespace is
trimmed, so pasted indentation survives; a block that is nothing but
whitespace is not sent. A multi-line paste that begins with `/` is
sent as a prompt. Only a single-line input starting with `/` is a
command.

The input line is not a text editor. It shows a multi-line paste on
one row with return glyphs where the breaks are, and you can move
through it and delete from it, but there is no cursor movement between
lines and no editing of a block as a block.

Interrupting drops whatever was queued behind it. Esc during a turn
interrupts it, and Ctrl+C during a turn does too. If input was still
arriving when you interrupt (a long paste still draining, say), that
input is DISCARDED: it does not reach the input line and it does not
start another turn once the interrupted one ends. Delivering it would
start the next turn the moment the current one stopped, and the
interrupt would read as if it did nothing.

A file search can be interrupted, and bounds itself. `glob` and
`grep` check for an interrupt on every entry they visit, so Esc stops a
search over a large tree instead of waiting for it to finish. Each walk
is also bounded on its own: after 200,000 entries visited or ten
seconds, whichever comes first, the search returns what it found so far
followed by one line saying it stopped and to narrow the path or
pattern. The limits apply to the WALK. A search that completes is
unaffected, and the existing caps on how many results are shown are
unchanged. Both bounds exist because a working directory on a mounted
Windows drive can be hundreds of thousands of entries deep and minutes
slow. A `glob` pattern that is a comma-separated list of names, such as
`alpha.txt,beta.txt`, is read as alternatives, and the output ends with
a line saying how many.

Double Ctrl+C force-quits. Two Ctrl+C presses within two seconds
during a turn quit the program, whatever else arrived between them.
That is the escape hatch when a turn will not stop; it exits 130.

## Command reference

Inside a session, any input line starting with `/` is a command: it
never reaches the model or the history (which also means a literal
message starting with `/` cannot be sent):

- `/help` - list commands
- `/status` - profile, provider, model, the endpoint of an openai-compat
  profile (`local 127.0.0.1:8080` or `hosted api.example.com`), thinking,
  prompt profile, context use, an estimated session cost when the active
  profile is
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
  active provider listed in `/models` always switches
  literally, and with no anthropic profile a hint notice explains the
  hop. · `/model <model-id> --save` - the same switch, persisted to
  config.json on success (a surgical edit: your key order and unknown
  fields survive; when a profile is active - including one a hop just
  activated - the save site is that profile's `model` and the notice
  names it) · `/model --save` - persist the currently active model;
  `/model <profile> --save` - switch to the profile, then write it as
  the startup `profile` key in config.json (the same surgical edit,
  made only when the switch succeeds)
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
`~/.local/state/temur/sessions/`). Sessions live under the state
directory because transcripts carry tool output and grow to megabytes.
Each working directory has a
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

When a turn ends on a provider error, the file also records that error
under `errors`: the message as the screen showed it (a registered API key
already replaced with `[redacted]`), the model, and the history length at
the time. A stored message longer than 1,000 characters is cut there and
marked `(truncated)`; what the screen showed is never cut. It keeps the
most recent 50. `/clear` and `/new` drop them with
the history they describe, and the key is absent from a session that has
had no errors.

The save happens *within* a turn as well as at the end of one. An
agentic turn can run for many minutes across many tool calls, so the
session is written after each assistant message (before its tools run,
which is where a long turn spends its time) and again before each
following request; a `SIGKILL` costs at most the single request that
was in flight. Until v0.29.0 the file was written once, after the turn
returned, and 4 of 32 Terminal-Bench cells in T39 whose budget expired
left no session file at all. Replay runs (`--mock`) still write
nothing. The `/sessions` listing order comes from filesystem mtimes,
read at list time and used for nothing else: on a clock-less device
every file sorts equal and the listing falls back to name order. Past
the size cap the
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

That estimate is one round-trip behind: it is what the last response
reported, so a large tool result appended since is invisible to it.
The check therefore runs a second time immediately before each request
goes out, adding a rough four-characters-per-token estimate of
everything appended since. Dense content defeats that average (G-code
measured about 1.2 characters per token in one experiment), so it
catches the ordinary large result rather than every one. The backstop
for the rest is further down: temur also recovers *after* a server
rejects an over-sized request. One crossing produces exactly one line.

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
in one-shot. It is a base-config key: whether an unattended run may
spend a summary call to survive depends on how temur was invoked, so a
`/model` switch must not change it.

Auto-compaction keeps a different shape from `/compact`. `/compact`'s
verbatim tail runs back to the last plain
user message, which *mid-turn* is the task prompt itself, so the whole
turn would be tail and the compaction would free nothing. Auto-
compaction instead cuts inside the turn:

```
[ the task prompt, verbatim ] + [ summary of the work so far ] + [ the last 2 round-trips ]
```

The prompt survives byte-identical because in a one-shot run it is the
only statement of the task, and a model handed a paraphrase of its
assignment does the wrong job. The cut always falls on a
`tool_use`/`tool_result` boundary, so no tool call is ever separated
from its result. A turn with fewer than three completed round-trips has
nothing to fold and is left alone. Such a crossing is not reported the
moment it happens, since the next round-trip may fold it; if the turn
ends and nothing ever folded, the ordinary advisory
prints then, and the once-per-session latch is left open so a later
turn can still compact.

On resume it works differently. When
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

Those byte figures are measured. Folding a single short round-trip
can cost more than it saves, and the line will say so.

Both lines print together, immediately before and after the fold they
describe.

It is bounded at three compactions per turn; a fourth crossing prints
the ordinary advisory and lets the request go out as it would have,
which may still be rejected. A failed summary call names the error and
continues uncompacted. Compaction
happens between round-trips, never in the middle of one, so a response
whose tool calls are still unanswered is never cut.

### When the server rejects the request anyway

Prediction is not enough on its own. A single capped tool result of
dense content can take one request past the window with no crossing
ever detected, and on the first round-trip of a turn there is nothing
to fold even if it were. So a rejection is treated as recoverable:
when a request comes back as a context-size rejection, temur recovers
once and retries once, and says which it did.

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

The fold is tried first, because a fold that works is cheaper than
cutting a result and loses nothing. But a fold only counts as a
recovery if it freed space. When a compaction has already
taken the turn, the one round-trip left to fold can be summarized for
no saving at all, and the thing that filled the window is sitting in
the round-trips kept verbatim, where a fold cannot reach it. So a fold
that frees less than a sixteenth of the conversation is not treated as
a recovery: it says so and cuts the largest tool result as well.

```
[!] context overflow: the server rejected the request; compacting and retrying
[!] compacted: 1 round-trip(s) summarized, 2 kept, ~29756 -> ~29851 bytes
[!] context overflow: the compaction freed too little; truncating the largest tool result as well
[!] context overflow: the server rejected the request; truncating the largest tool result and retrying
[!] truncated the largest tool result: 12433 -> 6459 chars
```

That is still ONE recovery and one retry: both things happen before the
single retry goes out.

Bounded, like everything else here: at most one recovery per request,
counted against the same three-per-turn limit as auto-compaction, and a
retry that is rejected again propagates rather than looping. If anything
goes wrong inside the recovery, the error reported is the server's
original one.

Requests are append-only by design (pinned by a prefix-stability test
suite), which is what makes provider prompt caching effective: the
anthropic provider marks cache breakpoints (system+tools, plus a
moving one at the end of history), and against local llama.cpp the
same append-only shape makes prefix KV reuse work for free (start the
server with `--cache-reuse 256` to keep prompt processing incremental
across turns). `/compact` invalidates that warm prefix once, in
exchange for a small history from then on; per-turn trimming would
invalidate it on every turn, so there is none.

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
`-c` flag), and temur reads it from the server's `/props` endpoint:
`temur init` writes the detected value into a fresh local config when
the server is up, startup asks the same question when a keyless local
selection has no `context_window` configured at
all, and `temur doctor` compares a configured value against the same
source, warning in both directions
(configured larger than the allocation means this advisory fires too
late and requests can fail at the real limit; smaller is safe but
early) and naming the exact line to add when the value is missing.
Non-llama.cpp servers answer nothing useful at `/props` and stay
silent, and doctor NOTEs any profile with no `context_window` at all.

The startup probe runs only for an `openai-compat` selection with no
key file and no configured window, never under `--mock`, and is the
same unauthenticated GET `init` and `doctor` make. On an answer it
says so once and writes nothing to disk:

```
[!] context window 12288 detected from the server (/v1/models); the context advisory, auto-compaction, and the tool-output cap now use it
```

If the detected window also puts the selection below the `"auto"`
prompt-profile threshold, the ordinary profile line follows it. A
configured `context_window` is never probed over, and a server that is
down, or is not llama.cpp, is silent. Doctor still recommends adding
the explicit `"context_window"` line to the config.

Doctor also checks for a `max_tokens` larger than the `context_window`
it runs against, which draws a WARN naming both numbers. That
configuration makes the advisory's second arm (`window - used <
max_tokens`) true from the first response of every session, so temur
recommends `/compact` about a window that is barely touched, and
underneath that every request reserves more completion than the server
can hold. It is a WARN, never a FAIL: the config runs, and it is what
a hand-written local config falls into when it names a window but
keeps the default cap.
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
disagree about the window, temur says nothing.
Doctor sends one authenticated listing per keyed endpoint, and none
when the key file is empty. That listing carries model ids only, so
the window comparison above still rides only the `/models` request you
make yourself.

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

Two costs. First, the request after a `/compact` re-processes its
now-short prompt from scratch, because the provider's cached prefix
(and a local server's reused KV state) was built on the old history.
Resuming is the exception:
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

Two safety keys are documented beside the behaviour they govern:
`approve_mutations` under "Approval mode", and
`allow_bash_without_key_sandbox` under "Bash approval mode".

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

The baked `context_window` values are per model: haiku serves 200k of
input where the other three serve 1M. They are knowledge as of
2026-08-04, read once off the authenticated models API; `init` never
makes an authenticated call, so it does not detect them. `/models`
checks them against the live wire, so if a tier's real limit moves you
will see it there; doctor sends one authenticated listing per keyed
endpoint, and none when the key file is empty, but that listing carries
model ids only, so a moved limit does not show up there. Nothing
rewrites an existing profile, so a config written by an older version
keeps its values; edit them by hand, or re-run `temur init` into a
scratch config, for the current ones.

The baked prices are per model too, USD per million tokens at
Anthropic's standard list rate, knowledge as of 2026-08-19. They feed
the `/status` cost estimate and nothing else; see "Cost estimate" below.
Sonnet's 2.0/10.0 was announced as an introductory rate through
2026-08-31; Anthropic has since made it the standard price and
cancelled the increase to 3.0/15.0 scheduled for 2026-09-01. Nothing
re-checks list prices, so edit them if they move. Only the anthropic
template bakes any.

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

The OpenAI template is the one exception on `max_tokens`: it also writes
`"max_tokens": 16384`, because gpt-4o caps completions there and rejects
anything larger, while temur's default is 32000. The others accept the
default.

The Gemini template bakes one thing of its own, the served context window,
because `gemini-3.6-flash` publishes a single figure for it:

```json
{
  "provider": "openai-compat",
  "openai_compat": { "base_url": "https://generativelanguage.googleapis.com/v1beta/openai",
                     "model": "gemini-3.6-flash",
                     "context_window": 1000000,
                     "api_key_file": "/home/you/.secrets/temur-gemini-key" }
}
```

Without that line the context usage advisory, auto-compaction and the
context-scaled tool-output ceiling are all off for the profile, and a
profile does not inherit a global `context_window` the way it inherits
`max_tokens`. The figure is right for the versioned id the template
defaults to; check it if you point the profile at a different model.

Prices are not baked, for Gemini or any other hosted non-Anthropic
provider, because they change more often than windows do. Add them
yourself if you want cost reporting:

```json
                     "price_input_per_mtok": 0.75,
                     "price_output_per_mtok": 3.75
```

Those two numbers are an example to check, not values temur maintains. They
are the promotional rate on Google's Gemini API pricing page as read on
2026-09-13, which that page gives until 2026-12-31, with 1.50 and 7.50 per
million tokens after it. Read the page before relying on either pair.

The OpenAI, Gemini, and Anthropic paths were verified against the real
endpoints on 2026-08-05, with two follow-up legs on 2026-08-10; xAI
was not, for want of a key. Three caveats:

- gpt-5 era model ids use a different token-cap field, and temur
  now works it out for you. They reject `max_tokens` and require
  `max_completion_tokens`. You can still say so explicitly, and an
  explicit setting always wins:

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
  startup error. No template bakes it.

  Left unset, two things cover you. On `https://api.openai.com/v1`, a
  `gpt-5`-or-later or o-series model id gets `max_completion_tokens`
  up front, including an id you switch to mid-session with `/model`.
  Anywhere else, and for any id the rule does not recognise, temur
  sends the classic name; if the server rejects it by name, temur
  retries the same request once with the other name, keeps it for the
  rest of the session, and prints one line to say so:

  ```
  note: this model wants max_completion_tokens; using it for this
  session (set "max_tokens_parameter" on the profile to skip the retry)
  ```

  The learned name lives in the session. Nothing is written to disk,
  and it resets whenever the selection changes, because the next model
  may want the other name. Setting the field on a profile you use
  often skips the one rejected request per session.

  Only that exact rejection triggers a retry: the server has to name
  the field as an UNSUPPORTED parameter, or name both fields in its
  message. A complaint about the VALUE, such as a cap larger than the
  model allows, reaches you unchanged, as does every other 400. If
  BOTH names are refused, temur stops after two attempts and says so:

  ```
  ... (set "max_tokens_parameter" on the profile; temur tried both names)
  ```

  Live-verified on `gpt-5` on 2026-08-10, including a tool call. The
  symptom it removes, seen live on `gpt-5-mini` on 2026-09-07:

  ```
  provider error: api error (HTTP 400) invalid_request_error:
  Unsupported parameter: 'max_tokens' is not supported with this
  model. Use 'max_completion_tokens' instead.
  ```

  (wrapped for the page; temur prints it as one line.)
- A hosted profile has no `context_window`, so the context
  advisory and the context-scaled tool-output caps are off for it and
  `/status` says "window size unknown". `init` never makes an
  authenticated call, so it cannot detect one; set the value by hand
  if you want the advisory.
- `/status` still reads as a floor on wires that omit usage
  entirely: a server that reports no usage object contributes nothing
  to the session total. Where a server DOES report a `total_tokens`
  larger than the counts it names, temur folds that difference into
  the output count, which is where an unreported thinking spend
  belongs and how it is priced. Gemini is the case this covers: it
  bills thinking tokens and counts them in its total while naming
  them nowhere. Servers whose total already equals the sum of its
  parts, OpenAI and llama.cpp among them, are unaffected.
  Live-verified on the streaming path on 2026-08-10: a Gemini turn
  reporting 6498 prompt and 1 completion token against a total of
  6526 was recorded as 28 output tokens, the 27-token gap folded in.

Gemini needed two fixes before its tool calling worked at all, both
shipped: its streaming responses report `finish_reason` "stop" while
attaching real tool calls, and it requires the opaque thought
signature on each call to be echoed back on the following request.
Model ids in its listing all carry a `models/` prefix, and the bare
form works on the wire. temur ignores the prefix when it compares ids,
so a bare id draws no "not in the listing" note and Tab completion
offers the bare form. Appearing in that listing is no guarantee an
id is usable, since retired ids stay listed and 404 for new accounts.

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
TCP-connect/TLS-handshake reachability probe per endpoint, whether each
configured model is one its endpoint lists, and, for keyless local
endpoints only, whether `context_window` matches what the server itself
reports. `--no-network` skips the probes and those checks. Running
`temur` with no config at all prints quickstart pointers instead of a
raw credential error.

The model check asks each configured endpoint what it serves. A keyless
local endpoint is asked without credentials, as it always has been. A
KEYED endpoint is asked too, and that is the one place doctor reads a
key at all: ONE authenticated listing GET per distinct endpoint, sent
only to the endpoint your config already gives the key to on every
turn, cached so a shared endpoint is asked once, and skipped entirely
under `--no-network`. No doctor line ever contains a key.

If the key file is missing, empty, or not a regular file, doctor opens
no connection at all, and the line says so:

```
NOTE: model check at https://api.anthropic.com skipped: the key file is empty (no request sent)
```

Absence from a listing is advisory, never a failure. Hosted providers
omit live aliases from their own listings (`claude-haiku-4-5` is
unlisted while `claude-haiku-4-5-20251001` is listed) and proxies alias
freely, so an id that some listed dated id extends counts as listed:

```
PASS: model "claude-haiku-4-5" matches claude-haiku-4-5-20251001 in the listing at https://api.anthropic.com (one authenticated GET)
```

Anything else absent is a WARN naming your id and up to ten ids the
endpoint does list. A refused, timed-out, or unparseable listing is a
NOTE rather than a FAIL, since the reachability probe above already
reported whether the endpoint is there.

For the active selection, again on a keyless local endpoint only,
doctor also checks whether the server renders your tool definitions.
llama.cpp's `--jinja` mode drops the tools array outright when the
model's chat template has no tool support: HTTP 200, nothing in the
log, nothing in the response, and an agent whose tools never fire.
Doctor sends the same one-token completion twice, once
bare and once carrying the tool definitions this session would
send, and compares the reported prompt tokens:

```
WARN: the server at http://127.0.0.1:8080/v1 appears to drop tool definitions for "gemma-3-4b" (prompt_tokens 10 with and without temur's tools): the chat template has no tool support, so tool calls can silently never happen
```

Identical counts mean the array went nowhere. Differing counts PASS,
naming both. A server that reports no usable token counts is a NOTE,
never a FAIL.

There is a third answer, and it is why the probe carries the real
definitions: a server that answers the bare completion and then
rejects the request the moment tools are attached.

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
above. Nothing found on PATH is ever executed, since a diagnostic that
runs a binary it found by searching directories would be a worse
problem than the one it reports: the comparison is contents-only, and
doctor never asks the other copy for its version.

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
profile, and `/model <name> --save` also makes it the startup default.

For keyed templates the wizard (fresh or `--add`) creates the key
file empty (mode 600), then offers a hidden paste prompt: input is
never echoed, Enter skips, and a pasted key is written only to the
key file. If the file already holds a key, an interactive wizard asks
once whether to replace it, defaulting to No; only an explicit `y`
reaches the hidden prompt, and the replacement is written at mode 600
over the old contents. Answering No, or running with input piped
(where there is nobody to ask), leaves the file alone and says how to
replace it by hand. `--force` governs the config only and never
touches a key file.

`temur init` writes a config only when the key step succeeds. A key
path it cannot use is named, with the reason, before anything is
written; a key step that fails after the config was written takes the
config back with it, restoring the previous one if `--force` had
overwritten it. A path with no directory part, like `mykey`, means the
working directory.

As a rotation reminder, `temur doctor` WARNs when a key
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
profile whose window puts it on compact prints the same line.
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

### Today's date in the prompt

Both default system prompts carry `Today's date is YYYY-MM-DD (UTC).`
just before the working-directory line, so a model knows the current
year. The date is fixed when the session starts, so a `/model` switch
sends the same prompt prefix even across midnight. Set `TEMUR_TODAY` to
pin it:

```
TEMUR_TODAY=2026-01-01 temur
```

A value not shaped `YYYY-MM-DD` is ignored with a startup notice. `--mock`
runs use 2026-01-01 unless `TEMUR_TODAY` is set. An explicit
`system_prompt` in config replaces the default prompt, so it carries no
date line unless you write one. The line costs 18 tokens in each profile.

### The prompt floor

The floor is what a turn costs before the conversation starts. Measured
live on 2026-09-14 (Qwen3-4B-Instruct-2507 Q4_K_M, `context_window`
12288, cwd `/home/dev/temur-desktop`, one request per profile, the
reported input-token count). The compact figure came from llama.cpp
`server-b10438` on CPU. The full figure came from `server-cuda-b10438`
with `-ngl 99`, because a CPU server did not finish that prefill inside
doctor's 300-second bound; the token count does not depend on the device.

| Prompt profile | Floor | Left of a 12288 window |
| --- | --- | --- |
| `full` | 7,448 tokens | ~4,840 |
| `compact` | 3,220 tokens | ~9,068 |

That is the reason auto-selection exists: on the full profile a 12288
window is 60% spent before the model reads the task, and at a
`context_window` of 4096 the floor exceeds the whole window.

Your own number will differ. The floor moves with the length of your
cwd path and the number of installed skills, both of which ride in the
system prompt, so `temur doctor` reports it for the ACTIVE selection
rather than quoting the table above:

```
PASS: prompt floor (estimate): ~2459 tokens; window 12288; 20% of the window
NOTE: that estimate is prompt bytes divided by 4, which is not tokenization: expect it to be off by some percent in either direction. A networked run against a keyless openai-compat server reports a measured figure instead. Reference measurement (2026-09-14, llama.cpp, Qwen3-4B-Instruct-2507, measured from a short working-directory path; a longer one raises both): 7,448 tokens for the full profile, 3,220 for the compact one.
NOTE: the prompt floor moves with the length of the cwd path and the number of installed skills, both of which ride in the system prompt
```

The estimate is offline and always runs. On a keyless openai-compat
endpoint with network checks enabled, doctor also asks the server that
will serve the session, with one more one-token request carrying the
real system prompt and definitions, and reports `prompt floor
(measured): N tokens` instead. When the measurement cannot be taken,
doctor says so and falls back to the estimate:

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

A WARN at exactly 20480 with no `prompt_profile` set is possible: the
auto threshold keeps temur's own full-profile floor under the WARN
line on a baseline install, but your floor also carries your installed
skills, any `system_prompt` override and your real cwd. A skills-heavy
install can get `full` from the rule and still be told to make it
compact; set `"prompt_profile": "compact"` there.

### Cost estimate

Give a profile a price pair and `/status` adds one line:

```
  [!] cost: ~$0.42 this session (estimate, configured list rates)
```

It is an estimate. temur multiplies the token counts the provider
reported for this session by the list prices YOU configured, offline;
it never asks any provider what you owe, and no provider offers an API
that would answer. Two decimals
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

The line is absent whenever computing it would mean guessing:

- an unpriced profile (nothing to compute; add the two fields),
- a keyless profile (a local server bills nobody; anthropic profiles
  are always keyed, openai-compat ones only with an `api_key_file`),
- a session that has not reported any usage yet.

The anthropic template bakes prices for its four profiles; no other
template bakes any, because no other provider's rates were verified,
and a wrong price is worse than none. The base (non-profile)
configuration has nowhere to carry a price pair, so the estimate is a
profiles feature: put your hosted selection in a profile to get it.

The two error directions:

- It can UNDERSTATE. The estimate can only count tokens the
  provider reported, and some providers do not report all of them.
  Gemini omits thinking tokens from its usage, so its session total is
  a floor and so is any figure derived from it (the same limit noted
  under the hosted-template caveats above). A provider that reports
  nothing at all shows no line rather than a fabricated zero.
- It can OVERSTATE. On the OpenAI-compatible wire, cached prompt
  tokens are reported as a SUBSET of the prompt tokens already counted,
  and the discount for them is not modeled, so a cache-heavy compat
  session estimates a little high. High is the safe direction for a
  spend number.

Anthropic is the one wire that reports cache tokens as separate counts,
so the estimate does model its published cache multipliers there: cache
reads at 0.1x and cache writes at 1.25x the input rate (the 5-minute
TTL temur uses). Those multipliers, like the baked prices, are
knowledge as of 2026-08-07 and nothing re-checks them.

### The mid-session advisory

The same estimate also speaks up on its own, every `$5` it crosses:

```
  [!] cost: this session has crossed $5.00 (estimate: ~$6.12 at configured list rates); set cost_advisory_step_usd to change the step or 0 to disable
```

One turn can be hundreds of provider round-trips, so the check runs
after EVERY response inside a turn. A jump that
clears several steps at once says so once, at the highest step crossed,
rather than printing a line per step it flew past.

The step is one global field, beside `max_tokens` and the rest:

```json
"cost_advisory_step_usd": 5.0
```

Absent means $5.00. `0` disables the advisory entirely. Negative or
non-finite is a startup error naming the field. It is global because
a budget is yours whichever provider is active, and must not reset on
a `/model` switch.

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
is unauthenticated and never touches key files, while a keyed endpoint
gets one authenticated listing per endpoint).

`temur init`'s local template asks where the server lives, then offers
what it serves, numbered:

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

`temur doctor` also compares each configured model against the
server's listing, the most likely new-user misconfig. A mismatch is a
WARN, because servers alias ids (Ollama tags, llama.cpp path names):

```
WARN: model "qwen3-bogus" is not in the server listing at http://127.0.0.1:8080/v1 (server lists: /model.gguf; advisory only, servers may alias ids)
doctor: 5 pass, 1 warn, 0 fail
```

The same check now covers KEYED endpoints (see "doctor" above), which is
where this misconfiguration actually costs you: an id your key cannot
use passes `temur init`, because init is bring-your-own and makes no
authenticated call, and it used to reach you for the first time as the
provider's raw 404 on your first message. Three things now point at the
fix. Doctor asks the endpoint. `/models` says when the id you are
running is not in the listing it just printed:

```
note: the active model "luna" is not in this listing; the provider may still serve it under an alias
```

And a 404 on a turn names the active model and the two commands:

```
provider error: api error (HTTP 404) not_found_error: model luna does not exist (the active model is "luna"; /models lists what this key can use, /model <id> switches)
```

(wrapped for the page; temur prints each as one line.) All three stay
advisory, because a listing is not a capability list. Only a 404 gains
that sentence; every other status reaches you exactly as before.

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

When an anthropic profile is configured (the Anthropic init template
writes a set of four), typing a `claude-*` model id while a local (or
any non-anthropic) provider is active hops to it: a full profile
switch, so the profile's key file, endpoint, and limits apply. It used
to set that id on the local server and fail on the next turn, which
read as "/model seems broken". Real transcript against
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

- stdout carries only the assistant's prose. All tool and status
  chrome, and any `--continue`/`--resume` backscroll, goes to stderr.
- The exit code reports the outcome: 0 for a completed turn, 1 for
  a provider or startup error, 130 when interrupted with Ctrl+C (the
  shell convention for SIGINT).
- Mutating tool calls are refused unless you pass
  `--allow-mutations`. A one-shot run cannot ask, so it denies; the
  refusal names the flag and the config key. Read-only one-shots are
  unaffected. See "Approval mode" below.
- Live one-shots save the session exactly like interactive runs, so
  `--continue -p` chains work. The save happens after every round-trip,
  so a killed one-shot still leaves a resumable transcript of the work
  it got through.
- Auto-compaction is on by default here, and only here: a one-shot
  run has nobody to act on the context advisory, so it compacts itself
  and continues. Set
  `"auto_compact": false` to restore advisory-only behaviour. See
  [Auto-compaction for unattended runs](#auto-compaction-for-unattended-runs).
- A turn that ends without a tool call gets one nudge telling the model
  nobody is reading. See "The unattended nudge" below.

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
and `doctor` subcommands. `--allow-mutations` combines with `-p` and
with interactive runs, and is rejected on a subcommand, which runs no
tools.

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

Nothing reached stdout: an interrupted one-shot never emits a
partial answer as if it were complete.

### The unattended nudge

An unattended run has nobody to answer a question or approve a plan, so
a turn that ends on either one ends the work. When temur is unattended
and a turn ends with no tool call in the model's final message, it sends
one more user message and takes one more turn:

```
Nobody is reading this session, so no answer or approval will come. If
the task is not finished, decide for yourself and finish it. If it is
finished, run whatever proves the result (the tests, the compiler, the
file's contents), fix what fails, then stop.
```

The notice on stderr says it fired:

```
  [!] unattended: the turn ended without a tool call; one continue nudge sent
```

Two conditions narrow it. The turn must have dispatched a mutating tool
(`write`, `edit`, `bash`, `spreadsheet`), or the model's reply must end
on a question mark. A read-only turn that ends in prose gets nothing,
because a session that only looked at files has no result to prove.

It fires at most once per session. The latch survives `/clear`,
compaction and a provider switch, because the model has already been
told once and a second telling is a second bill for the same sentence.
The existing recovery paths keep their own wording: a prose-formatted
tool call, an unknown tool name, or a stated promise to act each get the
nudge written for that case, and this one runs only when none of them
matched.

Unattended means `-p`, and it also means the plain REPL with piped
stdin, which is the same shape with no reader. A typed REPL and the TUI
never see it.

The cost is one extra request per nudged session, which resends the
history and therefore costs at least a prompt floor: on a 16-task
Terminal-Bench subset it fired in 9 of 16 cells per run and added 17.5%
wall clock on Qwen3-4B, 44.6% on Qwen3-8B. What the turn buys depends on
the model. Counted per nudged cell, a tool call followed it in 4 of 24
(nine-task eval, 4B) and 6 of 18 (subset, 4B), against 15 of 16 (subset,
8B). See
[COMPARISON.md](COMPARISON.md#same-rig-one-commit-apart-the-unattended-nudge-4b-row-refreshed-2026-09-11)
and the 8B sub-row below it.

To turn the nudge off, set `"unattended_nudge": false` in the config,
top-level next to `provider`. An unattended turn then ends where the
model stops, as it did before the nudge existed. `temur doctor`
accepts the key and runs no check for it.

## Documents and spreadsheets

temur reads PDFs and office files and writes spreadsheets with no
system tools: the parsers are pure Rust compiled into the binary, so
this works on a machine with nothing installed.

Reading rides the `read` tool, with no new argument: the model reads
`resume.pdf` the way it reads `main.rs`. The tool's own description
names these formats and says they come back as extracted text, so a
model has a written reason to open the file rather than report that it
cannot read one.

| extension | what comes back |
|---|---|
| `.pdf` | the text layer, page by page |
| `.xlsx` `.xlsm` `.xls` `.ods` | one block per sheet, `== Sheet: <name> ==` then rows as CSV lines |
| `.docx` | paragraphs as lines, tables as tab-separated rows |

Spreadsheet cells come back as their CACHED VALUES: a cell holding
`=SUM(B2:B8)` reads as `47`, what a person looking at the sheet sees.
Extracted text flows through the ordinary `read` pipeline, so `offset`
and `limit` page a 300-page PDF the way they page a long log, with the
same "has more" tail, and PDF extraction stops once the requested
window is full.

Writing a spreadsheet rides the `write` tool. Write CSV content to a
path ending in `.xlsx` and you get a one-sheet workbook: a field that
parses as an integer or a float is written as a number, everything
else as text. Nothing is written as a FORMULA, so a field beginning
`=`, `+`, `-` or `@` that is not a number becomes text and CSV content
cannot inject one; `-5` is still a number.

Writing a document rides the same tool. Write Markdown to a path
ending in `.docx` or `.pdf` and you get the document. The subset that
is rendered: headings 1 to 3, paragraphs, bold, italic, inline code,
fenced code blocks, bullet lists and numbered lists. Tables, images and
links are not rendered (a link becomes its text), and a nested list is
flattened to one level. List items carry a literal `- ` or `1. ` in the
text rather than Word numbering, so they read the same in every
viewer. A `.docx` has Normal, Heading 1 to 3 and a Code style on
Courier New; a `.pdf` is Helvetica with Courier for code, US Letter,
one inch margins, and its text is WinAnsi (Latin-1 plus the Windows
extras such as curly quotes and dashes). A character outside WinAnsi
is written as `?` and the tool's result says how many were replaced,
so the model can tell you; bold and italic are flattened to plain in
a PDF.

Charts and multiple sheets need the one new tool, `spreadsheet`,
because CSV cannot express them. It takes sheets of values and charts
over A1 ranges (`line`, `column`, `bar`, `scatter`, `pie`); a range
outside the rows you passed is an error naming the range. Its own
instructions tell the model to use `write` for a single sheet of plain
data.

Caps: an input document is at most 32 MiB, and a zip-based format
(`.xlsx`, `.docx`, `.ods`) at most 64 MiB decompressed, with only the
entries needed opened. Both refusals name the cap. The limits are
fixed because temur is a 32-bit binary and the address space sets
them.

A read extracts only as far as its own window. A workbook or PDF read
with the default 2000-line window stops there instead of rendering the
whole file, and the result then says more was not extracted rather than
quoting a total line count. A later `offset` extracts from the start
again; the rows before the window are counted and dropped, so paging to
row 400,000 costs no more memory than paging to row 1.

An `.ods` is measured before it is opened, because an ODS sheet is laid
out during the open and no check after that can intervene. A sheet whose
content spans more than two million cells is refused with a sentence
naming the rows and columns; the trailing empty block LibreOffice writes
at the end of every sheet does not count toward the span. The same sheet
saved as `.xlsx` reads, because that path streams instead.

Not supported: images of any kind, so a scanned PDF with no text layer
is refused with a sentence saying so; encrypted PDFs, refused by name
so you know to supply an unencrypted copy; writing `.pptx`; and
formulas evaluated by temur. A malformed or hostile file is a
one-line error the model can act on, never a crash or a raw parser
message.

## Project instructions

A project can tell temur how to behave. Put a `TEMUR.md` in the
repository and its text joins the system prompt at startup: build
commands, conventions, what not to touch.

Two names. `TEMUR.md` is temur's own; `AGENTS.md` is the cross-tool
convention other agents read, so a repository that has one works
unmodified. When both sit in the SAME directory, `TEMUR.md` wins
there.

Two locations, root first: the repository root (found by walking up
for a `.git`, so no `git` binary is needed) and the working directory,
so a monorepo's shared rules and a subdirectory's own both arrive.
Nothing between the two is read, and a working directory that IS the
root contributes once.

Read once per session. The file is loaded at startup and never
re-read, which keeps the prompt prefix stable and a provider's prompt
cache warm. The model can read and edit the file with its ordinary
tools, but what it was TOLD does not change until you restart temur.

Capped. The combined instructions are limited the way tool output is,
scaled by context window between 4,000 and 30,000 characters. Over the
cap the head is kept, the tail dropped, and both the text and the
startup line say `truncated` with the true byte count.

When a file loads, one line at startup names it:

```
project instructions: TEMUR.md (1,204 bytes)
project instructions: AGENTS.md (root) + TEMUR.md (cwd), 3,410 bytes, truncated
```

`/status` repeats that line mid-session, and `temur doctor` reports
what it would load from the directory it runs in, `none here`
included. A session with no project file prints nothing.

A `TEMUR.md` in a repository you cloned is text the model will follow.
Read it before you run an agent there, as you would read a `Makefile`
before running `make`. To refuse it:

```
temur --no-project-instructions
```

or `"project_instructions": false` in the config, top-level next to
`provider`. The flag wins over the config in either direction.

There is no way to load, reload, or edit project instructions from
inside a session.

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
that comes back as a section index instead of being middle-elided like
other oversized output, which would lose the middle of a document the
model asked for by name. This is the tool's verbatim output for a
48,427-character SKILL.md, produced over a generated fixture rather
than captured from a live session:

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

Nothing is cached: the index is a pure function of the file's bytes,
recomputed on every call, so it cannot go stale and the feature adds
no config keys and no session state.

The tool also minifies a SKILL.md before returning it: a frontmatter
block holding only `name:` and `description:` is dropped (the model
already has both from `<available_skills>`), trailing whitespace goes,
and blank runs collapse, all of it outside fenced code, which is
copied byte for byte because whitespace is semantic in a heredoc or in
Python. Measured, it saves 0.0% on this repo's own markdown, 2.2% on a
SKILL.md with frontmatter and loose spacing, and 62 characters (0.1%)
on the 48k skill above, so minification is a rounding error kept
because it is free and lossless. The section index is the mechanism.

The `Base directory for this skill: <path>` line is emitted only when
the skill's directory holds at least one entry besides its SKILL.md (a
`playbooks/` directory, a template, a script); the fixture above is a
lone SKILL.md, so it names no path. Naming it always did harm:
watching three local models work an over-cap skill, one went to grep
the directory instead of asking for a section and gave up, and another
wrote its answer into the skill directory instead of the working
directory.

Two cases keep the old behavior, because an index would
not help: a skill with no headings at all, and one whose prose before
the first heading already exceeds the cap. Both are returned whole and
truncated centrally, now with advice to fetch a section rather than to
run grep.

## The weak-model floor (T19)

Three behaviors keep small local models productive; all of them are
also active on hosted models.

Tool output keeps both ends. A tool result larger than the per-result
cap is elided in the MIDDLE, so build errors and log tails survive.
The marker between the kept halves reads:

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
`--continue` and `--resume` start with an empty read
set: the file may have changed on disk while temur was away, so a
resumed session must re-read before overwriting.

A write that destroys content says so. The guard above is about files
the session has not seen; it says nothing about a file the model read
a moment ago and then overwrote with something shorter, which weak
models do (one read three files, then replaced the 30-byte file
holding the answer with an 8-byte one and reported success). Any
successful write over a non-empty file now names what is gone:

```
Overwrote /work/beta.txt (8 bytes, replaced 30 bytes of prior content)
```

Always, with no smallness threshold, and never for a new or previously
empty file. It is a fact in the result the model has to read past.
`write` still replaces exactly what it is told to.

Prose tool calls are recovered. When a model writes its tool call
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
executed, because "the whole message is the call" is what makes a
prose call unambiguous. The nudge fires there instead, so a model that
narrates before it calls (Qwen2.5-Coder-1.5B does) gets a retry prompt
and one more chance at the tool interface. A bare
JSON object mid-prose with no fence around it stays silent:
prose that quotes a call shape while discussing a plan is common, and
the fence is the only cheap evidence the model meant it as a call.

A prose call is executed once, however often it is resent. One model
wrote a single fenced `write` about sixty consecutive times, each
resend a fresh execution, until the context window overflowed. A
resend byte-identical to the call just dispatched is answered instead
of run:

```
  [!] prose-call recovery: the write call repeated verbatim; not executed again
```

The model is told the call was already made and its result is above.
Any change of tool name or argument resets this, so a model making
progress never notices it, and the answers are capped like every other
nudge, so a model that will not move on ends its turn. Structured tool
calls have had their own doom-loop guard since M2 and are unaffected.

A call to a tool that does not exist gets named. Both the executor
and the nudge require a REGISTERED tool name, so a fenced call to,
say, `delete` used to end the turn in silence; temur now says which
tool does not exist and lists the ones that do:

```
  [!] the model called a tool that does not exist ("delete"); listed the available tools
```

It never executes anything, it is capped like the other nudges, and it
requires both a fence and an arguments key, so a `{"name": ...}`
package.json fragment in a code block still says nothing.

A turn that promises work and then stops gets one nudge. A model
that ends its turn with "Please wait while I analyze it" and makes no
tool call has stopped without starting: nothing runs between turns, so
the promise never resolves and you wait on a model that is no longer
doing anything.

```
  [!] the model promised work without calling a tool; asked it to act or answer
```

The check is narrow. It fires only when the turn made ZERO
tool calls anywhere, and only when one of a few fixed phrases ("please
wait", "one moment", "I will now", and a handful more) falls in the
LAST part of the message. That last-part rule is what separates "I will
now summarize:" followed by an actual summary, which is a finished
answer, from the same words as the final thing written, which is a
turn that stalled. A genuine answer that ends on one of those phrases
costs one extra request, since the nudge is capped like every other
one.

A turn that says it cannot read a file, having read nothing, gets one
nudge. Asked "can you read my resume and give me feedback?" in a
directory holding a single PDF, a 4B model answered "I can't directly
read files" and called nothing. It never asked for the file, so the
prompt sentences about finding files did not apply. It declared an
inability instead.

```
  [!] the model said it cannot read a file without trying; asked it to read the file
```

The nudge says files are readable here, names the two ways to find one
(`glob`, or reading the working directory), and says that PDF, Word and
spreadsheet files come back as text. The check is the promise nudge's:
ZERO tool calls anywhere in the turn, and one of nine fixed phrases in
the LAST part of the message. Those phrases are the wordings the models
were actually observed to use, so a model that declines in some other
wording is not caught. It counts against the same per-turn cap as the
other nudges, so a model that keeps declining ends its turn.

Tool calls that keep re-fetching what you already have get stopped.
A model can slip between the guards above by ROTATING: call A, then B,
then C, then A again, forever.
No two consecutive calls are identical, no two alternate, and the turn
runs until the context window ends it. One archived run did that for 77
calls and 440,983 input tokens.

A model editing ten files calls the same few tools over and over, so
temur counts FUTILE calls instead of repeats: a call that repeats an
earlier call
from the same turn and gets back a byte-identical result. Nothing
changed between the two, so the second one learned nothing. At six of
those the model is told once, in the tool results themselves, that what
it is re-fetching is already in front of it:

```
  [!] 6 tool calls this turn repeated earlier calls with unchanged results; asked the model to use what it already has (6 by input, 0 by result)
```

At eighteen the turn ends:

```
  [!] stopped: 18 tool calls this turn repeated earlier calls with unchanged results (18 by input, 0 by result)
```

A model can also dodge that rule by changing its arguments by one
character and getting the same answer back. So temur counts a second
kind of futile call: the third identical result in a row, and every one
after, from a call whose arguments are new to the turn. A call whose
arguments the turn has already seen is judged by the first rule only.
An empty result and `(no output)` never count, because distinct real
work (`mkdir`, `touch`, `chmod`) returns exactly that, and neither do
`grep`'s `No matches found` and `glob`'s `No files found`, which twenty
distinct searches that find nothing all return. An acknowledgement from
`edit`, `write`, the `spreadsheet` tool or `todowrite` never counts
either when the call succeeded, because a change made is not information
re-fetched. A failed call never breaks a run of identical results: it
extends the run when its result is the same and is otherwise passed over,
because a failure is neither progress nor information. A run of identical
failures does count on its own, wherever in the turn it starts, because
that is the same fetch failing the same way under a new input each time;
any successful call ends the run. Both kinds feed the
same count, and the brackets at the end of each notice say how many came
from each rule.

Rereading a file you just wrote is never futile, because the result
changed. A failing call counts exactly like a succeeding one, since an
identical error message is just as uninformative the second time. The
real false positive is the opposite case: if you ask a model to POLL
for something outside temur, waiting on a file another process writes
or a server coming up, an unchanged answer is the point. That is why
six calls buy only a notice, and why the gap to eighteen is wide.

An empty or whitespace `workdir` on bash falls back to the working
directory (a model that filled it with `""` used to get `failed to
spawn shell: No such file or directory (os error 2)` and parrot that
text into its next call); a workdir naming a missing path still fails,
loudly.

Binary refusals suggest the right tool. `read` refuses binary files
it cannot parse (PDFs and office documents it reads directly; see
"Documents and spreadsheets") and points at a remedy per type:
`unzip -l` for an archive, `zcat` for a gzip, and "ask the user to
describe it" for an image, since temur cannot see images. Unknown
binary types keep the general suggestion to inspect with `file`,
`unzip -l` or `strings`.

A bash command that names a `.pdf` or `.docx` and fails with 127 or
is a failed `pip` or `apt` install gets one line saying that no
converter is installed and that `write` produces those formats from
Markdown; a command that exits 0 after redirecting text into a `.pdf`
or `.docx` name gets the same line, prefixed with the path and that it
starts with plain text. The `spreadsheet` tool refuses any path that
does not end in `.xlsx` with the same pointer, so a workbook is never
written under a document's name.

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
  has read, is scrubbed from every tool result (successes and
  errors, before output truncation), so even an unexpected leak path
  cannot echo it back verbatim.

The invariant: a keyless config behaves byte-identically to earlier
releases. No guard, no namespace, no probe, no redaction.

Known limits: the identity check knows a key's identity only while the
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

## Approval mode

By default temur asks before a tool changes anything. `write`, `edit`
and `bash` ask; `read`, `glob`, `grep`, `skill` and the todo pair never
do, because they change nothing. The prompt shows what is about to
happen: the command for bash, `write <path> (<n> bytes)` for a write,
and `edit <path> (replaces "<first line>"...)` for an edit.

A plain REPL whose stdin or stdout is not a terminal, such as a piped
run, cannot ask, so it refuses, exactly as `-p` does and with the same
message: the first mutating call fails naming `--allow-mutations` and
the config key, and the turn continues from there. A piped run that
must mutate passes `--allow-mutations` or sets `"approve_mutations":
"allow"`. In v0.36.0 and earlier such a run neither asked nor refused:
mutating tools ran as if `--allow-mutations` had been given.

This section's transcripts were captured 2026-09-04 against a local
llama.cpp server (image `server-cuda-b10438`) serving
Qwen3-4B-Instruct-2507 Q4_K_M with the compact prompt profile and a
12,288-token context, so their context figures differ from the 8,192
shown elsewhere. A real run, approving one command and declining the
next (plain REPL, keyless config):

```
> Use the bash tool to run exactly this command: echo approved > allowed.txt
  → bash
  [!] context: ~2765 of 12288 tokens used; /compact frees the window by summarizing the conversation, or start a new session
  [?] bash approval needed:
        echo approved > allowed.txt
      allow? [y/a/N] (y once, a every bash this session) y
  ✓ bash: echo approved > allowed.txt
The command `echo approved > allowed.txt` was executed successfully, and the file `allowed.txt` has been created with the content "approved". Since there was no output from the command, it indicates that the operation completed silently as expected. If you need further actions or verification, let me know!
  (turn: 5522 in / 84 out, cache read 2764 write — — session: 5522 in / 84 out, cache read 2764 write —)
> Use the bash tool to run exactly this command: rm -rf /tmp/temur-demo-dir
  → bash
  [?] bash approval needed:
        rm -rf /tmp/temur-demo-dir
      !! recursive delete
      allow? [y/a/N] (y once, a every bash this session) n
  ✗ bash: bash
The command to remove `/tmp/temur-demo-dir` was declined by the user. I will not proceed with it. Let me know if you'd like an alternative action or further assistance.
  (turn: 5801 in / 68 out, cache read 5737 write — — session: 11323 in / 152 out, cache read 8501 write —)
```

(The `y` and `n` were typed at the `[y/a/N]` prompts. `allowed.txt` was
written; the `rm -rf` never ran.)

The three answers:

- `y` allows this one call.
- `a` allows this TOOL for the rest of the session. It is per tool:
  allowing bash does not allow write.
- `n` denies, and so does an empty line, an unrecognized answer, or end
  of input: the plain REPL treats anything that is not an explicit allow
  as a refusal. The TUI is stricter about stray keys: `n` and Esc deny
  there, and every other key is ignored rather than read as an answer,
  so a keystroke that arrives while the prompt is opening can neither
  approve nor deny.

A denial goes back to the model as an ordinary tool error saying the
call was declined and asking it not to retry unchanged, so the turn
continues and the model can adjust or finish. It gets no exemption from
the repetition guards: a model that resends the identical denied call
gets the identical result, and the doom-loop guard ends the turn on the
third one. Against qwen3-4b, 8 of 8 scripted denials ended with the
model finishing rather than retrying.

The `!! recursive delete` line above is emphasis only. A short fixed
list (recursive `rm`, `mkfs`, `dd` to a device, `git reset --hard` and
`git clean -f`, `shred`) adds that line to a prompt that was
already going to appear. Missing one costs nothing, because the base
rule already asks about every mutation; there is no list of commands
that skips the prompt.

Turning it off, in order of scope:

- `"approve_mutations": "allow"` in `config.json` restores the behaviour
  from before this default changed, for every session: nothing asks, and
  mutating tools just run.
  `"ask"` is the default and can be set explicitly. Any other value is a
  startup error.
- `--allow-mutations` on the command line does the same for one run. It
  is the flag form of the config value, so it also silences the
  interactive prompt, and where the two disagree the flag wins. Neither
  can make temur ask MORE than the default.

### One-shot `-p` refuses instead of asking

A one-shot run has nobody to ask, so it denies. The first mutating call
fails with an error naming both ways out:

```
$ temur -p "Create a file called notes.txt containing the single line hello."
  [!] context window 12288 detected from the server (/v1/models); the context advisory, auto-compaction, and the tool-output cap now use it
  → write
  ✗ write: write
I cannot create or modify files as requested due to safety restrictions in this non-interactive session. Please let me know if there's another way I can assist!
  [!] context: ~2874 of 12288 tokens used; /compact frees the window by summarizing the conversation, or start a new session
  (turn: 5564 in / 63 out, cache read 2831 write — — session: 5564 in / 63 out, cache read 2831 write —)
$ ls notes.txt
ls: cannot access 'notes.txt': No such file or directory
$ temur --allow-mutations -p "Create a file called notes.txt containing the single line hello."
  [!] context window 12288 detected from the server (/v1/models); the context advisory, auto-compaction, and the tool-output cap now use it
  → write
  ✓ write: /home/dev/demo/notes.txt
The file `notes.txt` has been successfully created with the content "hello".
  [!] context: ~2794 of 12288 tokens used; /compact frees the window by summarizing the conversation, or start a new session
  (turn: 5500 in / 47 out, cache read 5474 write — — session: 5500 in / 47 out, cache read 5474 write —)
```

(The two `[!]` lines in each run are this server's startup and advisory
notices, on stderr with the rest of the chrome; they have nothing to do
with approval.)

What temur handed the model was the refusal text, and the model then
described it in its own words. The text itself is the sibling of the
bash key-sandbox refusal below: `<tool> is disabled: this call would
change your system... For non-interactive use, pass --allow-mutations,
or set "approve_mutations": "allow" in config.json`. Read-only `-p` runs
are untouched: a run that only reads, globs or greps never meets this.

Every script in this repository that drives temur non-interactively
passes `--allow-mutations` for this reason. A script of your own that
mutates needs the flag or the config value. A plain REPL fed by a pipe
refuses the same way.

## Bash approval mode (T21)

This is a SECOND, independent question, about key isolation. With key
files configured, bash normally runs inside the
key sandbox ("Key isolation" above). On a kernel that denies
unprivileged user namespaces (locked-down containers and playgrounds,
commonly), the sandbox cannot start, and an interactive session asks you
about each bash command instead of refusing. The prompt shows the exact
command; `y` runs that one command unsandboxed, anything else denies it.
A denial goes back to the model as an ordinary tool error, so the turn
continues and the model can adapt. Nothing is remembered: the next
command asks again.

When a command needs BOTH answers, one prompt carries both facts, and
that combined prompt offers `y`/`N` only. No session allow is offered
for it: needing the sandbox waiver AND changing the system is the
riskiest combination temur has, and a session-wide answer is the wrong
shape for it. A session allow already held for bash answers the mutation
question only; this one still gets asked.

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
raw pty capture their echo sits with the piped input block, so they
do not appear beside the prompts above.) In the TUI the same question
appears in the input area with the command wrapped below it, answered
with a single `y`, `n`, or Esc keypress.

The rules:

- A working sandbox always wins: this question is never asked when the
  sandbox runs. Keyless configs never face it either, there being
  nothing to guard. Both still meet the mutation prompt above, which is
  a different question.
- Only interactive sessions ask: the TUI, and the plain REPL when
  stdin and stdout are a real terminal. One-shot `-p` and piped runs
  never ask; with keys guarded and no sandbox they refuse bash, and
  the refusal names both this mode and the config override.
- This refusal comes first. On a keyed host with no working sandbox, a
  one-shot `-p` gets it rather than the mutation refusal, and
  `--allow-mutations` is no way around it: `allow_bash_without_key_sandbox`
  is the only override for the key sandbox.
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
guards against the MODEL only.

- Never place a primary key on a host you do not control. Use a
  dedicated key with a spend cap, rotate it on a schedule, and revoke
  it when the machine goes away. `temur doctor` warns when a key file
  has not been rotated in `key_rotate_warn_days` (default 90).
- The durable pattern is a relay you control. Run a small
  OpenAI-compatible proxy (LiteLLM is the common choice) on a machine
  you trust, holding the real provider key. Point the playground
  profile's `base_url` at the relay and give the playground only a
  revocable virtual key with its own budget. The existing
  `openai-compat` provider and per-profile `base_url` support this
  unchanged; the untrusted host never sees the real credential, and
  killing the virtual key ends its access without touching anything
  else. If the relay stops answering mid-session, the turn ends with a
  network error inside a minute: temur bounds connecting (10s) and the
  wait for a response (60s), and tolerates 120 seconds of silence
  mid-stream, a limit that resets on every chunk so a long answer is
  never cut short.
- Locked-down kernels. Playground containers often deny
  unprivileged user namespaces, so the bash key sandbox cannot start.
  Interactive sessions then ask per-command approval (see "Bash
  approval mode" above); for non-interactive use on such a host,
  either accept `allow_bash_without_key_sandbox` (with a throwaway
  key only) or leave bash refusing and rely on the other tools.
- Paste carefully. `temur init` never accepts a key at the
  question asking where to SAVE it; a key-shaped answer there is
  dropped with a warning to rotate, because the value reached the
  terminal. Keys go in only at the hidden prompt, or into the key
  file with your editor.

## Where the other guides are

- TUI design, markdown rendering, key bindings, turn interruption:
  [TUI.md](TUI.md).
- Local/offline model serving (llama.cpp, Ollama, LM Studio, WSL2
  topology), recommended small models, the compact prompt profile:
  [OFFLINE.md](OFFLINE.md).
- Install, quickstart, and the starter config: the README.
- Every crate that ships inside the binary, with its licence and the
  command that regenerates the census:
  [THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md).
