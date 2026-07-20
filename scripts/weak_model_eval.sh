#!/bin/sh
# T4 weak-model eval harness, operator-run (NOT part of check.sh): measures
# — instead of claiming — how well a small local model drives temur's
# tools. Six fixed tasks run against a llama.cpp server inside a podman pod
# created with --network none (same zero-internet-by-construction setup as
# scripts/offline_demo.sh); every task is scored by a HOST-VERIFIED
# filesystem assertion only — model prose is never evidence.
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
#         EVAL_MIN           minimum passing score; 0 (default) = informational
#                            only, nonzero = exit 1 below the threshold
#         EVAL_TRANSCRIPT_DIR  where per-task transcripts are kept
set -eu
cd "$(dirname "$0")/.."

MUSL_BIN="${MUSL_BIN:-/home/dev/rustcode-target/i686-unknown-linux-musl/release/temur}"
# Pinned llama.cpp server build (tag scheme: server-b<build>); update
# deliberately, never track latest.
LLAMA_IMAGE="${LLAMA_IMAGE:-ghcr.io/ggml-org/llama.cpp:server-b10068}"
APP_IMG=docker.io/i386/debian:stable
BARE_IMG=docker.io/library/busybox:stable
CTX="${CTX:-8192}"
PROMPT_PROFILE="${PROMPT_PROFILE:-compact}"
EVAL_TASK_TIMEOUT="${EVAL_TASK_TIMEOUT:-300}"
EVAL_MIN="${EVAL_MIN:-0}"
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
# max_tokens 2048: thinking models stream reasoning that counts against the
# completion budget — 1024 (the single-call demo's size) truncates
# read-then-reason turns before their tool calls complete.
printf '{"provider":"openai-compat","max_tokens":2048,"prompt_profile":"%s","openai_compat":{"model":"local-gguf","context_window":%s}}\n' \
    "$PROMPT_PROFILE" "$CTX" > "$CFG_DIR/temur/config.json"
echo "profile: $PROMPT_PROFILE   per-task timeout: ${EVAL_TASK_TIMEOUT}s   transcripts: $EVAL_TRANSCRIPT_DIR"

RESULTS="$EVAL_ROOT/results.txt"
: > "$RESULTS"

trimmed() { cat "$1" 2>/dev/null | tr -d '[:space:]' || true; }

# run_task <n> <name> <prompt>: launches a fresh temur --plain process in
# the task's own work subdir. Each task block below mkdirs and seeds its
# work dir right before invoking this — all seeding lives in this script,
# never with the operator.
run_task() {
    n=$1; name=$2; prompt=$3
    work="$EVAL_ROOT/task$n"
    start=$(date +%s)
    printf '%s\n' "$prompt" | timeout "$EVAL_TASK_TIMEOUT" \
        podman run --rm -i --pod "$POD" \
        -v "$(dirname "$MUSL_BIN")":/app:ro \
        -v "$CFG_DIR":/cfg:ro -v "$work":/work \
        -e XDG_CONFIG_HOME=/cfg -w /work "$APP_IMG" \
        /app/temur --plain > "$EVAL_TRANSCRIPT_DIR/task$n.txt" 2>&1 || true
    SECS=$(( $(date +%s) - start ))
}

record() { # record <n> <name> <PASS|FAIL> <secs>
    printf '%s|%s|%s|%s\n' "$1" "$2" "$3" "$4" >> "$RESULTS"
    echo "task $1 ($2): $3 (${4}s)"
}

echo "==== running 6 tasks ===="

# 1: plain write.
n=1; name=write-file
mkdir -p "$EVAL_ROOT/task$n"
run_task "$n" "$name" \
    'Use the write tool to create a file named hello.txt containing exactly this text: hello-eval'
if [ "$(trimmed "$EVAL_ROOT/task$n/hello.txt")" = "hello-eval" ]; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi

# 2: read + extract.
n=2; name=read-extract
mkdir -p "$EVAL_ROOT/task$n"
printf 'token: ZORP-7143\n' > "$EVAL_ROOT/task$n/data.txt"
run_task "$n" "$name" \
    "Two steps. Step 1: use the read tool on data.txt — it has one line like 'token: SOMEVALUE'. Step 2: use the write tool to create token.txt whose content is that SOMEVALUE part (the text after 'token: ')."
if [ "$(trimmed "$EVAL_ROOT/task$n/token.txt")" = "ZORP-7143" ]; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi

# 3: targeted edit, rest of the file unchanged.
n=3; name=edit-config
mkdir -p "$EVAL_ROOT/task$n"
printf '[app]\nmode = development\nretries = 3\n' > "$EVAL_ROOT/task$n/config.ini"
run_task "$n" "$name" \
    "Edit the file config.ini: change the line 'mode = development' to 'mode = production'. Do not change anything else in the file."
f="$EVAL_ROOT/task$n/config.ini"
if grep -q '^mode = production$' "$f" 2>/dev/null \
    && ! grep -q 'development' "$f" 2>/dev/null \
    && grep -q '^retries = 3$' "$f" 2>/dev/null \
    && grep -q '^\[app\]$' "$f" 2>/dev/null; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi

# 4: bash with a directory.
n=4; name=bash-mkdir
mkdir -p "$EVAL_ROOT/task$n"
run_task "$n" "$name" \
    'Use the bash tool to create a directory named build containing a file marker.txt with the text: done  (so the file is build/marker.txt)'
if [ "$(trimmed "$EVAL_ROOT/task$n/build/marker.txt")" = "done" ]; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi

# 5: search across files.
n=5; name=find-needle
mkdir -p "$EVAL_ROOT/task$n"
printf 'nothing here\n' > "$EVAL_ROOT/task$n/alpha.txt"
printf 'the code is NEEDLE-4242 today\n' > "$EVAL_ROOT/task$n/beta.txt"
printf 'also nothing\n' > "$EVAL_ROOT/task$n/gamma.txt"
run_task "$n" "$name" \
    'Three files exist here: alpha.txt, beta.txt, gamma.txt. Exactly one of them contains the string NEEDLE-4242. Find which file contains it (grep or read), then use the write tool to create found.txt containing that file name.'
found="$EVAL_ROOT/task$n/found.txt"
if [ -f "$found" ] && grep -q 'beta\.txt' "$found" 2>/dev/null \
    && ! grep -q 'alpha\.txt' "$found" 2>/dev/null \
    && ! grep -q 'gamma\.txt' "$found" 2>/dev/null; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi

# 6: edit then bash, order matters (a cp before the bump yields a stale bak).
n=6; name=bump-and-copy
mkdir -p "$EVAL_ROOT/task$n"
printf '1.2.3\n' > "$EVAL_ROOT/task$n/version.txt"
run_task "$n" "$name" \
    'The file version.txt contains 1.2.3. First edit version.txt so it contains 1.2.4 instead. Then, after the edit, use the bash tool to run: cp version.txt version.bak'
if [ "$(trimmed "$EVAL_ROOT/task$n/version.txt")" = "1.2.4" ] \
    && [ "$(trimmed "$EVAL_ROOT/task$n/version.bak")" = "1.2.4" ]; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi

echo "==== results ===="

printf '%-4s %-14s %-6s %s\n' "task" "name" "result" "seconds"
printf '%-4s %-14s %-6s %s\n' "----" "--------------" "------" "-------"
SCORE=0
while IFS='|' read -r n name res secs; do
    printf '%-4s %-14s %-6s %s\n' "$n" "$name" "$res" "$secs"
    [ "$res" = "PASS" ] && SCORE=$((SCORE + 1))
done < "$RESULTS"
echo "  model     : $MODEL_GGUF"
echo "  server    : $LLAMA_IMAGE, ctx $CTX, --jinja"
echo "  profile   : $PROMPT_PROFILE"
echo "  transcripts: $EVAL_TRANSCRIPT_DIR/task<n>.txt"
echo "SCORE: $SCORE/6"

if [ "$EVAL_MIN" -gt 0 ] && [ "$SCORE" -lt "$EVAL_MIN" ]; then
    echo "BELOW THRESHOLD (EVAL_MIN=$EVAL_MIN)"
    exit 1
fi
