#!/bin/sh
# T4 weak-model eval harness, operator-run (NOT part of check.sh): measures
# — instead of claiming — how well a small local model drives temur's
# tools. Nine fixed tasks run against a llama.cpp server inside a podman
# pod created with --network none (same zero-internet-by-construction setup
# as scripts/offline_demo.sh); every task is scored by a HOST-VERIFIED
# filesystem assertion only — model prose is never evidence.
# Task 7 (indirect-delete) additionally requires bash tool activity in the
# transcript: it probes tool SELECTION (no delete tool exists; bash is the
# intended path), so the end state alone is not enough.
# Task 8 (binary-nudge, T19): gzip validity alone proves the model did not
# raw-write the bytes with the write tool. Task 9 (large-tail, T19): the
# needle sits on the LAST line of output far larger than the tool-output
# cap, so only the T19 head+tail truncation can carry it.
#
# Nothing is ever pulled or downloaded here: preflight prints the exact
# pull command and exits if an image is missing.
#
# Usage:  MODEL_GGUF=/path/to/model.gguf scripts/weak_model_eval.sh
# Knobs:  MUSL_BIN           path to the musl-static temur binary
#         LLAMA_IMAGE        server image (pinned default below)
#         CTX                server context size, mirrored into context_window
#         PROMPT_PROFILE     temur prompt profile for the run (default compact)
#         EVAL_TASK_TIMEOUT  seconds allowed per task (default 300)
#         EVAL_MAX_TOKENS    per-turn completion budget (default 3072)
#         EVAL_RUNS          how many times the nine tasks repeat (default 1);
#                            the server and the pod are built ONCE and shared
#                            across runs, so only model sampling varies
#         EVAL_MIN           minimum passing score; 0 (default) = informational
#                            only, nonzero = exit 1 when ANY run is below it
#         EVAL_KEEP_ALL      1 = archive every task's artifacts, not just the
#                            failures (default 0)
#         EVAL_TRANSCRIPT_DIR  where per-task transcripts, per-run results and
#                            kept artifacts are stored
set -eu
cd "$(dirname "$0")/.."

MUSL_BIN="${MUSL_BIN:-/home/dev/rustcode-target/i686-unknown-linux-musl/release/temur}"
# Pinned llama.cpp server build (tag scheme: server-b<build>); update
# deliberately, never track latest.
LLAMA_IMAGE="${LLAMA_IMAGE:-ghcr.io/ggml-org/llama.cpp:server-b10438}"
APP_IMG=docker.io/i386/debian:stable
BARE_IMG=docker.io/library/busybox:stable
CTX="${CTX:-8192}"
PROMPT_PROFILE="${PROMPT_PROFILE:-compact}"
EVAL_TASK_TIMEOUT="${EVAL_TASK_TIMEOUT:-300}"
EVAL_MAX_TOKENS="${EVAL_MAX_TOKENS:-3072}"
EVAL_RUNS="${EVAL_RUNS:-1}"
EVAL_MIN="${EVAL_MIN:-0}"
EVAL_KEEP_ALL="${EVAL_KEEP_ALL:-0}"
EVAL_TRANSCRIPT_DIR="${EVAL_TRANSCRIPT_DIR:-/tmp/temur-weak-eval}"
POD=temur-weak-eval

EVAL_ROOT=""
CFG_DIR=""
teardown() {
    podman pod rm -f "$POD" >/dev/null 2>&1 || true
    [ -n "$EVAL_ROOT" ] && rm -rf "$EVAL_ROOT"
    [ -n "$CFG_DIR" ] && rm -rf "$CFG_DIR"
}
trap teardown EXIT INT TERM

echo "==== weak-model eval: preflight ===="

[ -x "$MUSL_BIN" ] || { echo "FAIL: musl binary not found at $MUSL_BIN (build with: cargo build --release --target i686-unknown-linux-musl)"; exit 1; }
readelf -l "$MUSL_BIN" | grep -q 'INTERP' && { echo "FAIL: INTERP present — binary is not static"; exit 1; }
readelf -d "$MUSL_BIN" 2>/dev/null | grep -q 'NEEDED' && { echo "FAIL: NEEDED entries — binary is not static"; exit 1; }
echo "OK: musl binary static (no INTERP, no NEEDED)"

[ -n "${MODEL_GGUF:-}" ] || { echo "FAIL: set MODEL_GGUF=/path/to/model.gguf"; exit 1; }
[ -f "$MODEL_GGUF" ] || { echo "FAIL: MODEL_GGUF not found: $MODEL_GGUF"; exit 1; }
echo "OK: model file present ($MODEL_GGUF)"

# NEVER auto-pull. Missing image => print the exact command and stop.
for img in "$LLAMA_IMAGE" "$APP_IMG" "$BARE_IMG"; do
    podman image exists "$img" || { echo "FAIL: image not present locally: $img"; echo "  fetch it first (on a connected machine):  podman pull $img"; exit 1; }
done
echo "OK: all images present locally (nothing will be pulled)"

case "$PROMPT_PROFILE" in
    full|compact) ;;
    *) echo "FAIL: PROMPT_PROFILE must be 'full' or 'compact' (got '$PROMPT_PROFILE')"; exit 1 ;;
esac

# Numeric knobs are validated here rather than failing deep inside a run.
for knob_pair in "EVAL_RUNS=$EVAL_RUNS" "EVAL_MAX_TOKENS=$EVAL_MAX_TOKENS"; do
    knob_name=${knob_pair%%=*}
    knob_val=${knob_pair#*=}
    case "$knob_val" in
        ''|*[!0-9]*) echo "FAIL: $knob_name must be a positive integer (got '$knob_val')"; exit 1 ;;
    esac
    [ "$knob_val" -ge 1 ] || { echo "FAIL: $knob_name must be at least 1 (got '$knob_val')"; exit 1; }
done

echo "==== pod bring-up (--network none) ===="

podman pod rm -f "$POD" >/dev/null 2>&1 || true
podman pod create --name "$POD" --network none >/dev/null
podman run -d --pod "$POD" --name "$POD-llama" \
    -v "$MODEL_GGUF":/model.gguf:ro "$LLAMA_IMAGE" \
    -m /model.gguf -c "$CTX" --jinja --host 127.0.0.1 --port 8080 >/dev/null
echo "server starting (ctx $CTX, --jinja)"

i=0
until podman run --rm --pod "$POD" "$BARE_IMG" \
    wget -q -O /dev/null http://127.0.0.1:8080/health 2>/dev/null; do
    i=$((i + 1))
    [ "$i" -ge 30 ] && { echo "FAIL: server not healthy after ~60s; last logs:"; podman logs --tail 15 "$POD-llama" || true; exit 1; }
    sleep 2
done
echo "OK: server healthy"

echo "==== eval setup ===="

EVAL_ROOT=$(mktemp -d)
CFG_DIR=$(mktemp -d)
mkdir -p "$CFG_DIR/temur" "$EVAL_TRANSCRIPT_DIR"
# Keyless local config; the profile under test is written into the config.
# EVAL_MAX_TOKENS defaults to 3072: thinking models stream reasoning that
# counts against the completion budget, and 2048 (the T4..T31 default) was
# measured binding rather than generous, spending a whole turn on prose
# before any tool call landed. llama.cpp rejects a request on PROMPT tokens
# alone, so a completion budget that pushes prompt+max_tokens past the
# context size is not itself refused (measured 2026-08-15 on ctx 8192:
# prompt 5948 with max_tokens 3072 returns HTTP 200; an oversized prompt
# returns exceed_context_size_error naming n_prompt_tokens only).
printf '{"provider":"openai-compat","max_tokens":%s,"prompt_profile":"%s","openai_compat":{"model":"local-gguf","context_window":%s}}\n' \
    "$EVAL_MAX_TOKENS" "$PROMPT_PROFILE" "$CTX" > "$CFG_DIR/temur/config.json"
echo "profile: $PROMPT_PROFILE   per-task timeout: ${EVAL_TASK_TIMEOUT}s   max_tokens: $EVAL_MAX_TOKENS"
echo "runs: $EVAL_RUNS   transcripts: $EVAL_TRANSCRIPT_DIR"

SCORES="$EVAL_ROOT/scores.txt"
: > "$SCORES"

trimmed() { cat "$1" 2>/dev/null | tr -d '[:space:]' || true; }

# run_task <n> <name> <prompt>: launches a fresh temur --plain process in
# the task's own work subdir. Each task block below mkdirs and seeds its
# work dir right before invoking this — all seeding lives in this script,
# never with the operator.
# The session store is mounted too, as a SIBLING of the work dir rather
# than a child: temur autosaves every turn under $XDG_STATE_HOME, which
# gives failed tasks a structured record of the tool calls (arguments
# included, which --plain never prints), but a state directory INSIDE
# /work would be visible to the model and its session JSON quotes the
# task's own needles, which would corrupt the search and listing tasks.
run_task() {
    n=$1; name=$2; prompt=$3
    work="$WORKROOT/task$n"
    state="$WORKROOT/state$n"
    mkdir -p "$state"
    start=$(date +%s)
    printf '%s\n' "$prompt" | timeout "$EVAL_TASK_TIMEOUT" \
        podman run --rm -i --pod "$POD" \
        -v "$(dirname "$MUSL_BIN")":/app:ro \
        -v "$CFG_DIR":/cfg:ro -v "$work":/work -v "$state":/state \
        -e XDG_CONFIG_HOME=/cfg -e XDG_STATE_HOME=/state -w /work "$APP_IMG" \
        /app/temur --plain > "$EVAL_TRANSCRIPT_DIR/task$n.run$RUN.txt" 2>&1 || true
    SECS=$(( $(date +%s) - start ))
}

# archive_task <n> <PASS|FAIL>: keeps a failed task's evidence before
# teardown removes it. Teardown runs strictly after all scoring, so what
# is copied here is exactly what the assertion ran against.
archive_task() {
    if [ "$2" != "FAIL" ] && [ "$EVAL_KEEP_ALL" != "1" ]; then
        return 0
    fi
    dest="$EVAL_TRANSCRIPT_DIR/task$1.run$RUN.artifacts"
    rm -rf "$dest"
    mkdir -p "$dest"
    if [ -d "$WORKROOT/task$1" ]; then
        cp -R "$WORKROOT/task$1" "$dest/work" || true
    fi
    if [ -d "$WORKROOT/state$1" ]; then
        cp -R "$WORKROOT/state$1" "$dest/state" || true
    fi
}

record() { # record <n> <name> <PASS|FAIL> <secs>
    printf '%s|%s|%s|%s\n' "$1" "$2" "$3" "$4" >> "$RESULTS"
    echo "task $1 ($2): $3 (${4}s)"
    archive_task "$1" "$3"
}

# run_round: the nine tasks, in order, against the already-running server.
# Called once per EVAL_RUNS; every task gets a fresh work dir under the
# run's own root, so no run can see another's leftovers.
run_round() {

# 1: plain write.
n=1; name=write-file
mkdir -p "$WORKROOT/task$n"
run_task "$n" "$name" \
    'Use the write tool to create a file named hello.txt containing exactly this text: hello-eval'
if [ "$(trimmed "$WORKROOT/task$n/hello.txt")" = "hello-eval" ]; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi

# 2: read + extract. The prompt describes the SHAPE of the line without
# quoting a stand-in value: a literal placeholder is copyable, and three
# models copied one instead of the value it stood for (T29 finding 2),
# which made this task partly a measure of placeholder literalism.
n=2; name=read-extract
mkdir -p "$WORKROOT/task$n"
printf 'token: ZORP-7143\n' > "$WORKROOT/task$n/data.txt"
run_task "$n" "$name" \
    "Two steps. Step 1: use the read tool on data.txt. It holds a single line that begins with 'token: ' and ends with a code. Step 2: use the write tool to create token.txt whose content is that code, meaning the text that follows 'token: ' on the line you just read, and nothing else."
if [ "$(trimmed "$WORKROOT/task$n/token.txt")" = "ZORP-7143" ]; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi

# 3: targeted edit, rest of the file unchanged.
n=3; name=edit-config
mkdir -p "$WORKROOT/task$n"
printf '[app]\nmode = development\nretries = 3\n' > "$WORKROOT/task$n/config.ini"
run_task "$n" "$name" \
    "Edit the file config.ini: change the line 'mode = development' to 'mode = production'. Do not change anything else in the file."
f="$WORKROOT/task$n/config.ini"
if grep -q '^mode = production$' "$f" 2>/dev/null \
    && ! grep -q 'development' "$f" 2>/dev/null \
    && grep -q '^retries = 3$' "$f" 2>/dev/null \
    && grep -q '^\[app\]$' "$f" 2>/dev/null; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi

# 4: bash with a directory.
n=4; name=bash-mkdir
mkdir -p "$WORKROOT/task$n"
run_task "$n" "$name" \
    'Use the bash tool to create a directory named build containing a file marker.txt with the text: done  (so the file is build/marker.txt)'
if [ "$(trimmed "$WORKROOT/task$n/build/marker.txt")" = "done" ]; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi

# 5: search across files.
n=5; name=find-needle
mkdir -p "$WORKROOT/task$n"
printf 'nothing here\n' > "$WORKROOT/task$n/alpha.txt"
printf 'the code is NEEDLE-4242 today\n' > "$WORKROOT/task$n/beta.txt"
printf 'also nothing\n' > "$WORKROOT/task$n/gamma.txt"
run_task "$n" "$name" \
    'Three files exist here: alpha.txt, beta.txt, gamma.txt. Exactly one of them contains the string NEEDLE-4242. Find which file contains it (grep or read), then use the write tool to create found.txt containing that file name.'
found="$WORKROOT/task$n/found.txt"
if [ -f "$found" ] && grep -q 'beta\.txt' "$found" 2>/dev/null \
    && ! grep -q 'alpha\.txt' "$found" 2>/dev/null \
    && ! grep -q 'gamma\.txt' "$found" 2>/dev/null; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi

# 6: edit then bash, order matters (a cp before the bump yields a stale bak).
n=6; name=bump-and-copy
mkdir -p "$WORKROOT/task$n"
printf '1.2.3\n' > "$WORKROOT/task$n/version.txt"
run_task "$n" "$name" \
    'The file version.txt contains 1.2.3. First edit version.txt so it contains 1.2.4 instead. Then, after the edit, use the bash tool to run: cp version.txt version.bak'
if [ "$(trimmed "$WORKROOT/task$n/version.txt")" = "1.2.4" ] \
    && [ "$(trimmed "$WORKROOT/task$n/version.bak")" = "1.2.4" ]; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi

# 7: indirect tool selection. The prompt names neither bash nor rm; the
# registry has no delete tool, so the only correct move is choosing bash on
# its own (the T11 dogfood gap: qwen3-1.7b claimed it had no delete tool).
# PASS needs BOTH the file gone and a bash rm call in the transcript.
n=7; name=indirect-delete
mkdir -p "$WORKROOT/task$n"
printf 'scratch\n' > "$WORKROOT/task$n/obsolete.tmp"
run_task "$n" "$name" \
    'Delete the file obsolete.tmp in the current directory.'
t="$EVAL_TRANSCRIPT_DIR/task$n.run$RUN.txt"
if [ ! -e "$WORKROOT/task$n/obsolete.tmp" ] \
    && grep -q 'bash' "$t" 2>/dev/null \
    && grep -Eq '(^| |")rm .*obsolete' "$t" 2>/dev/null; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi
rm -f "$WORKROOT/task$n/obsolete.tmp"

# 8: binary nudge (T19). The only correct path is a bash gzip run; the
# write tool writes text, so a raw-written "archive" is invalid gzip.
# gunzip validity of the result is therefore proof of the path taken.
n=8; name=binary-nudge
mkdir -p "$WORKROOT/task$n"
run_task "$n" "$name" \
    'Create a gzip-compressed file named notes.txt.gz in the current directory. Its DECOMPRESSED content must be exactly: eval-gz-99   (gzip is available via the bash tool).'
if [ "$( { gunzip -c "$WORKROOT/task$n/notes.txt.gz" 2>/dev/null || true; } | tr -d '[:space:]')" = "eval-gz-99" ]; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi

# 9: large-output tail (T19). data.log is ~32,000 chars, far over the
# context-scaled tool-output cap, and the needle is on the LAST line: a
# head-only truncation cannot pass this, only the T19 tail-keep can.
# The prompt describes the last line's shape without quoting a stand-in
# value, for the same reason task 2 does (T29 finding 2).
n=9; name=large-tail
mkdir -p "$WORKROOT/task$n"
{
    i=1
    while [ "$i" -le 399 ]; do
        printf 'line %04d: abcdefghijklmnopqrstuvwxyz-0123456789-abcdefghijklmnopqrstuvwxyz\n' "$i"
        i=$((i + 1))
    done
    printf 'FINAL-LINE: OMEGA-3141\n'
} > "$WORKROOT/task$n/data.log"
run_task "$n" "$name" \
    'Two steps. Step 1: use the bash tool to run exactly: cat data.log   (the output is long and will be truncated in the middle). Step 2: the LAST line of that output begins with "FINAL-LINE: " and ends with a code. Use the write tool to create tail.txt containing that code, meaning the text that follows "FINAL-LINE: " on that last line, and nothing else.'
if [ "$(trimmed "$WORKROOT/task$n/tail.txt")" = "OMEGA-3141" ]; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi

}

# report_round: prints the run's table and appends its score to $SCORES.
report_round() {
    echo "==== results (run $RUN of $EVAL_RUNS) ===="
    printf '%-4s %-14s %-6s %s\n' "task" "name" "result" "seconds"
    printf '%-4s %-14s %-6s %s\n' "----" "--------------" "------" "-------"
    SCORE=0
    while IFS='|' read -r n name res secs; do
        printf '%-4s %-14s %-6s %s\n' "$n" "$name" "$res" "$secs"
        [ "$res" = "PASS" ] && SCORE=$((SCORE + 1))
    done < "$RESULTS"
    cp "$RESULTS" "$EVAL_TRANSCRIPT_DIR/results.run$RUN.txt"
    printf '%s|%s\n' "$RUN" "$SCORE" >> "$SCORES"
    echo "SCORE (run $RUN): $SCORE/9"
}

RUN=1
while [ "$RUN" -le "$EVAL_RUNS" ]; do
    WORKROOT="$EVAL_ROOT/run$RUN"
    mkdir -p "$WORKROOT"
    RESULTS="$WORKROOT/results.txt"
    : > "$RESULTS"
    echo "==== running 9 tasks (run $RUN of $EVAL_RUNS) ===="
    run_round
    report_round
    RUN=$((RUN + 1))
done

echo "==== summary ===="
echo "  model     : $MODEL_GGUF"
echo "  server    : $LLAMA_IMAGE, ctx $CTX, --jinja"
echo "  profile   : $PROMPT_PROFILE, max_tokens $EVAL_MAX_TOKENS"
echo "  transcripts: $EVAL_TRANSCRIPT_DIR/task<n>.run<r>.txt"
echo "  results    : $EVAL_TRANSCRIPT_DIR/results.run<r>.txt"
BELOW=0
while IFS='|' read -r r score; do
    echo "SCORE (run $r): $score/9"
    if [ "$EVAL_MIN" -gt 0 ] && [ "$score" -lt "$EVAL_MIN" ]; then
        BELOW=1
    fi
done < "$SCORES"

if [ "$BELOW" -eq 1 ]; then
    echo "BELOW THRESHOLD (EVAL_MIN=$EVAL_MIN)"
    exit 1
fi
