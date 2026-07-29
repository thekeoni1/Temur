# Using temur day to day

This guide assumes temur is installed and configured (README "Install",
"Quickstart", and "Configure"). It walks one real interactive session,
then the one-shot scripting recipes, then skills.

> **Capture note.** Every transcript below is from a real run, captured
> 2026-07-28 against a local llama.cpp server (image `server-b10068`)
> serving Qwen3-4B-Instruct-2507 Q4_K_M with the compact prompt profile,
> in a scratch directory `/home/dev/demo`. Input was piped, where a
> terminal would echo the typed line after `>`; the transcripts show the
> input inline exactly as a terminal session displays it. The startup
> version banner (`temur <version> (model=..., thinking=...)`) is
> omitted so this document does not go stale on version bumps.

## A worked interactive session

Start temur with no arguments. On a terminal you get the TUI (markdown
rendering, Tab completion, a status row; see [TUI.md](TUI.md)); when
stdin or stdout is piped you get the plain line REPL shown here. Both
render the same underlying events, so everything below applies to both.

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
  `[!] context: ~7175 of 8192 tokens used; the next response may not
  fit ...` when a small context window fills up), and safety stops
  such as `[!] stopped: two tool calls alternated 3 times in a row`
  (the doom-loop guard; both examples are from real runs of the
  sessions above).
- A row of dots (`.`) is streamed thinking activity, shown as a
  passive indicator. Only the anthropic provider uses thinking, and it
  is off by default (`/thinking on` flips it for the session).

To leave: `exit`, `quit`, or Ctrl+D (EOF); temur prints `bye`. Ctrl+C
during a turn interrupts the turn, not the program (details in
[TUI.md](TUI.md), "Turn interruption").

### /clear vs /new vs /resume

Every live run saves the conversation after each turn (README
"Sessions" has the full model). Three commands manage it; pick by what
you want to keep:

- `/clear` wipes the current session's history in place and persists
  the empty state immediately. Use it when the current thread is done
  or has gone off the rails and you will not want it back, for example
  when the context notice above starts firing.
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

## Picking and keeping a model

Two T15 conveniences remove the "type a model id blind, keep it by
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
  `--continue -p` chains work.

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

## Where the other guides are

- TUI design, markdown rendering, key bindings, turn interruption:
  [TUI.md](TUI.md).
- Local/offline model serving (llama.cpp, Ollama, LM Studio, WSL2
  topology), recommended small models, the compact prompt profile:
  [OFFLINE.md](OFFLINE.md).
- The full command and session reference: README "Commands" and
  "Sessions".
