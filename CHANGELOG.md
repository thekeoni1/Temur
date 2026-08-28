# Changelog

Newest first. Dates are release dates; "Unreleased" ships next.

## Unreleased

## v0.29.0 - 2026-08-27

- `docs/COMPARISON.md` gains a GPU desktop row: the same pre-registered
  16-task Terminal-Bench 2 subset, same model, build and pins, run on a
  second machine with a GTX 1070 Ti. Pass rate does not separate the
  three harnesses there either (temur 2/16 and 1/16, opencode 1/16 and
  2/16, codex 1/16 and 0/16); wall clock does, and no cause is
  attributed to it. The earlier run on that box is disclosed as
  archive-only, because its codex column was a build failure rather
  than a measurement.

- **Known limit, measured not fixed:** temur's own request floor is
  6,991 prompt tokens on the default (full) prompt profile and 2,763 on
  the compact profile, being the system prompt plus eight tool
  definitions. On a 12288-token window the default leaves ~5.3k for the
  task, and a `context_window` below about 8k is unusable on the full
  profile because the floor exceeds it. Set `"prompt_profile":
  "compact"` on small windows.

- **An unattended run can now survive filling its own context.** temur
  watched the window fill, printed advice to run `/compact`, and then
  died on the next request with an HTTP 400. In one-shot `-p` there is
  nobody to read that advice. With `auto_compact` on, the session
  compacts itself at the next safe point and carries on with the turn.
  The default follows the invocation, because the question is whether
  anyone is there to act: on in one-shot `-p`, off in the plain REPL
  and the TUI, and an explicit `true`/`false` wins in every mode.

  Auto-compaction keeps its own history shape, distinct from
  `/compact`, which is untouched. `/compact`'s verbatim tail reaches
  back to the last plain user message, which mid-turn is the task
  prompt, so the whole turn would be tail and the compaction would free
  nothing. Auto-compaction cuts inside the turn instead: the task
  prompt verbatim, a summary of the work so far, then the last two
  round-trips. The prompt survives byte-identical because in a one-shot
  run it is the only statement of the task. The cut always lands on a
  `tool_use`/`tool_result` boundary, a turn too short to fold is left
  alone, and the whole thing is bounded at three compactions per turn,
  after which the ordinary advisory prints and the request goes out as
  it would have.

- Auto-compaction also fires at the resume seam. A `--continue -p` over
  a session that is already past the threshold used to advise at load,
  which set the advisory latch before the turn began and left the run
  unable to compact at all: the one invocation the feature exists for.
  The seam now compacts too, using the ordinary `/compact` rule, since
  before a turn begins the whole restored history is what should fold.

- A turn too short to fold yet says nothing and keeps its once-per-
  session context latch, rather than spending it on a crossing nobody
  can act on. Spending it there locked auto-compaction out of the whole
  session: a run whose very first round-trip crossed the threshold
  would advise once and then never compact, which is the opposite of
  what the feature is for. The check now repeats each round-trip and
  compacts at the first one with enough history to fold.

- The auto path reports what it did in round-trips and bytes
  (`compacted: 4 round-trip(s) summarized, 2 kept, ~2847 -> ~1132
  bytes`) instead of borrowing `/compact`'s message counts, which could
  read "7 message(s) summarized into 7" for a fold that had in fact
  replaced several round-trips of tool output. The figures are
  measured: a small fold can grow the history, and the line says so.
  `/compact`'s own outcome line is unchanged.

- **A killed run no longer loses its whole session.** The session file
  was written once, after the turn returned, so a hard kill during a
  long agentic turn left nothing: no transcript, nothing for
  `--continue`, nothing to inspect. In T39, 4 of 32 Terminal-Bench
  cells had no session file and all four were the cells whose budget
  expired, so the runs most worth reading were the ones that left no
  trace. temur now writes after every round-trip, both when the
  assistant message is appended (before its tools run) and before the
  following request, so a `SIGKILL` costs at most the request in
  flight. A `SIGTERM` handler would not have sufficed, since `SIGKILL`
  cannot be trapped. Behaviour at turn end is unchanged, and the
  save-failure notice stays once per process rather than once per
  round-trip. Replay runs still write nothing.

## v0.28.0 - 2026-08-26

- **temur now has a row on a suite it did not write.** Every
  comparison table before this one used tasks from temur's own eval,
  which the page discloses. `docs/COMPARISON.md` gains a
  Terminal-Bench 2 section: Harbor 0.22.0, a 16-task subset
  pre-registered by a mechanical rule before any score was seen, the
  same local Qwen3-4B and pinned llama.cpp server as the other tables,
  two runs each for temur, codex and opencode. The headline is that
  pass rate does not separate the three harnesses at this model, 1/16
  against 1/16 against 0-to-1/16 with exactly one task solved and all
  three solving it. Timeouts and wall clock do separate them, and no
  cause is attributed to the wall clock.

- **The first temur matrix on that suite was invalid and is disclosed
  on the page rather than quietly replaced.** The adapter written for
  Terminal-Bench piped each instruction into `temur --plain`, the line
  REPL, which reads one line per turn; 12 of the 16 instructions are
  multi-line, so temur received only the first line as its task while
  the competitors received the whole thing. temur's 32 cells were
  re-run with one-shot `-p`; the competitors' were not, because their
  delivery was never broken. A product finding derived from the
  invalid cells was withdrawn, and the ROADMAP entry built on it was
  removed with a dated correction.

- **The futile-call loop guard has now fired against a live model.**
  Open since v0.25.0. It fired once on Terminal-Bench
  `prove-plus-comm`, the notice arm rather than the stop arm, and the
  cell finished normally afterwards. One firing is not a rate, but the
  guard is no longer unexercised.

- **Two harness properties of the competitors, from reading Harbor's
  own adapters.** Harbor installs codex and opencode with `@latest`,
  so an unpinned comparison measures whatever npm served that day;
  both are pinned here. And agent install runs outside the measured
  task budget, which is worth knowing when reading the wall clock:
  4.2s for temur, which copies one static binary, against 119.8s and
  182.1s for harnesses that install Node and npm first.

- Three findings queued from the run rather than built: an unattended
  agent has no way to compact and dies at the context wall it warned
  about; a killed run loses its whole session file; and temur can end
  a turn asking a user who is not there. See ROADMAP.

- `scripts/harness_compare/run.sh` gains a comment recording why
  `--plain` is safe for that driver, whose nine prompts are all single
  line, and dangerous for any suite whose prompts are not.

## v0.27.0 - 2026-08-25

- **The claim that temur's small-model score rests on prose-call
  recovery is now measured, not inferred.** `docs/COMPARISON.md` said
  that without the feature temur would score what OpenCode and Codex
  score. A control run says so directly: the same 0.25.0 binary with
  `"prose_tool_calls": false` scores 0/9 twice on Qwen2.5-Coder-3B,
  beside 8/9 and 9/9 with the feature on, and 0/9 twice for each
  competitor. Detection and the corrective nudge stay on in the
  control, so exactly one thing is removed. Two things the control
  adds that the inference could not: the nudge alone converts nothing
  on this model, zero native tool calls across 36 nudges; and the 0/9
  is not a crash or a timeout, since the control cells are the fastest
  in the table. On Qwen3-4B, where the model calls tools natively, the
  control scores 9/9 twice and the feature is simply inert.
- The comparison driver gains a `temur-noprose` harness and a
  `HARNESSES` list. The default is the same three harnesses in the
  same order, so existing runs are unchanged. No product code changed:
  the `prose_tool_calls` config field has existed since v0.8.0.

## v0.26.0 - 2026-08-25

- **A measured comparison against OpenCode and Codex CLI**, in
  `docs/COMPARISON.md`: the same nine tasks, byte-identical, driving
  the same local model on the same machine, plus a footprint table.
  Headline, on Qwen2.5-Coder-3B: the model emits NO native tool calls
  at all against this server build, in any transcript, under any of the
  three harnesses. It writes the call as prose. temur scores 8/9 and
  9/9 because it executes a tool call written as text; OpenCode and
  Codex score 0/9 twice each. Same model, sha, server and prompts, so
  the harness is the whole difference. On Qwen3-4B, where the model
  does call tools natively, the spread is narrower: temur 9/9 twice,
  Codex 8/9 twice, OpenCode 7/9 and 6/9.
  The page leads with the home-turf disclosure, pins every version and
  sha, and states what it did not establish, including the cause of the
  wall-clock differences.

## v0.25.0 - 2026-08-23

- **A rotating repertoire of tool calls no longer runs unbounded.**
  temur's three loop guards are each narrow by construction: the
  doom-loop guard needs three identical calls IN A ROW, the
  alternating-pair guard needs a strict A,B,A,B,A,B, and the prose
  repeat guard only looks at the last prose call. A model that cycles
  through half a dozen different calls satisfies none of them. One did
  exactly that for 77 calls in a single archived task, burning 440,983
  input tokens before the context window ended it.

  The new guard counts FUTILE calls rather than repeated ones, because
  a model editing ten files legitimately rotates through the same few
  tools. A call is futile when it repeats an earlier call from the same
  turn AND gets back a byte-identical result: nothing changed, so
  nothing was learned. Rereading a file you just edited is not futile,
  because the result differs; nor is any call whose arguments differ.
  At six futile calls temur tells the model once that the results it is
  re-fetching are already in front of it, and at eighteen it stops the
  turn:

  ```
  [!] stopped: 18 tool calls this turn repeated earlier calls with unchanged results
  ```

  Errors count the same as successes, since a repeated identical error
  message teaches the model just as little. The one honest false
  positive is a model deliberately polling for something outside temur
  to change, which is why the first response is a notice and not a
  stop. Re-run against the archived task, the notice lands at call 13
  and the stop at call 28.

## v0.24.0 - 2026-08-23

- **The baked Claude Sonnet 5 list price is corrected to $2/$10 per
  million tokens.** temur baked $3/$15, which was right when it was
  recorded (2026-08-07): the $2/$10 launch pricing was introductory
  through 2026-08-31 and an increase to $3/$15 was scheduled for
  2026-09-01, so the higher pair was the standard rate to estimate at.
  Anthropic has since recorded $2/$10 as the standard price and
  cancelled that increase, so the baked pair was overstating every
  sonnet cost estimate by half. The other three tiers, all four context
  windows, and the cache multipliers were re-checked against the same
  page and are unchanged.

- **`scripts/metadata_drift.sh` cross-checks the baked model metadata
  against models.dev.** Report-only and not part of any gate: it reads
  the baked windows and prices out of `src/init.rs`, compares them to
  the community feed, and prints one PASS, DRIFT or MISSING line per
  model. models.dev is a cross-check rather than an oracle, so a drift
  is a prompt for a human to go read Anthropic's pricing page and
  decide, and the RUNBOOK ship procedure records the outcome either
  way. It exists because nothing in the repo would otherwise have
  noticed the stale sonnet price above.

- **A turn that promises work and then makes no tool call gets one
  nudge.** A model ending its turn with "Please wait while I analyze
  it" and zero tool calls has stopped without starting, and nothing
  runs between turns, so the promise never resolves. temur now says so
  and asks it to call the tool or give the final answer. Narrow by
  construction: zero tool calls anywhere in the turn, plus one of seven
  fixed phrases in the message TAIL, so "I will now summarize:"
  followed by a summary does not fire. Capped like every other nudge.

- Three RUNBOOK acceptance headings that read "(stage 1 only, NOT
  released)" now read "(recorded at stage 1, before its release)": each
  of those versions has long since shipped, so the headings asserted a
  state that stopped being true. The records under them are untouched.

## v0.23.0 - 2026-08-18

- **The `skill` tool's `section` argument is declared as a string
  instead of a two-type union, which some chat templates could not
  render at all.** JSON Schema allows `"type": ["string", "number"]`,
  but a whole class of shipped chat templates stringifies a schema by
  looking its type up in a table, and a list is not a valid key there.
  llama.cpp re-renders the template on every request when it has no
  specialized handler for the model, so one union type in one
  always-registered tool meant HTTP 400 on every turn against those
  servers, before the model saw anything. Confirmed against a
  Hermes-2-Pro template on 2026-08-17 and fixed: that same server now
  renders temur's tools normally. Nothing about what temur ACCEPTS
  changed, `{"section": 2}` still selects section 2, and a new
  registry-wide test walks every tool's schema at every depth so no
  union type can come back.
- **`temur doctor`'s tools-drop probe now sends the tool definitions the
  session would really send.** It used to send one small synthetic tool,
  which meant it could report PASS against a server that then rejected
  every real request: a template can render a toy schema and throw on
  temur's own. That happened, and it is now the probe's third answer, a
  WARN quoting the server's own message and saying plainly that every
  turn sending tools will fail the same way. Still never a FAIL. One
  cost to expect on a CPU-only local server: the second request makes it
  prefill about 24KB of tool definitions, measured at 106 seconds the
  first time, so doctor announces the probe before going quiet for it.
  That is the same prefill the first real turn pays, and it warms the
  server's prompt cache for it.
- **`scripts/serve.sh` and `scripts/weak_model_eval.sh` take an optional
  `CHAT_TEMPLATE_FILE`,** serving a model with a substitute chat
  template instead of its bundled one. It exists because that is how a
  model whose own template hides its tools can be measured at all:
  Phi-4-mini goes from 0/9 to 4/9 this way, SmolLM2-1.7B from 0/9 to
  2/9. It is a diagnostic, not a fix, and both scripts say so loudly
  whenever it is set, because the same substitute template left
  gemma-3-4b at 0/9 while it spent minutes per task inventing tool
  results that never happened. Unset, both scripts behave exactly as
  before. `scripts/offline_demo.sh` deliberately has no such knob.
- **OFFLINE.md stops saying that three models cannot call tools.** The
  verified position is narrower and more useful: the tools never
  REACHED two of the three. Phi-4-mini's own bundled template reads a
  per-message `tools` key and never the top-level variable every
  standard pipeline passes, so it renders identically with and without
  tools and llama.cpp drops the array (publisher report drafted, not
  yet filed); SmolLM2's template has no tool branch at all; gemma-3-4b
  stayed at 0/9 even under a working substitute template and remains
  unexplained.
  The substitute-template scores are published in their own subsection,
  captioned as NOT comparable to the main matrix.

## v0.22.0 - 2026-08-16

- **A tool argument sent as a string where a number or a boolean was
  asked for is now accepted rather than rejected.** Small local models
  sometimes emit an otherwise perfect tool call carrying
  `"replaceAll": "false"` or `"timeout": "600000"` in quotes; temur
  answered `invalid type: string "false", expected a boolean`, and a
  model with no other idea resent the identical call until the repeat
  guard stopped the turn. The four non-string scalar arguments in the
  tool schemas (`edit` `replaceAll`, `read` `offset` and `limit`, `bash`
  `timeout`) now accept `"true"`/`"false"` for a boolean, a string of
  digits for a count, and `"null"` for an omitted value. Nothing else:
  `"maybe"`, `"12.5"`, `"-3"`, `"True"` and a padded `" true"` all still
  fail, now with a message that names the forms that would have worked.
  The tolerance is per-field and applies only to those four arguments,
  so text stays text, and an edit whose search string is literally
  `false` is untouched. The published schemas are byte-identical: this
  is what temur ACCEPTS, not what it asks for.
- **The eval harness's per-task timeout now actually stops a task.**
  `EVAL_TASK_TIMEOUT` documented itself as "seconds allowed per task"
  and enforced nothing: the signal went to the podman client, which
  neither stopped nor stopped waiting, and one measured task ran 994s
  against a 300s cap. The bound is now enforced on the container, a
  task that hits it is recorded as a failure with a `TIMEOUT@<n>s` note
  rather than silently overrunning, and no container outlives its task.
  The default rises to 1200s, chosen above the slowest legitimate task
  ever observed so that it bounds hangs rather than truncating work;
  `0` disables it. No published score changes.

## v0.21.0 - 2026-08-16

- **The local-model table is re-measured, and every model now shows two
  scores instead of one.** Ten models ran the nine-task eval twice each
  against the shipped v0.20.0 binary on llama.cpp `server-b10438`, and
  the two rows whose runs disagreed by 2 or more tasks ran a third
  time. Showing every run is the point: two models changed score
  between consecutive runs under identical conditions, two more held
  their score while the tasks underneath them moved, and one returned a
  third distinct score when asked again, so a one-task difference
  between two rows was never a real difference. The
  new numbers are a fresh baseline and are NOT comparable to the
  2026-08-12 table, because the server build, the completion budget and
  two task wordings all changed between passes. Qwen3-4B-Instruct-2507
  remains the primary recommendation as the only model to sweep 9/9
  twice; Qwen3-4B-Thinking-2507 joins the table at the same ceiling and
  roughly twelve times the wall clock.
- **The eval harness repeats, keeps evidence, and stops handing models
  the answer.** `EVAL_RUNS` repeats the nine tasks with the server and
  pod built once; every FAILED task's work dir, state dir and results
  are archived before teardown, so a failure can be read after the fact
  instead of guessed at; `EVAL_MAX_TOKENS` (new default 3072) replaces a
  hardcoded 2048 that was a binding limit on some tasks rather than
  headroom; and the two tasks that printed a literal placeholder for the
  model to copy now name their target indirectly, so they measure the
  capability they are named for. Operator-run harness only, not part of
  `check.sh`, and no product behavior changes.
- **The tools-drop defect now cites its upstream issue.** `temur doctor`
  and the offline docs point at ggml-org/llama.cpp#27129 instead of a
  bare date. The probe itself was confirmed live for the first time
  across ten served models, reproducing the three hand-measured token
  counts exactly on a different server build.

## v0.20.0 - 2026-08-14

- **A tool call written as plain text is executed once, not once per
  resend.** Prose-call recovery executes a call the model wrote as text.
  A model can write the same call again, and again: one wrote a single
  fenced `write` about sixty consecutive times, each resend a fresh
  successful execution, until the context window overflowed. There was
  no bound, because the nudge cap counts nudges and FAILED executions
  only. A resend byte-identical to the call just dispatched is now
  answered rather than run, telling the model the call was already made
  and its result is above; any change of tool name or argument resets
  the guard, and the answers are capped like every other nudge, so a
  model that will not move on ends its turn. Structured tool calls keep
  their existing doom-loop guard and are unaffected.
- **A call to a tool that does not exist gets named instead of
  ignored.** A fenced `{"name": "delete", ...}` matched nothing, because
  both the executor and the nudge require a REGISTERED tool name, so the
  turn ended in total silence three seconds in with 31 output tokens.
  temur now names the tool that does not exist and lists the ones that
  do, from the live registry. It never executes, it is capped like the
  other nudges, and it requires both a fence and an arguments key, so a
  `{"name": ...}` package.json fragment in a code block stays silent.
- **`temur doctor` detects a server that silently drops your tool
  definitions.** llama.cpp `--jinja` drops the tools array when the
  model's chat template has no tool support: HTTP 200, nothing logged,
  nothing in the response, and an agent whose tools never fire, which
  reads as a model that cannot follow instructions. Doctor sends the
  same one-token completion twice, bare and with one probe tool, and
  compares the reported prompt tokens; identical counts WARN naming
  both numbers, differing counts PASS, no usable counts is a NOTE and
  never a FAIL. Active selection only, keyless local endpoints only,
  skipped under `--no-network`. Confirmed on `b10423-a94d563ed` and
  reported upstream 2026-08-14.
- **An empty `workdir` no longer breaks the bash tool.** A model that
  filled the optional field in with `""` got `failed to spawn shell: No
  such file or directory (os error 2)` and then parroted that error text
  into its next call's arguments. Empty or whitespace now means "not
  specified" and falls back to the working directory. A workdir naming a
  real but missing path still fails.
- **Binary read refusals name the right tool for the file.**
  `pdftotext` for a PDF, `unzip -l` for an archive, `zcat` for a gzip,
  `tar -tf` for a tarball, and "ask the user to describe it" for an
  image, since temur cannot see images. The refusal itself is unchanged
  and unknown binary types keep the general `file`/`strings` hint.
- **The default system prompt says the filesystem is reachable.** Asked
  conversationally ("can you find it in the folder?"), qwen3-4b denied
  having file access while holding file tools; the same request phrased
  as an instruction used them at once. Both prompt profiles now say to
  list or read a path before claiming it cannot be accessed.
- **`scripts/serve.sh start <model>` no longer says OK while serving a
  different one.** With a server already up, a request for another model
  printed `OK: already running` and kept serving the old one, which
  silently poisoned a measurement. It now fails, naming both models and
  the stop-then-restart sequence. With no model requested, the previous
  behavior stands.

## v0.19.0 - 2026-08-13

- **A model that narrates before it calls no longer ends its turn in
  silence.** A tool call written as plain text is executed only when the
  whole message IS the call, which is deliberate and unchanged. The
  detect-and-nudge fallback happened to share that same gate, so one
  sentence of preamble followed by a fenced call got no execution, no
  retry prompt, and no notice at all. Detection now also looks for a
  fenced block anywhere in the message and applies the same checks it
  always applied: real JSON, a registered tool name, an arguments key.
  Nothing new EXECUTES; what was silence is now a retry. Qwen2.5-Coder-
  1.5B lost eval tasks this way, including one whose call would have
  passed. A bare JSON object mid-prose without a fence stays silent on
  purpose, since prose that quotes a call shape while discussing a plan
  is common.
- **A skill only names its base directory when it has assets to name it
  for.** `Base directory for this skill: <path>` opened every skill
  result. Watched against an over-cap skill, two of three local models
  were pulled off the section index by it: one went to grep the
  directory instead of asking for a section and gave up, the other
  answered correctly from section 5 and then wrote its answer into the
  skill directory instead of the working directory. The line now appears
  only when the skill's directory holds something besides its SKILL.md,
  which is when it points at anything (a `playbooks/` directory, a
  template, a script). Skills that ship assets see byte-identical output.
- **A write that destroys content says how much.** Overwriting a
  non-empty file now reports `Overwrote <path> (8 bytes, replaced 30
  bytes of prior content)`. Always, with no smallness threshold, and
  never for a new or previously empty file. The read-first guard is
  unchanged and was never the problem: the model that replaced a 30-byte
  file holding the answer with an 8-byte one, and then reported success,
  had read that file a moment earlier and was allowed through correctly.
  What was missing was any trace in the result.
- **`temur init`'s local template now defaults to
  Qwen3-4B-Instruct-2507, not Qwen3-1.7B.** The measurements of
  2026-08-12 put the 4B at 9/9 and the 1.7B at 6/9, with the 4B also
  several times faster per task, so the shipped default and the
  "(primary)" label in the offline guide had stopped matching the
  evidence. The 1.7B is now presented as the low-RAM choice, still
  recommended and still measured, at 1.3 GB less resident. The fallback
  shortlist, printed when no server answers the model picker, leads with
  the 4B.

## v0.18.0 - 2026-08-12

- **The recommended-models table is now nine models measured on one
  day, instead of two seven-task records and two lines of hearsay.**
  Every row ran the same nine-task eval under identical conditions on
  2026-08-12, so the scores are comparable to each other: Qwen3-4B-
  Instruct-2507 9/9, Qwen2.5-Coder-3B-Instruct 8/9, Qwen2.5-Coder-1.5B-
  Instruct 7/9, Qwen3-1.7B 6/9, Qwen3-0.6B 4/9, Llama-3.2-3B-Instruct
  1/9, and Gemma-3-4B-it, Phi-4-mini-instruct and SmolLM2-1.7B-Instruct
  0/9. The two "reported (pre-T11)" rows became measurements, and the
  caveat that the table mixed seven-task and nine-task scores is gone
  because there are no seven-task rows left.
- **Three families score zero because llama.cpp never gives them the
  tools, not because they cannot use them.** Sending one request three
  ways and comparing prompt tokens (system plus a tool schema, system
  alone, neither) shows `--jinja` silently dropping the tools array for
  gemma-3, Phi-4-mini and SmolLM2, whose bundled chat templates have no
  tool-call support: those three count identical tokens with and
  without the schema, against a Qwen3-1.7B control that gains 177. The
  server answers HTTP 200 and warns about nothing, so the models invent
  calls like `{"tool": "file_delete", "path": "obsolete.tmp"}` for
  tools they were never shown. Llama-3.2-3B does receive them and fails
  for its own reason, llama.cpp's tool-call grammar rejecting its
  output server-side.
- **Qwen2.5-Coder-3B went from 0/7 to 8/9 without changing.** temur
  changed. The row had recorded a model that picked the right tool
  every time and wrote every call as plain text; T19's prose-call
  recovery now executes exactly that shape, and the transcripts carry
  the notice each time. Its 1.5B sibling writes the same JSON behind a
  sentence of preamble, which the recovery does not accept, which is
  why it still loses calls.
- The docs also now say what a score does not mean: one run carries
  about a task of noise (Qwen3-1.7B scored 6/9 twice while failing
  different tasks), and two of the nine tasks partly measure whether a
  model copies a placeholder such as `SOMEVALUE` literally, which three
  of them did.

## v0.17.0 - 2026-08-12

- **A skill too large for one tool result comes back as a section
  index instead of being cut in half.** Loading one used to middle-
  elide it like any other oversized output and then advise the model
  to "narrow the command, e.g. grep or head/tail", which is advice
  about a shell pipeline given to a model that asked for a document by
  name. Now the tool returns `<skill_index>`: the skill's opening text
  verbatim, then every heading numbered with its level and size, plus
  a sentence saying how big the skill is, that it exceeds this
  session's limit, and that nothing is summarized or omitted. A
  48,427-character skill produces an 846-character index, 1.7% of the
  file. The new optional `section` argument then fetches any part in
  full, by number or by heading text; matching ignores case,
  whitespace, and a leading `#` run, because the model is copying a
  title out of a listing it was just shown. Section extents are
  hierarchical, so asking for a chapter brings its subsections and
  never ends mid-thought, and when two sections share a heading the
  first is returned along with the numbers that reach the others.
  Errors carry their own fix: an unmatched section re-lists the
  sections, and asking for a section of a heading-less skill says so
  and spells the call that loads it whole (T28 P1, P2).
- **Nothing about this is cached, so an index cannot go stale.** It is
  a pure function of the file's bytes, recomputed per call: edit a
  SKILL.md and the next call describes the edited file, because
  nothing from the previous one was kept. No new config keys, no
  session state, no persisted index, and no invalidation rule that
  could disagree with the file (T28 P1).
- **This is aimed at small-context models, and engages by
  configuration alone.** The threshold is the same context-scaled
  tool-output cap the rest of the tools respect, so a skill that fits
  whole for a 200k model is indexed for one with an 8k window, with no
  separate code path for either. Skill loading also now drops a
  frontmatter block holding only `name:` and `description:`, which the
  model already has from `<available_skills>`, and trims trailing
  whitespace and blank runs outside fenced code, which is copied byte
  for byte. Honest about scale: that minification saves 0.0% on tidy
  markdown and 2.2% on a sloppy one. The index is what does the work
  (T28 P2, P3).
- Both skill prompts, including the compact one, now tell the model
  not to reload a skill or re-fetch a section it already has in the
  conversation (T28 P2).

## v0.16.0 - 2026-08-12

- **Turn footers no longer relabel themselves after a `/model`
  switch.** Each `▣ temur · <model> · ...` line records the model the
  turn actually ran on, captured when the turn ends. Before, the whole
  scrollback was drawn from whichever model was active now, so
  switching models rewrote the history of the session in place, which
  is exactly backwards for the one thing that backscroll is for
  (T27 P2).
- **A refused turn no longer leaves a tool spinner running forever.**
  When the model refuses after it has already streamed a tool call,
  that call's cell is closed as an error before the refusal notice.
  The call itself never ran and never will: unlike an interrupt,
  nothing is written into history, because the refused response is
  discarded whole (T27 P2).
- **`--tui` against a pipe says so instead of spinning.** It needed a
  terminal on stdin and stdout all along; without one it drew a prompt
  it could never read and burned roughly 1.6 KB/s of redraw output and
  7% CPU indefinitely. It is now a usage error naming both ways to
  work without a terminal, `-p "..."` for piped one-shot input and
  `--plain` for the line REPL. Automatic mode selection already
  required both terminals, so nothing that worked before changes
  (T27 P2).
- **`/models` can finally judge a profile on a bare model alias.** The
  Anthropic API lists dated ids (`claude-haiku-4-5-20251001`) and not
  the aliases people configure (`claude-haiku-4-5`), so a haiku
  profile got no context-window check at all. The alias is now matched
  against dated entries of itself, and the notice names the dated id it
  matched, so you can see the inference rather than wonder where the
  number came from. It is made only when unambiguous: one such entry,
  or several agreeing on one window. Disagreement stays silent, because
  a guess about a context limit is worse than no answer (T27 P3).
- **`/models` now says something when your `context_window` is smaller
  than the API reports.** Under-configuring is safe, which is why it
  was silent, but it makes the context advisory fire earlier than it
  needs to and there was no way to notice. The hint names both numbers
  and the value to raise it to. Equal stays silent (T27 P3).
- **`temur doctor` checks whether the `temur` on your PATH is the one
  that is running.** After a rebuild it is easy to keep running a
  months-old copy from `~/.local/bin` and see bugs that were already
  fixed. Same file or byte-identical copy: PASS. A different build:
  WARN naming both paths, when each was last modified, and which is
  newer, so you know whether to reinstall or rebuild. Never a FAIL,
  since a second copy is a legitimate setup, and offline like the
  other local checks. Nothing found on PATH is ever executed: the
  comparison is bytes only, and doctor never asks the other copy for
  its version (T27 P4).
- **`Session::switch_provider` takes the resolved profile.** Internal:
  six positional parameters become three, and the rule that a switch
  replaces the whole selection is now structural instead of
  conventional. Behavior is byte-identical (T27 P1).
- Not fixed, and honestly so: the report that `/models` renders two ids
  on one line in some widths could not be reproduced. Rendering was
  probed at every width from 4 to 200 columns with ids built so that
  even a fragment of one landing beside another would be caught, and
  no row ever mixed two; the plain REPL prints one line per id and
  cannot merge either. The probe is kept as a regression pin rather
  than a fix applied blind (T27 P3).

## v0.15.0 - 2026-08-11

- **The session says what it has cost, without being asked.** Every $5
  of estimated spend it crosses, temur prints
  `cost: this session has crossed $5.00 (estimate: ~$6.12 at configured
  list rates); set cost_advisory_step_usd to change the step or 0 to
  disable`. The check runs after every response inside a turn, not once
  per prompt, because a single agentic turn can be hundreds of
  round-trips: that is exactly the shape of the run this was written
  for, which reached roughly $26 before anyone looked. A jump that
  clears several steps at once advises once, at the highest crossed.
  The new global `cost_advisory_step_usd` sets the step, `0` disables
  it, and a negative or non-finite value is a startup error naming the
  field. It rides the `/status` estimate's own gate, so it appears
  exactly where that line appears and nowhere else: keyless, unpriced,
  and local selections never see it. Money already spent never fires,
  including across `--continue` / `--resume`, `/clear`, and a `/model`
  switch onto different rates. In `temur -p` it is stderr chrome like
  every other notice, and stdout stays exactly the answer.

## v0.14.0 - 2026-08-10

- **gpt-5 era OpenAI model ids are reachable.** They reject
  `max_tokens` and require `max_completion_tokens`. Set
  `"max_tokens_parameter": "max_completion_tokens"` on an
  openai-compat profile and temur sends that name, carrying the same
  value. The field takes exactly one of the two names, anything else
  is a startup error naming both, and setting it on an anthropic
  profile is an error too, since that wire uses `max_tokens` natively.
  Leaving it out sends byte-identical requests to every config written
  before it existed, and no template bakes it: the OpenAI template
  defaults to `gpt-4o`, which wants the classic name.
- **Thinking tokens are counted where a server reports a total.**
  Gemini bills thinking tokens and includes them in `total_tokens`
  while naming them in no usage field, so a turn reporting 48 prompt
  and 19 completion against a total of 103 was leaving 36 tokens of
  real spend invisible to `/status` and to the cost estimate. That
  difference now folds into the output count, where the spend belongs
  and how it is priced. Servers whose total already equals the sum of
  its parts, OpenAI and llama.cpp among them, are unaffected. The
  understatement caveat narrows to wires that report no usage at all,
  which nothing can recover.

## v0.13.0 - 2026-08-07

- **`/status` estimates what the session has cost** when the active
  profile is keyed and priced. Give a profile
  `price_input_per_mtok` / `price_output_per_mtok` (per million tokens,
  in the key's billing currency) and `/status` adds
  `cost: ~$0.42 this session (estimate, configured list rates)`,
  computed locally from the token counts the provider already reported.
  It is an estimate for awareness, never a bill: nothing calls a
  billing API, and no provider offers one to call. The line is absent
  entirely, with no nag, for an unpriced profile, a keyless local
  server, or a session that has not reported usage yet. The Anthropic
  template bakes per-model list rates; no other template bakes any,
  because a wrong price is worse than none. Both error directions are
  documented: the estimate understates where a provider omits thinking
  tokens from its usage (Gemini), and overstates a cache-heavy
  OpenAI-compatible session, since only Anthropic reports cache tokens
  as separate counts and only its cache multipliers are modeled.

## v0.12.0 - 2026-08-07

Hosted-provider verification (T13): the openai-compat provider run
against the real OpenAI and Gemini endpoints for the first time, with
every fix below found by that run rather than by review.

- **Tool calls now dispatch on assembled calls, whatever
  `finish_reason` says.** Gemini's streaming responses report "stop"
  while attaching real tool calls (its non-streaming responses report
  "tool_calls" for the identical request). temur streams, believed the
  wire, and silently discarded well-formed calls: no tool ran, nothing
  printed, and the saved session was left holding a `tool_use` with no
  `tool_result`. Assembled calls now mean tool use regardless, with
  one exception: a completion the provider filtered still refuses
  rather than dispatching. `finish_reason` "length" continues to
  report truncation, now even when calls were assembled too.
- **Gemini's thought signatures round-trip.** Its tool calls carry
  opaque state that must come back on the next request, or the request
  is rejected, which broke the agent loop on its first round trip
  while single-shot calls worked. Tool calls now carry optional opaque
  provider state through the neutral types, saved sessions, and back
  onto the wire it came from. Absent for every other provider, and
  absent means absent: request bodies and session files are unchanged
  wherever it does not apply.
- **Error bodies wrapped in a JSON array are unwrapped.** Google sends
  one, so a 404 printed `api error (HTTP 404) api_error:` with no
  message at all, hiding the sentence that explained the failure.
- **Hosted template defaults repaired from live evidence.** The OpenAI
  template defaults to `gpt-4o` and bakes its 16384 completion cap,
  because the previous default was absent from a real account listing
  and because a fresh profile inheriting the global 32000 was rejected
  on every call. The Gemini template defaults to `gemini-3.6-flash`;
  the previous default is retired for new accounts.
- **Per-model context windows in the Anthropic template**, read off
  the authenticated models API rather than assumed: haiku serves 200k
  where the other three serve 1M, so one shared constant was wrong for
  whichever tier it did not match.
- `temur init` now says the key file path question out loud, so the
  key cannot be pasted where the path belongs.

## v0.11.0 - 2026-08-01

Launch-readiness documentation pass (T23; prose and layout only, no
code or behavior change):

- README rebuilt for a first-time reader: badge row, a short pitch,
  the nine-task weak-model eval as a table near the top
  (Qwen3-4B-Instruct-2507 Q4_K_M, 9/9, RUNBOOK pointer), Install and
  Quickstart within the first two screens, a minimal Configure, a
  short Untrusted hosts, and a "How this was built" section naming
  the AI-directed, gate-everything workflow plainly. The tag-pinned
  install lines are byte-identical. Deep reference material (full
  command list, session model, context lifecycle, configuration
  recipes, key isolation, untrusted-host practice) merged into
  docs/USAGE.md, deduplicated against what was already there.
- Root tidy: the machine-setup and v1-plan documents moved from the
  root into docs/ (now docs/SETUP.md and docs/IMPLEMENTATION_PLAN.md)
  with every live reference updated; the root now holds README,
  CHANGELOG, ROADMAP, LICENSE, CLAUDE.md, and the cargo manifests.
- Milestone codes moved out of user-facing lead lines: earlier
  release sections in this changelog now lead with the feature and
  keep the code parenthetical, and README and Cargo.toml prose drop
  bare codes; RUNBOOK record titles stay verbatim.
- New scripts/bump_version.sh: a stage-1 helper that rewrites the
  four-file version map (Cargo.toml, Cargo.lock, scripts/install.sh
  VERSION, the README tag pins) on a clean tree, prints the
  resulting diff, and never commits. release.sh gate 3 remains the
  authority on version skew.

## v0.10.0 - 2026-07-31

Context-window detection and discoverability (T22):

- `temur doctor` now checks `context_window` per profile. On keyless
  openai-compat profiles with network allowed it reads the server's
  real context allocation from llama.cpp's `/props` endpoint
  (`default_generation_settings.n_ctx`, the server's `-c` flag): a
  matching configured value PASSes; a mismatch WARNs naming both
  values and the consequence direction (configured larger than the
  allocation means the context advisory fires too late and requests
  can fail at the real limit; smaller is safe but early); a missing
  value WARNs with the exact config line to add. Non-llama.cpp servers
  answer nothing useful there and stay silent. Independently, any
  profile without a `context_window` gets a one-line NOTE that the
  context advisory and the context-scaled tool-output caps are off for
  it. Keyed profiles and `--no-network` are never probed; the probe
  takes only a base URL, so it is unauthenticated by construction, the
  second and last request under the keyless-GET amendment (RUNBOOK
  record).
- `temur init` (local template, fresh and `--add`): with the server
  up, the wizard now writes the detected allocation as
  `context_window` instead of the baked 8192, with a notice naming the
  value and its source; server down or not llama.cpp keeps the baked
  value, byte-identical to before. The anthropic template's four
  profiles now carry `"context_window": 200000` (knowledge-based, not
  detected; the `/models` enrichment below reads the real value off
  the wire). Existing configs are never rewritten.
- `/models` on an anthropic profile now uses the per-model
  `max_input_tokens` the listing response already carries: a
  configured `context_window` larger than the reported value draws a
  warning naming both, a missing one draws a hint naming the exact
  config line to add, equal or smaller stays silent. No new network
  calls, and the cached listing keeps the reported windows (still
  cleared on a provider change).

## v0.9.0 - 2026-07-30

Bash approval mode and untrusted-host riders (T21):

- With key files configured and no working bash key sandbox (kernels
  that deny unprivileged user namespaces), an interactive session (TUI,
  or plain REPL on a real terminal) now asks per-command approval
  instead of refusing bash outright: the prompt shows the exact
  command, `y` runs that one command unsandboxed, anything else denies
  it, and nothing is remembered between commands. A denial goes back to
  the model as an ordinary tool error, so the turn continues. A working
  sandbox is never preempted, keyless configs never ask, one-shot `-p`
  and piped runs never ask (they still refuse), and
  `allow_bash_without_key_sandbox` still runs plain and now silences
  the ask entirely. The refusal wording leads with the interactive ask
  and keeps the config override as the non-interactive answer.
- `temur init` now catches a key-shaped answer at the key file PATH
  question (no `/`, 20+ chars, all in `[A-Za-z0-9_-]`): the value is
  dropped, never used or stored, with a warning that keys are only
  accepted at the hidden prompt and that the pasted value reached the
  terminal and should be rotated. Interactive runs re-ask; piped runs
  fail closed.
- `temur doctor`'s sandbox-unavailable line now names all three
  outcomes (interactive ask, non-interactive refusal, config override)
  and points at the new README "Untrusted hosts" section, which covers
  spend-capped throwaway keys and the LiteLLM-style relay pattern over
  the existing per-profile `base_url`.
- Test-harness only: the headless TUI key pump is now readiness-gated
  (a scripted line starts only once the app is idle), fixing a
  pre-existing flake where a zero-delay Enter could race the deliberate
  busy-Enter drop; `App` key semantics are unchanged. New one-way test
  seam `TEMUR_TEST_SANDBOX_UNAVAILABLE` forces the sandbox probe to
  FAIL (never to succeed) so the approval arms are testable on hosts
  whose kernel supports the sandbox.

## v0.8.0 - 2026-07-30

Context lifecycle: living with small context windows (T20):

- New `/compact` command: one model call (tools omitted, the session's
  own model and system prompt) summarizes the conversation under
  structured headings, then the history becomes that summary plus a
  verbatim tail, the last user-initiated exchange, merged
  alternation-safe (the summary rides inside the tail's first user
  message as a leading text block). Fail-closed: any provider error,
  interrupt, or empty summary leaves history untouched. On success the
  context estimate resets, the advisory re-arms, session usage totals
  keep accumulating (including the summary call itself), todos stay,
  and the compacted state is persisted immediately, like `/clear`. The
  notice is honest that the next request rebuilds the provider's
  cached prefix once.
- The once-per-session context warning is now a unified advisory with
  two arms: it fires at 80% of `context_window` OR when the remaining
  window is smaller than `max_tokens`, whichever comes first, and its
  wording names both remedies (`/compact`, new session). A second
  trigger fires it immediately at `--continue`/`--resume`/`/resume`
  when the restored estimate already crosses the threshold, because
  resume is the zero-waste moment to compact.
- Prefix-stability invariant tests on both providers pin that requests
  are append-only (growing the history never rewrites earlier bytes,
  modulo the one moving Anthropic cache breakpoint), the property that
  makes provider prompt caching and llama.cpp `--cache-reuse` prefix
  KV reuse effective.

Model floor: raising the harness floor for weak local models (T19):

- Tool-output truncation now scales to the model's context window
  and keeps both ends: the per-result cap is `context_window`
  clamped to 4,000..30,000 chars (no configured window keeps the
  30,000 cap exactly as before), and truncation keeps the true head
  and true tail around a one-line marker that says how to narrow
  the command, so build errors at the end of long output survive.
  Key redaction still runs before truncation.
- `write` now enforces its prompt's read-first promise: overwriting
  an existing file this session has not seen (via read, edit, or a
  previous successful write) fails with a pointer to read or edit.
  New files are unaffected. `--continue`/`--resume` start with an
  empty read set deliberately: the file may have changed on disk.
- Binary-format nudge: the write prompt steers models to produce
  binary formats (xlsx, zip, png, gz, ...) with a small script run
  via bash instead of raw-writing corrupt bytes, and read's binary
  denial now names bash inspection tools (file, unzip -l, strings).
- Prose tool-call execution, a recorded narrow amendment of T4's
  "prose is never executed" policy: an end-of-turn message with no
  structured tool calls that IS one unambiguous tool call (exactly
  one candidate in a known shape, losslessly parsed JSON, registered
  tool, object arguments) executes through the same guarded registry
  path as a structured call; the result returns as plain user text
  and a notice announces it. Failed prose executions count toward
  the per-turn nudge cap. New config `prose_tool_calls` (default
  true); false restores detect+nudge exactly.
- `scripts/weak_model_eval.sh` grows task 8 (gzip binary nudge:
  gunzip validity proves the file was scripted, not raw-written)
  and task 9 (a needle on the LAST line of over-cap output, the
  live proof of the head+tail keep). The score line is now /9.

## v0.7.0 - 2026-07-29

Key isolation: guaranteeing tools cannot reach configured keys (T18):

- File guard for read/write/edit/glob/grep: every configured
  `api_key_file` (active selection and all named profiles) plus the
  `APP_SECRET_FILE` path is denied to tools, by canonical path
  (symlinks, unborn write targets), by parent-directory prefix
  (sibling keys in a secrets dir), and by device+inode identity
  (hardlinks, renames). grep never reads a protected file, glob never
  lists one, writes and creates under a secrets dir are refused.
  Denials are ordinary tool errors naming the policy, never key
  material.
- bash sandbox: with keys configured, bash runs in an unprivileged
  user namespace + private mount namespace with every existing key
  file bind-masked by /dev/null (reads empty, writes discarded, host
  file untouched). Kernels without unprivileged user namespaces make
  bash refuse instead, naming the new
  `allow_bash_without_key_sandbox` config override (default false;
  it never disables a working sandbox). Keyless configs spawn bash
  byte-identically to before: no namespace, no probe.
- Redaction: the active provider's key (the one credential actually
  read) is scrubbed from every tool result, successes and errors,
  before output truncation so a key cannot leak split across the
  30k cut; re-registered on `/model` switches, cleared on a switch
  to keyless. Keys shorter than 8 chars are never matched.
- `temur doctor` adds two offline lines: the key-isolation guard
  count (or a keyless note) and bash sandbox availability, WARNing
  when keys exist but no sandbox is possible (naming the refusal or
  the override), never affecting the exit code.

Provider onboarding (T17):

- `temur init --add <template>` merges a template into an EXISTING
  config as named profiles instead of overwriting it: `anthropic` adds
  the curated four-profile set (fable/haiku/opus/sonnet) sharing one
  key file, `openai`/`gemini`/`xai` add one profile named after the
  template, `local` adds a keyless `local` profile reusing the
  base-URL question and model picker. Surgical config edit: key order
  and unknown fields survive, the startup `profile` key is never
  touched, and ANY profile-name collision aborts the whole merge with
  the file untouched (every collision named). The cross-provider hop
  hint now names the command (`temur init --add anthropic sets one
  up`).
- New `xai` starter template: xAI Grok API over its OpenAI-compatible
  endpoint (`https://api.x.ai/v1`, default model `grok-4`), in both
  the fresh wizard and `--add`. Spec-written; live verification stays
  parked with T13 until keys exist.
- The init wizard (fresh and `--add`) can now take the API key at a
  hidden prompt right after creating, or finding, an EMPTY key file:
  input is never echoed (termios echo off behind an RAII restore,
  SIGINT held off for the read), Enter or EOF skips, and a pasted key
  lands only in the key file (mode 600) with a best-effort wipe of the
  in-memory buffer. This is a deliberate NARROW amendment of the T14
  "init never accepts key material" rule, recorded in the RUNBOOK T17
  amendment record; a non-empty key file is never touched, no other
  surface accepts key material, and there is no --key flag, env, or
  argv path.
- `temur doctor` adds a key-rotation reminder: a present, non-empty
  key file whose mtime is at least `key_rotate_warn_days` days old
  (new optional config field; default 90, 0 disables) gets a WARN
  suggesting a rotation and naming `temur init --add` as the
  re-prompt path. Metadata only, advisory only, never affects the
  exit code.

## v0.6.0 - 2026-07-28

Model-access footgun fixes (T16):

- Cross-provider hop: `/model <claude-* id>` on a non-anthropic
  provider with an anthropic profile configured now switches to that
  profile (the exact-model match, else the first anthropic profile by
  name) instead of setting an anthropic id on the wrong provider; when
  the id is not the profile's own model it is applied on top, and the
  notice names the mechanism and the profile. Escape hatches: an id
  the active provider itself listed in `/models` always switches
  literally, and with no anthropic profile the raw switch happens as
  before plus a hint that an anthropic profile enables the hop. The
  hop makes no network request. `--save` composes: the save site is
  the hop profile's `model`, and the persist notice now names the site
  profile whenever one is active.
- `temur init`, Anthropic template: writes a curated four-profile set
  (fable, haiku, opus, sonnet over the current Anthropic model tiers),
  every profile sharing the one key file the wizard asks for; the
  model question becomes a startup-profile question (number or name,
  default sonnet, anything else re-asks). The effective default model
  stays claude-sonnet-5.
- `/model` with no argument appends two hint lines after the profile
  list (what a non-profile argument does, where `/models` fits, that
  `--save` persists). A raw-id switch whose id is absent from the last
  `/models` listing gets an advisory notice; the switch stands. Cached
  listing ids are dropped when a switch changes the provider, so one
  provider's listing never completes or judges another's ids.
- init local template writes `max_tokens` 4096 (1024 truncated first
  real tasks); README and OFFLINE recipes updated in lockstep. The
  plain truncation notice now names the limit and its source
  (`max_tokens (4096, from profile "local")`, or `from config`) and
  says the fix. init's closing text and the first-run quickstart note
  that conversations are saved automatically per working directory and
  `temur --continue` resumes the last one.

Model-selection onboarding polish (T15):

- `temur init`, local template: a Base URL question (default
  `http://127.0.0.1:8080/v1`) now precedes the model question, and when
  the server answers, its own model listing prints as a numbered picker
  (capped at 20 shown; a number or a free-text id both work; default is
  the template default when listed, else the first listed id). With no
  server reachable: a one-line note, the old free-text question, and a
  short baked shortlist of known-good small models pointing at
  docs/OFFLINE.md "Recommended small models" (which stays canonical).
  Keyed templates are unchanged. A non-default base URL is written into
  the config; the default render stays byte-identical.
- `/model <model-id> --save` persists a raw-id switch to config.json
  after the switch succeeded; `/model --save` persists the currently
  active model. The write is a surgical serde_json::Value edit (never a
  round trip through the config struct): unknown fields and the user's
  key order survive, the file is written atomically (temp + rename),
  and the site is the active profile's `model`, `openai_compat.model`,
  or the top-level `model` key as appropriate. `--save` with a profile
  name is a clean error (the startup profile stays the hand-edited
  `profile` key). Persistence failure after a successful switch keeps
  the switch and says why the save failed. Replay-guarded like the
  other mutators.
- `temur doctor` now checks each keyless openai-compat selection's
  configured model against the server's listing: PASS when listed, WARN
  naming the model and up to 10 served ids when not (advisory, never a
  FAIL, since servers alias ids), a plain NOTE when the listing itself
  fails, and a SKIP line for keyed selections (that check would need an
  authenticated request). `--no-network` skips model checks like the
  probes.
- The single network capability all of this rides on is one new
  provider fn, `list_models_keyless(base_url, timeout)`: an
  unauthenticated GET of `{base}/models` with a 3s timeout that cannot
  attach auth headers or touch key files by construction. init and
  doctor call only this, never the authenticated listing path.
- First-run quickstart gains a pointer line to docs/OFFLINE.md
  "Recommended small models".
- Internal: serde_json's preserve_order feature is enabled so saves
  keep config key order; request bodies now serialize through a
  sorted-key step pinning the wire byte-identical to every release
  since T1 (the request_golden suite enforces it).

## v0.5.0 - 2026-07-28

Onboarding + one-shot mode (T14; built before T13, which awaits keys):

- First-run quickstart: running with no config file, no `--mock`, and no
  usable credential now prints guidance (the config path looked for,
  `temur init` / `temur doctor` pointers, a README pointer) and exits
  FAILURE, instead of the raw "secret: APP_SECRET_FILE is not set"
  error. Any existing config, `--mock` run, or launcher-style
  `APP_SECRET_FILE` run behaves byte-identically to before.
- One-shot mode: `-p <text>` / `--prompt <text>` runs exactly one full
  agentic turn (all tool rounds) on the plain path with no banner;
  assistant prose to stdout, tool/status chrome (and `--continue`/
  `--resume` backscroll) to stderr; exit SUCCESS on a completed turn,
  FAILURE on a provider or startup error. Composes with `--continue`,
  `--resume`, and `--mock` (persistence stays off there); mutually
  exclusive with `--tui`. Live one-shots save the session, so
  `temur -p` chains with `temur --continue -p`.
- `temur init`: line-based wizard (pipeable answers) with four
  templates: local llama.cpp/Ollama/LM Studio (keyless), Anthropic,
  OpenAI, Gemini (hosted pair via their OpenAI-compat endpoints);
  per-template model defaults; keyed templates get a key file path
  (default `~/.secrets/temur-<provider>-key`) created EMPTY, mode 600
  (parent dir 700 if created), paste-with-your-editor instruction.
  Refuses to overwrite an existing config unless `--force`; an existing
  key file is never touched. No key material ever passes through temur.
- `temur doctor`: read-only diagnosis, one PASS/WARN/FAIL line per
  check, exit SUCCESS iff no FAIL: config parse + the same eager
  validation as startup, active selection, key files by metadata only
  (missing/empty FAIL for the active selection, WARN for inactive
  profiles; group/other mode bits WARN), sessions dir writability, and
  one TCP-connect(+TLS-handshake for https) probe per distinct
  base_url, never an HTTP request. `--no-network` skips probes. With no
  config, FAILs with the quickstart pointer.
- tests/cli.rs: new black-box suite spawning the real binary with
  isolated XDG dirs (exit codes, stdout/stderr split, wizard piping,
  key-file metadata); check.sh mounts the bin dir so the suite also
  runs in both containers.
- One-shot exit codes completed: an interrupted one-shot (Ctrl+C)
  now exits 130 (128+SIGINT, the shell convention), and interruption
  wins over a raced error. The full contract: 0 completed turn, 1
  provider or startup error, 130 interrupted; verified end-to-end by
  an event-driven SIGINT test in tests/cli.rs.
- Usage docs: new docs/USAGE.md (a worked interactive session,
  one-shot scripting recipes with the exit-code contract, the skills
  contract with a minimal working example; all transcripts from real
  local-model runs), an audience note atop SETUP.md separating the
  dev-machine recipe from installing/using temur, and README links to
  USAGE.md and TUI.md.

CI (T12):

- Two-tier GitHub Actions CI (first-party actions only). Tier 1 on
  every push to main: a hermetic test job (cargo build + full suite +
  forbidden-dep scan) and a release-gate job running the real
  release.sh (generic leak scan over files and full history, skew
  gate, 4-target static build with per-target asserts, SHA256SUMS)
  with staged artifacts uploaded for 7 days. Tier 2 on manual
  dispatch: the full check.sh under rootless podman, verified green
  in a live run.
- check.sh: target dir and TUI smoke log dir are now env-overridable
  (`TEMUR_TARGET_DIR`, `TEMUR_CHECK_TMP`); defaults are
  behavior-identical, no other script changes.
- SETUP.md: recorded the T7-era cross-toolchain additions (ARM cross
  compilers, qemu-user-static, the three extra rustup targets) that
  the original stages predated.

## v0.4.0 - 2026-07-27

Multi-model ergonomics (T11):

- `scripts/serve.sh start [model]` selects a `.gguf` from `MODELS_DIR`
  by name: exact basename beats unique substring, case-insensitive;
  zero or several matches fail and list every candidate with its size.
  The no-argument lone-gguf auto-default stays; its failure now lists
  candidates too. `MODEL_GGUF` remains an explicit override and
  conflicts with a name argument.
- serve.sh RAM fit warning (advisory only): model file size plus a
  generous context allowance (128 KiB per context token) checked
  against `MemAvailable` before start; a single WARN line, then the
  start proceeds. `MEMINFO` knob makes the check testable.
- Compact bash prompt gained one sentence steering models to bash for
  file operations no dedicated tool covers (delete, move, copy, chmod):
  closes the observed qwen3-1.7b gap where "delete the file" was
  refused for lack of a delete tool.
- weak_model_eval.sh task 7, indirect-delete: "delete the file" naming
  no tool; PASS requires the file gone AND a bash rm call in the
  transcript. SCORE is now N/7.
- docs: expanded Ollama recipe (profile example, /models note), new
  LM Studio recipe including WSL2-to-Windows-host networking, serve.sh
  selection and RAM warn docs, and the small-model shortlist table
  (file size, est. RAM at 8k ctx, tool calls, indirect selection,
  verification status).

## v0.3.0 - 2026-07-26

Command ergonomics (T9):

- Per-profile prompt profiles: a profile's `prompt_profile` overrides the
  global one (own > global > full), validated at startup; `/model`
  switches swap the system prompt and tool descriptions atomically;
  `/status` gained the `prompt: full|compact` field.
- `/models` lists model ids from the active provider (live GET, both
  wire families); `/model <model-id>` switches to a raw id WITHIN the
  active provider (profile names win on collision; endpoint,
  credentials, limits, and prompt profile stay).
- TUI command styling and completion: `/`-input renders in the cyan
  accent, the status row live-hints the command being typed from the
  COMMANDS table, and Tab/BackTab cycle completions in place.
- `scripts/serve.sh start` defaults `MODEL_GGUF` when `MODELS_DIR`
  (default `$HOME/models`) holds exactly one `.gguf`; zero or several
  files fail with the searched dir and count.

Session management (T10):

- Named multi-session per project: the default session keeps the exact
  pre-T10 filename; `/new <name>` creates named sibling sessions
  (names keep `[A-Za-z0-9._-]`, cap 32; the file appears on the first
  turn's save).
- `/sessions` lists every saved session across projects (name, recorded
  cwd, message count, file name, derived title, active marker, newest
  first); `/resume <key>` switches by session name or file-name prefix
  with load-first atomicity (a failed resume changes nothing) and a
  cross-project advisory; `--resume <key>` does the same at startup.
- Resuming renders the saved history into the transcript as backscroll
  (prompts, replies, tool names) in both UIs; `--continue` now shows
  the same backscroll.
- Session format unchanged: FORMAT_VERSION stays 1, filenames and the
  FNV digest are untouched, compatibility holds in both directions
  (pre-T10 files load as the default session; default-session files
  stay byte-identical to the pre-T10 shape).

## v0.2.0 - 2026-07-26

Daily-driver UX (T8):

- Slash commands (`/help`, `/status`, `/model`, `/clear`, `/thinking`)
  and named config profiles with atomic in-session `/model` switching.
- Markdown rendering for assistant prose in the TUI (pulldown-cmark,
  terminal renderer) plus the formalized monochrome style contract
  (DIM/BOLD/ITALIC/UNDERLINED and exactly three accents).
- `scripts/serve.sh start|stop|status`: background llama.cpp server
  launcher, loopback-only publish, pinned image, never auto-pulls.
- check.sh hygiene: host-side invocations run with isolated XDG dirs;
  `tests/sigint.rs` joined the container suites on both paths.

## v0.1.1 - 2026-07-23

Ten review fixes from the post-v0.1.0 code review:

- Edit correctness: block-anchor matching now requires the expected
  offset or a similarity floor (silent mis-splice fixed), and fuzzy
  splices keep the FILE's indentation, not the model's.
- Portable installer verify: the GNU-only `sha256sum` flags that broke
  busybox/Alpine replaced with a portable check; `install_test.sh`
  matrix added.
- Interruption correctness: plain-REPL SIGINT bridges into the cancel
  token (first Ctrl+C interrupts, second force-quits), bash kills its
  whole process group, real provider errors are no longer swallowed by
  an interrupt race, thinking-only landings persist nothing, and the
  cancel token clears at submission (not at turn entry).
- Cleanups: matcher precompute, one Session constructor, one builder
  for the synthesized interrupt marker.

## v0.1.0 - 2026-07-23

Initial release:

- Agent loop with seven tools plus the skill tool, doom-loop and
  weak-model guards, session persistence with `--continue`, turn
  interruption (Esc / Ctrl+C).
- Providers: Anthropic and OpenAI-compatible (keyless local servers
  first-class), provider-neutral history, pure-Rust TLS (rustls/ring).
- UIs: ratatui TUI and plain line REPL.
- Packaging: static musl release builds for i686, x86_64, aarch64, and
  armv7 (hard-float), with checksum-verified installer.
