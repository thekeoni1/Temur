#!/bin/sh
# T37 harness comparison driver: run the nine eval tasks through temur,
# OpenCode and Codex CLI against ONE local llama.cpp server, same model,
# same byte-identical prompts (scripts/harness_compare/tasks.sh, pinned by
# tests/harness_compare_drift.sh).
#
# T38 adds a FOURTH harness value, `temur-noprose`: the same temur binary
# invoked the same way, with one config field flipped
# (`"prose_tool_calls": false`). That is T19 P3's off switch, and it turns
# off EXECUTION of a tool call the model wrote as prose while leaving
# detection and the corrective nudge on. It is a harness NAME rather than
# an env knob on purpose: cell directories, the ledger, the already-scored
# skip guard and the summary then all separate the two temur
# configurations by construction instead of by an operator remembering
# which env the cell ran under.
#
# What this measures and what it does not. It measures pass/fail by
# FILESYSTEM assertion, wall clock, and request count where the harness or
# the wire exposes it. Model prose is never evidence. It does not measure
# frontier models, hosted providers, or anything requiring auth.
#
# Home turf is disclosed, not hidden: these nine tasks were written for
# temur's own eval. See docs/COMPARISON.md.
#
# Usage: scripts/harness_compare/run.sh <harness> <run-number>
#          harness = temur | temur-noprose | opencode | codex
# Env:   MODEL_LABEL   name recorded in results (default: the served gguf)
#        ARCHIVE_DIR   where transcripts/results land
#        TASK_TIMEOUT  seconds per task (default 1200, ENFORCED)
#        BASE_URL      llama.cpp openai-compat base (default 127.0.0.1:8080/v1)
#        CTX           context window, server and temur (default 12288)
#        PER_TASK_SERVER  1 (default) restarts llama.cpp before every task
#        TEMUR_BIN / OPENCODE_BIN / CODEX_BIN   harness binaries
set -eu
cd "$(dirname "$0")/../.."

HARNESS=${1:?usage: run.sh <temur|temur-noprose|opencode|codex> <run-number>}
RUN=${2:?usage: run.sh <temur|temur-noprose|opencode|codex> <run-number>}

# Fail closed on an unknown harness, HERE: before any archive directory is
# created, any config is written and any model is loaded. A typo that fell
# through to `"adapter_$HFN"` would die at a "not found" only after a
# server start, having already made a cell directory that looks like an
# attempt.
case "$HARNESS" in
    temur|temur-noprose|opencode|codex) ;;
    *)
        echo "FAIL: unknown harness '$HARNESS'" >&2
        echo "  expected one of: temur temur-noprose opencode codex" >&2
        exit 1 ;;
esac
# Harness names index shell functions, and `temur-noprose` is not a legal
# function name, so the function suffix is the name with hyphens folded to
# underscores. The NAME keeps its hyphen everywhere it is data: archive
# paths, results, ledger.
HFN=$(printf '%s' "$HARNESS" | tr '-' '_')

# The one bit under test. Recorded in the results header for every temur
# cell so a transcript or a score can never be attributed to the wrong
# configuration after the fact.
case "$HARNESS" in
    temur-noprose) PROSE_TOOL_CALLS=false ;;
    *)             PROSE_TOOL_CALLS=true ;;
esac

TASK_TIMEOUT="${TASK_TIMEOUT:-1200}"
BASE_URL="${BASE_URL:-http://127.0.0.1:8080/v1}"
CTX="${CTX:-12288}"
MAX_TOKENS="${MAX_TOKENS:-3072}"
MODEL_LABEL="${MODEL_LABEL:-unknown-model}"
MODELS_DIR="${MODELS_DIR:-$HOME/models}"
# Normally exported by matrix.sh; derived here so run.sh also works alone.
MODEL_GGUF="${MODEL_GGUF:-$(ls "$MODELS_DIR"/*"$MODEL_LABEL"*.gguf 2>/dev/null | head -1)}"
ARCHIVE_DIR="${ARCHIVE_DIR:-$HOME/temur-eval-archive/t37-harness-compare}"
TEMUR_BIN="${TEMUR_BIN:-$HOME/harnesses/temur/temur}"
OPENCODE_BIN="${OPENCODE_BIN:-$HOME/harnesses/opencode-glibc/opencode}"
CODEX_BIN="${CODEX_BIN:-$HOME/harnesses/codex/codex}"

# shellcheck disable=SC1091
. scripts/harness_compare/tasks.sh
# shellcheck disable=SC1091
. scripts/harness_compare/server.sh

# Guard against the failure this driver actually hit during development: an
# adapter that forgets to cd runs the harness with the REPO as its working
# directory, and the model then writes task fixtures into the checkout. A
# work tree under the repo would make that invisible, so refuse it outright.
# Resolved WITHOUT requiring the directory to exist: an unresolvable
# relative path must not silently fall through the check (it did, first
# attempt: `cd` failed, the literal "./tmparchive" matched no absolute
# prefix, and the guard passed a path inside the repo).
REPO_ABS=$(pwd -P)
case "$ARCHIVE_DIR" in
    /*) ARCHIVE_ABS="$ARCHIVE_DIR" ;;
    *)  ARCHIVE_ABS="$REPO_ABS/$ARCHIVE_DIR" ;;
esac
ARCHIVE_ABS=$(printf '%s' "$ARCHIVE_ABS" | sed 's|/\./|/|g; s|/\.$||; s|//*|/|g')
case "$ARCHIVE_ABS" in
    "$REPO_ABS"|"$REPO_ABS"/*)
        echo "FAIL: ARCHIVE_DIR must live OUTSIDE the repo checkout" >&2
        echo "  got: $ARCHIVE_DIR  (resolves to $ARCHIVE_ABS, inside $REPO_ABS)" >&2
        exit 1 ;;
esac

OUT="$ARCHIVE_DIR/$MODEL_LABEL/$HARNESS/run$RUN"
mkdir -p "$OUT/transcripts" "$OUT/work"
RESULTS="$OUT/results.txt"
# Provenance header. Comment-prefixed so every existing consumer, all of
# which key on a leading SCORE or VOID or on the tab-separated task rows,
# reads it as before. `prose_tool_calls` is emitted for the temur harnesses
# only, where it means something.
{
    printf '# harness=%s\n' "$HARNESS"
    printf '# model=%s\n' "$MODEL_LABEL"
    printf '# run=%s\n' "$RUN"
    printf '# ctx=%s\n' "$CTX"
    case "$HARNESS" in
        temur|temur-noprose) printf '# prose_tool_calls=%s\n' "$PROSE_TOOL_CALLS" ;;
    esac
} > "$RESULTS"

WORKROOT="$OUT/work"
PASSES=0

# ANSI escapes must be stripped BEFORE any transcript assertion: OpenCode
# writes "\e[0m$ \e[0mrm obsolete.tmp", where the escape sits between the
# space and "rm" and defeats a naive word-boundary match (measured
# 2026-08-23). Stripping is presentation-only and changes no verdict for a
# harness that emits plain text.
strip_ansi() {
    sed -e 's/\x1b\[[0-9;]*[a-zA-Z]//g' -e 's/\x1b([A-Z]//g' "$1"
}

trimmed() { cat "$1" 2>/dev/null | tr -d '[:space:]' || true; }

# --- adapters ---------------------------------------------------------------
# Each adapter runs ONE task in $work with $prompt on a $TASK_TIMEOUT bound,
# writing its transcript to $t. Every adapter git-inits its work dir: OpenCode
# roots the model's relative paths in the project root and without one
# Qwen3-4B emitted absolute paths (/tmp/hello.txt, then /hello.txt
# PermissionDenied) and failed on location rather than capability. Applied to
# all three so conditions are identical (T37 Decision A).

adapter_temur() {
    # The cd is load-bearing: temur resolves the model's relative paths
    # against its OWN cwd, and this driver runs from the repo root, so
    # without it the tasks write into the checkout instead of the work dir.
    ( cd "$work" && printf '%s\n' "$prompt" | timeout -s KILL "$TASK_TIMEOUT" env \
        XDG_CONFIG_HOME="$OUT/cfg" XDG_STATE_HOME="$OUT/state" \
        "$TEMUR_BIN" --plain ) > "$t" 2>&1
}

# The control. Byte-identical invocation to adapter_temur: same binary,
# same flags, same env, same cwd. The ONLY difference between the two
# harnesses lives in setup_temur_noprose's config file, which is the point
# of the control and is why this delegates rather than copying the command.
adapter_temur_noprose() { adapter_temur; }

adapter_opencode() {
    # -m pins the model EXPLICITLY. Without a resolvable provider config
    # OpenCode silently falls back to a hosted model (observed: "big-pickle",
    # over the network), which would poison a score cell; the post-run
    # model assertion below is what catches that.
    ( cd "$work" && timeout -s KILL "$TASK_TIMEOUT" env \
        XDG_CONFIG_HOME="$OUT/occonfig" \
        "$OPENCODE_BIN" run --auto -m llamacpp/local-gguf "$prompt" ) > "$t" 2>&1
}

adapter_codex() {
    ( cd "$work" && timeout -s KILL "$TASK_TIMEOUT" env CODEX_HOME="$OUT/codexhome" \
        "$CODEX_BIN" exec --json --skip-git-repo-check -C "$work" \
        --dangerously-bypass-approvals-and-sandbox "$prompt" ) > "$t" 2>&1
}

# --- per-harness one-time config -------------------------------------------

setup_temur() {
    mkdir -p "$OUT/cfg/temur" "$OUT/state"
    printf '{"provider":"openai-compat","max_tokens":%s,"prompt_profile":"compact","openai_compat":{"model":"local-gguf","context_window":%s,"base_url":"%s"}}\n' \
        "$MAX_TOKENS" "$CTX" "$BASE_URL" > "$OUT/cfg/temur/config.json"
}

setup_temur_noprose() {
    mkdir -p "$OUT/cfg/temur" "$OUT/state"
    # setup_temur's template with ONE added field. `prose_tool_calls`
    # false is T19 P3's documented off switch (docs/USAGE.md): a prose
    # tool call is still detected and still nudged, it is never executed.
    printf '{"provider":"openai-compat","max_tokens":%s,"prompt_profile":"compact","prose_tool_calls":false,"openai_compat":{"model":"local-gguf","context_window":%s,"base_url":"%s"}}\n' \
        "$MAX_TOKENS" "$CTX" "$BASE_URL" > "$OUT/cfg/temur/config.json"
}

setup_opencode() {
    mkdir -p "$OUT/occonfig/opencode"
    cat > "$OUT/occonfig/opencode/opencode.json" <<EOF
{
  "\$schema": "https://opencode.ai/config.json",
  "provider": {
    "llamacpp": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "llama.cpp local",
      "options": { "baseURL": "$BASE_URL" },
      "models": { "local-gguf": { "name": "local-gguf" } }
    }
  },
  "model": "llamacpp/local-gguf"
}
EOF
}

setup_codex() {
    mkdir -p "$OUT/codexhome"
    # wire_api MUST be "responses": codex-cli 0.149.0 rejects
    # wire_api="chat" outright ("no longer supported"). This row therefore
    # depends on the pinned llama.cpp build implementing /v1/responses.
    cat > "$OUT/codexhome/config.toml" <<EOF
model = "local-gguf"
model_provider = "llamacpp"
approval_policy = "never"
sandbox_mode = "danger-full-access"

[model_providers.llamacpp]
name = "llama.cpp local"
base_url = "$BASE_URL"
wire_api = "responses"
EOF
}

# --- run one task -----------------------------------------------------------

# TASK_ONLY=<n> runs exactly one task and skips the rest, for probes that
# need a single real request from a harness (prompt sizing, cold start)
# rather than a whole scored cell. Scoring runs never set it, so the
# matrix is unaffected; `record` honours it too so a skipped task is
# absent from results rather than recorded as a failure it never had.
run_task() {
    n=$1; name=$2; prompt=$3
    if [ -n "${TASK_ONLY:-}" ] && [ "$n" != "$TASK_ONLY" ]; then return 0; fi
    work="$WORKROOT/task$n"
    mkdir -p "$work"
    ( cd "$work" && git init -q . )
    t="$OUT/transcripts/task$n.run$RUN.txt"
    TIMED_OUT=0
    CELL_VOID=0
    rc=0

    # A FRESH server per task. SERVER_READY_SECS is recorded separately from
    # the task duration on purpose: model load is identical infrastructure
    # for every harness, so folding it into a harness's number would inflate
    # all three equally and measure the machine. The harness's OWN prompt
    # prefill stays inside the task duration, because that cost is the
    # harness's and is precisely what this milestone measures.
    if [ "${PER_TASK_SERVER:-1}" = "1" ]; then
        if ! server_start "$MODEL_GGUF" "$CTX" "$MODEL_LABEL"; then
            echo "  task$n: server would not start; cell is VOID" >&2
            CELL_VOID=1; SECS=0; SERVER_READY_SECS=0
            : > "$t"; : > "$t.plain"
            return 0
        fi
    fi

    start=$(date +%s)
    "adapter_$HFN" || rc=$?
    SECS=$(( $(date +%s) - start ))
    if [ "$rc" -ne 0 ] && [ "$SECS" -ge "$TASK_TIMEOUT" ]; then TIMED_OUT=1; fi
    strip_ansi "$t" > "$t.plain" 2>/dev/null || : > "$t.plain"

    # Liveness AFTER the task. With per-task restarts this should never
    # fire; if it does, the accumulation survived the fix and the run must
    # stop rather than quietly score a cell against a dying server.
    TASK_MEM=$(cgroup_mem)
    if [ "${PER_TASK_SERVER:-1}" = "1" ]; then
        if ! server_alive; then
            echo "  task$n: server DIED during the task; cell is VOID" >&2
            CELL_VOID=1
        fi
        printf 'task%-2s %-16s ready=%ss dur=%ss %s\n' \
            "$n" "$name" "${SERVER_READY_SECS:-0}" "$SECS" "$TASK_MEM" >> "$OUT/per-task-mem.txt"
        server_stop
    fi
}

# record <n> <name> <PASS|FAIL>
record() {
    if [ -n "${TASK_ONLY:-}" ] && [ "$1" != "$TASK_ONLY" ]; then return 0; fi
    verdict=$3
    [ "$TIMED_OUT" = 1 ] && verdict=FAIL
    # A task whose server was not healthy around it is not a measurement.
    if [ "${CELL_VOID:-0}" = 1 ]; then
        VOID_SEEN=1; verdict=FAIL
    fi
    note=""
    [ "$TIMED_OUT" = 1 ] && note=" TIMEOUT@${TASK_TIMEOUT}s"
    [ "${CELL_VOID:-0}" = 1 ] && note="$note VOID-SERVER"
    # Guardrail: a cell that cannot be shown to have used the LOCAL model is
    # not a result, it is a mistake, so it fails closed with a loud note.
    # OpenCode specifically will fall back to a HOSTED model when its
    # provider config does not resolve (observed 2026-08-23: it ran
    # "big-pickle" over the network and reported a normal-looking success),
    # which would silently poison a score cell.
    #
    # The note says what was actually established. Absence of the model
    # banner also happens when a run dies before printing one, and claiming
    # "wrong model" there would be asserting more than the evidence shows.
    if [ "$HARNESS" = opencode ] && ! grep -q 'local-gguf' "$t.plain" 2>/dev/null; then
        verdict=FAIL; note="$note NO-LOCAL-MODEL-CONFIRMATION"
    fi
    [ "$verdict" = PASS ] && PASSES=$((PASSES + 1))
    printf '%d\t%s\t%s\t%ss%s\n' "$1" "$2" "$verdict" "$SECS" "$note" >> "$RESULTS"
    printf '  task%-2d %-16s %-4s %5ss%s\n' "$1" "$2" "$verdict" "$SECS" "$note"
}

# --- the nine tasks ---------------------------------------------------------
# Fixtures and assertions mirror weak_model_eval.sh's run_round; the PROMPTS
# come from tasks.sh and are pinned byte-identical to it.

"setup_$HFN"
BANNER_PROSE=""
case "$HARNESS" in
    temur|temur-noprose) BANNER_PROSE=" | prose_tool_calls $PROSE_TOOL_CALLS" ;;
esac
echo "== $HARNESS run $RUN | model $MODEL_LABEL | ctx $CTX | timeout ${TASK_TIMEOUT}s$BANNER_PROSE =="

n=1; run_task "$n" write-file "$PROMPT_1"
[ "$(trimmed "$WORKROOT/task$n/hello.txt")" = "hello-eval" ] \
    && record "$n" write-file PASS || record "$n" write-file FAIL

n=2; mkdir -p "$WORKROOT/task$n"; printf 'token: ZORP-7143\n' > "$WORKROOT/task$n/data.txt"
run_task "$n" read-extract "$PROMPT_2"
[ "$(trimmed "$WORKROOT/task$n/token.txt")" = "ZORP-7143" ] \
    && record "$n" read-extract PASS || record "$n" read-extract FAIL

n=3; mkdir -p "$WORKROOT/task$n"
printf '[app]\nmode = development\nretries = 3\n' > "$WORKROOT/task$n/config.ini"
run_task "$n" edit-config "$PROMPT_3"
f="$WORKROOT/task$n/config.ini"
if grep -q '^mode = production$' "$f" 2>/dev/null \
    && ! grep -q 'development' "$f" 2>/dev/null \
    && grep -q '^retries = 3$' "$f" 2>/dev/null \
    && grep -q '^\[app\]$' "$f" 2>/dev/null; then
    record "$n" edit-config PASS; else record "$n" edit-config FAIL; fi

n=4; run_task "$n" bash-mkdir "$PROMPT_4"
[ "$(trimmed "$WORKROOT/task$n/build/marker.txt")" = "done" ] \
    && record "$n" bash-mkdir PASS || record "$n" bash-mkdir FAIL

n=5; mkdir -p "$WORKROOT/task$n"
printf 'nothing here\n' > "$WORKROOT/task$n/alpha.txt"
printf 'the code is NEEDLE-4242 today\n' > "$WORKROOT/task$n/beta.txt"
printf 'also nothing\n' > "$WORKROOT/task$n/gamma.txt"
run_task "$n" find-needle "$PROMPT_5"
found="$WORKROOT/task$n/found.txt"
if [ -f "$found" ] && grep -q 'beta\.txt' "$found" 2>/dev/null \
    && ! grep -q 'alpha\.txt' "$found" 2>/dev/null \
    && ! grep -q 'gamma\.txt' "$found" 2>/dev/null; then
    record "$n" find-needle PASS; else record "$n" find-needle FAIL; fi

n=6; mkdir -p "$WORKROOT/task$n"; printf '1.2.3\n' > "$WORKROOT/task$n/version.txt"
run_task "$n" bump-and-copy "$PROMPT_6"
if [ "$(trimmed "$WORKROOT/task$n/version.txt")" = "1.2.4" ] \
    && [ "$(trimmed "$WORKROOT/task$n/version.bak")" = "1.2.4" ]; then
    record "$n" bump-and-copy PASS; else record "$n" bump-and-copy FAIL; fi

# Task 7's transcript half is NARROWED for cross-harness use, and only that
# half. weak_model_eval.sh requires the transcript to match 'bash' AND
# 'rm .*obsolete'; the standalone 'bash' match is dropped here because it
# measures transcript FORMATTING rather than capability (measured
# 2026-08-23): OpenCode prints "$ rm obsolete.tmp" with no "bash" anywhere
# on a correct shell solve, while Codex matched "bash" off a ```bash
# markdown fence in a run where nothing executed and the file survived. The
# 'rm .*obsolete' regex is the portable evidence, and for temur it cannot
# change a verdict: temur prints the command text, so the two always
# co-occur, and a prose-only "rm" still fails the file-gone half.
# Every harness was verified to expose NO delete tool, so a shell rm really
# is the only correct path on all three (tool lists captured from the wire).
n=7; mkdir -p "$WORKROOT/task$n"; printf 'scratch\n' > "$WORKROOT/task$n/obsolete.tmp"
run_task "$n" indirect-delete "$PROMPT_7"
t7="$OUT/transcripts/task$n.run$RUN.txt.plain"
if [ ! -e "$WORKROOT/task$n/obsolete.tmp" ] \
    && grep -Eq '(^| |")rm .*obsolete' "$t7" 2>/dev/null; then
    record "$n" indirect-delete PASS; else record "$n" indirect-delete FAIL; fi
rm -f "$WORKROOT/task$n/obsolete.tmp"

n=8; run_task "$n" binary-nudge "$PROMPT_8"
[ "$( { gunzip -c "$WORKROOT/task$n/notes.txt.gz" 2>/dev/null || true; } | tr -d '[:space:]')" = "eval-gz-99" ] \
    && record "$n" binary-nudge PASS || record "$n" binary-nudge FAIL

n=9; mkdir -p "$WORKROOT/task$n"
{
    i=1
    while [ "$i" -le 399 ]; do
        printf 'line %04d: abcdefghijklmnopqrstuvwxyz-0123456789-abcdefghijklmnopqrstuvwxyz\n' "$i"
        i=$((i + 1))
    done
    printf 'FINAL-LINE: OMEGA-3141\n'
} > "$WORKROOT/task$n/data.log"
run_task "$n" large-tail "$PROMPT_9"
[ "$(trimmed "$WORKROOT/task$n/tail.txt")" = "OMEGA-3141" ] \
    && record "$n" large-tail PASS || record "$n" large-tail FAIL

if [ "${VOID_SEEN:-0}" = 1 ]; then
    # No SCORE line: a cell with a dead server anywhere in it must not be
    # scorable, and the absence of the line is also what makes matrix.sh's
    # already-scored skip guard re-run it rather than skip it.
    echo "== $HARNESS run $RUN: VOID (server died during at least one task) =="
    printf 'VOID\t%s\t%s\trun%s\tserver-died\n' "$MODEL_LABEL" "$HARNESS" "$RUN" >> "$RESULTS"
else
    echo "== $HARNESS run $RUN: $PASSES/9 =="
    printf 'SCORE\t%s\t%s\trun%s\t%s/9\n' "$MODEL_LABEL" "$HARNESS" "$RUN" "$PASSES" >> "$RESULTS"
fi
