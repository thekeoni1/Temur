#!/bin/sh
# T4 weak-model eval harness, operator-run (NOT part of check.sh): measures
# — instead of claiming — how well a small local model drives temur's
# tools. Nine scored tasks run against a llama.cpp server inside a podman
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
# Task 10 (resume-feedback, T58) is the one task scored from the TRANSCRIPT
# rather than the filesystem, and the one reported OUTSIDE the /9. It is a
# conversational request that only implies a file, which is the shape the
# nine imperative tool instructions cannot see: D25 (2026-09-09) is the
# dogfood it reproduces, where the model answered "I can't directly read or
# process files like a resume" and called nothing. Its result gets its own
# line so the nine-task denominator, and every row already published
# against it, stay comparable.
#
# Tasks 11 (memo-docx) and 12 (summary-pdf, T60) are the two document
# tasks, reported OUTSIDE the /9 the same way. Each needs the file the
# prompt asked for to exist AND a read-back to yield a token from the
# prompt: the file is read through temur's own read tool, so the docx and
# PDF parsers compiled into the binary under test are what judge it, run
# through the tools test binary built beside it (this box has no unzip
# and no pdftotext, and a host tool would not be temur's read path
# anyway). A text file saved under the extension fails the read-back,
# because the zip and PDF parsers reject it; that is the gap the two
# tasks exist to see.
#
# Nothing is ever pulled or downloaded here: preflight prints the exact
# pull command and exits if an image is missing.
#
# Usage:  MODEL_GGUF=/path/to/model.gguf scripts/weak_model_eval.sh
# Knobs:  MUSL_BIN           path to the musl-static temur binary
#         LLAMA_IMAGE        server image (pinned default below)
#         CTX                server context size, mirrored into context_window
#         PROMPT_PROFILE     temur prompt profile for the run (default compact)
#         EVAL_TASK_TIMEOUT  seconds allowed per task, ENFORCED (default
#                            1200); 0 disables the bound entirely. A task
#                            the bound kills is recorded FAIL with a
#                            TIMEOUT@<n>s note in the results table.
#         EVAL_MAX_TOKENS    per-turn completion budget (default 3072)
#         EVAL_RUNS          how many times the nine tasks repeat (default 1);
#                            the server and the pod are built ONCE and shared
#                            across runs, so only model sampling varies
#         EVAL_ONLY          run only task <n> (1..13) instead of the nine
#                            plus tasks 10 to 12. The SCORE line and the archived
#                            results file both carry "(EVAL_ONLY=<n>, not a
#                            published row)", so a one-task score can never
#                            be read as a nine-task one. Unset (the default)
#                            is the same run as before.
#         EVAL_MIN           minimum passing score; 0 (default) = informational
#                            only, nonzero = exit 1 when ANY run is below it
#         EVAL_KEEP_ALL      1 = archive every task's artifacts, not just the
#                            failures (default 0)
#         EVAL_TRANSCRIPT_DIR  where per-task transcripts, per-run results and
#                            kept artifacts are stored
#         CHAT_TEMPLATE_FILE  path to a .jinja chat template to serve the
#                            model with INSTEAD of its bundled one. Unset
#                            (the default) is byte-identical to before.
#                            Scores measured under a substitute template are
#                            NOT comparable to native-template runs, and the
#                            banner says so at every opportunity: see the
#                            warning below and docs/OFFLINE.md.
set -eu
cd "$(dirname "$0")/.."

MUSL_BIN="${MUSL_BIN:-/home/dev/rustcode-target/i686-unknown-linux-musl/release/temur}"
# Pinned llama.cpp server build (tag scheme: server-b<build>); update
# deliberately, never track latest.
LLAMA_IMAGE="${LLAMA_IMAGE:-ghcr.io/ggml-org/llama.cpp:server-b10438}"
APP_IMG=docker.io/i386/debian:stable
BARE_IMG=docker.io/library/busybox:stable
# Task 10's seed document, found relative to this script's own directory
# via the cd above, exactly like every other input here and never by
# absolute path, so the script still runs from a clone anywhere.
RESUME_FIXTURE=tests/fixtures/office/sample-resume.pdf

# --- task 13 (T62 P1) -------------------------------------------------------
# The fixture is BUILT here rather than committed. A PDF small enough to sit
# in the repo is small enough for one read call to show whole, and task 13
# exists to measure whether grep saves the model a second read, so a
# committed PDF would be passed by every arm on its size alone. Measured:
# read's 28 KB MAX_BYTES caps a default read at line 414 whatever the limit,
# and at pad 100 the sentinel is at line 580 of 588.
#
# The padding rule, per Ruling T62-3: ONE fixed paragraph repeated a fixed
# number of times, substituted where the committed base marks it, with the
# target section after it. The base is the only committed part.
T13_MD_BASE=tests/fixtures/office/ferry-review.md
T13_PAD_COUNT=100
T13_PAD_PARA='Sailing notes carried forward from the previous review. The north landing approach is unchanged, the tide window is unchanged, and the pilotage requirement is unchanged. The board has asked that these notes be reproduced in full each year so that any change to them is visible against the prior text rather than summarised away by the clerk.'
# Pre-registered before launch, and deliberately absent from the prompt, so
# it can only reach the transcript out of the document.
T13_SENTINEL='Kestrel-class hull survey'
# Ruling T62-4: temur's PDF writer emits UNCOMPRESSED content streams, so a
# PDF it wrote carries its prose as plain bytes and grep searches it as text
# (grep.rs:113 skips a file only for a NUL in its first 4,096 bytes). Real
# PDFs are flate-compressed and do carry early NULs, which is why grep skips
# them. Task 13 measures what a model does when grep skips a document, so the
# fixture is compressed after the write path produces it. Bytes only; the
# read-back is asserted byte-identical below.
T13_COMPRESS=tests/fixtures/office/compress_pdf_streams.py
# Written ONCE by the control arm and then asserted, so every arm reads the
# same bytes: T13_GENERATE=1 builds it, and any later arm must be given the
# path and both hashes the control arm printed.
T13_GENERATE="${T13_GENERATE:-0}"
T13_PDF="${T13_PDF:-}"
T13_PDF_SHA256="${T13_PDF_SHA256:-}"
T13_MD_SHA256="${T13_MD_SHA256:-}"
# T60: the read-back for tasks 11 and 12 runs temur's read tool through
# the tools test binary that `cargo test --release --target
# i686-unknown-linux-musl --no-run` leaves beside the musl binary (the
# newest tools-<hash> in deps/, found the way scripts/check.sh finds it).
# The read tool prints nothing a --plain transcript carries, and --mock
# persists nothing, so a test hook is the one way to see what the parsers
# make of a file without a model in the loop.
READBACK_BIN=$(ls -t "$(dirname "$MUSL_BIN")/deps/tools-"* 2>/dev/null | grep -v '\.d$' | head -1 || true)
CTX="${CTX:-8192}"
PROMPT_PROFILE="${PROMPT_PROFILE:-compact}"
EVAL_TASK_TIMEOUT="${EVAL_TASK_TIMEOUT:-1200}"
EVAL_MAX_TOKENS="${EVAL_MAX_TOKENS:-3072}"
EVAL_RUNS="${EVAL_RUNS:-1}"
EVAL_MIN="${EVAL_MIN:-0}"
EVAL_KEEP_ALL="${EVAL_KEEP_ALL:-0}"
EVAL_ONLY="${EVAL_ONLY:-}"
EVAL_TRANSCRIPT_DIR="${EVAL_TRANSCRIPT_DIR:-/tmp/temur-weak-eval}"
# T34: substitute chat template, off unless set. Proven shape from the
# template experiment of 2026-08-17, which took Phi-4-mini from 0/9 to 4/9
# by serving it a Qwen2.5 template instead of its own broken one.
CHAT_TEMPLATE_FILE="${CHAT_TEMPLATE_FILE:-}"
TMPL_DEST=/tmpl.jinja
# sha256 of that file, computed ONCE at preflight. A path alone does not
# identify a template: these files are fetched from an upstream repo at a
# tag and edited by hand while a recipe is being found, so an archived
# result that names only a path can stop being reproducible without
# anything looking wrong. Empty while no substitute template is in use.
TMPL_SHA=""
POD=temur-weak-eval

# The loud banner, printed at bring-up and again in the summary. Measured,
# not hypothetical: under a substitute Qwen2.5 template on 2026-08-17,
# gemma-3-4b produced zero tool calls and spent 150-430s per task inventing
# plausible tool results, including the contents of a file that does not
# exist. A wrong template turns a silent failure into an expensive one.
template_banner() {
    [ -n "$CHAT_TEMPLATE_FILE" ] || return 0
    echo "WARNING: substitute chat template in use ($CHAT_TEMPLATE_FILE)."
    echo "  A template the model was not trained on can produce confident, WRONG"
    echo "  output: under a substitute template gemma-3-4b produced zero tool calls"
    echo "  and spent minutes per task hallucinating plausible tool results."
    echo "  Scores are NOT comparable to native-template runs."
}

# The template in force, as one self-describing phrase: path AND content
# hash, so an archived results file identifies the exact bytes it was
# measured under rather than a path that may since have changed.
template_desc() {
    if [ -n "$CHAT_TEMPLATE_FILE" ]; then
        echo "SUBSTITUTE $CHAT_TEMPLATE_FILE (sha256 $TMPL_SHA)"
    else
        echo "bundled (model default)"
    fi
}

# One line naming the template in force, for the run banner and for the
# header of every archived results file.
template_line() {
    if [ -n "$CHAT_TEMPLATE_FILE" ]; then
        echo "# chat template: $(template_desc) (NOT comparable to native-template runs)"
    else
        echo "# chat template: $(template_desc)"
    fi
}

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

[ -f "$RESUME_FIXTURE" ] || { echo "FAIL: task 10 fixture not found: $RESUME_FIXTURE"; exit 1; }
echo "OK: task 10 fixture present ($RESUME_FIXTURE)"
# Task 13's fixture is built, so what has to exist here is its SOURCE and the
# padding marker the builder substitutes.
[ -f "$T13_MD_BASE" ] || { echo "FAIL: task 13 fixture source not found: $T13_MD_BASE"; exit 1; }
grep -q '^<!-- PADDING -->$' "$T13_MD_BASE" || { echo "FAIL: $T13_MD_BASE has no padding marker"; exit 1; }
grep -qF -- "$T13_SENTINEL" "$T13_MD_BASE" || { echo "FAIL: $T13_MD_BASE does not carry the task 13 sentinel"; exit 1; }
[ -f "$T13_COMPRESS" ] || { echo "FAIL: task 13 stream compressor not found: $T13_COMPRESS"; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo "FAIL: task 13 needs python3 on the host for $T13_COMPRESS"; exit 1; }
echo "OK: task 13 fixture source present ($T13_MD_BASE, pad $T13_PAD_COUNT, compressor $T13_COMPRESS)"

# NEVER auto-pull. Missing image => print the exact command and stop.
for img in "$LLAMA_IMAGE" "$APP_IMG" "$BARE_IMG"; do
    podman image exists "$img" || { echo "FAIL: image not present locally: $img"; echo "  fetch it first (on a connected machine):  podman pull $img"; exit 1; }
done
echo "OK: all images present locally (nothing will be pulled)"

if [ -n "$CHAT_TEMPLATE_FILE" ]; then
    [ -f "$CHAT_TEMPLATE_FILE" ] || { echo "FAIL: CHAT_TEMPLATE_FILE not found: $CHAT_TEMPLATE_FILE"; exit 1; }
    [ -r "$CHAT_TEMPLATE_FILE" ] || { echo "FAIL: CHAT_TEMPLATE_FILE not readable: $CHAT_TEMPLATE_FILE"; exit 1; }
    TMPL_SHA=$(sha256sum "$CHAT_TEMPLATE_FILE" | cut -d' ' -f1)
    echo "OK: chat template file present ($CHAT_TEMPLATE_FILE, sha256 $TMPL_SHA)"
fi

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
# T59: a one-task run, validated here for the same reason the knobs above
# are. The marker it puts on every score line is applied in report_round.
if [ -n "$EVAL_ONLY" ]; then
    case "$EVAL_ONLY" in
        *[!0-9]*) echo "FAIL: EVAL_ONLY must be a task number 1..13 (got '$EVAL_ONLY')"; exit 1 ;;
    esac
    if [ "$EVAL_ONLY" -lt 1 ] || [ "$EVAL_ONLY" -gt 13 ]; then
        echo "FAIL: EVAL_ONLY must be a task number 1..13 (got '$EVAL_ONLY')"; exit 1
    fi
fi
# T60: tasks 11 and 12 cannot be scored without the read-back binary, so
# a run that would reach them refuses here rather than after the model
# has spent its minutes on them.
if [ -z "$EVAL_ONLY" ] || [ "$EVAL_ONLY" = 11 ] || [ "$EVAL_ONLY" = 12 ]; then
    [ -n "$READBACK_BIN" ] && [ -x "$READBACK_BIN" ] || {
        echo "FAIL: no tools test binary beside $MUSL_BIN (tasks 11 and 12 read documents back through it)"
        echo "  build it first:  cargo test --release --target i686-unknown-linux-musl --no-run"
        exit 1
    }
    echo "OK: read-back binary present ($READBACK_BIN)"
fi

echo "==== pod bring-up (--network none) ===="

podman pod rm -f "$POD" >/dev/null 2>&1 || true
podman pod create --name "$POD" --network none >/dev/null
TMPL_MOUNT=""
TMPL_ARG=""
if [ -n "$CHAT_TEMPLATE_FILE" ]; then
    TMPL_MOUNT="-v $CHAT_TEMPLATE_FILE:$TMPL_DEST:ro"
    TMPL_ARG="--chat-template-file $TMPL_DEST"
fi
# shellcheck disable=SC2086  # $TMPL_* are deliberately word-split
podman run -d --pod "$POD" --name "$POD-llama" \
    -v "$MODEL_GGUF":/model.gguf:ro $TMPL_MOUNT "$LLAMA_IMAGE" \
    -m /model.gguf -c "$CTX" --jinja $TMPL_ARG --host 127.0.0.1 --port 8080 >/dev/null
echo "server starting (ctx $CTX, --jinja${CHAT_TEMPLATE_FILE:+, template $CHAT_TEMPLATE_FILE})"
template_banner

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
if [ "$EVAL_TASK_TIMEOUT" -gt 0 ]; then
    TIMEOUT_BANNER="${EVAL_TASK_TIMEOUT}s (enforced)"
else
    TIMEOUT_BANNER="disabled"
fi
echo "profile: $PROMPT_PROFILE   per-task timeout: $TIMEOUT_BANNER   max_tokens: $EVAL_MAX_TOKENS"
echo "runs: $EVAL_RUNS   transcripts: $EVAL_TRANSCRIPT_DIR"
[ -z "$EVAL_ONLY" ] || echo "EVAL_ONLY=$EVAL_ONLY: only task $EVAL_ONLY runs; its score is not a published row"
template_line

SCORES="$EVAL_ROOT/scores.txt"
: > "$SCORES"
# Task 10's per-run result, kept beside the scores and printed the same
# way, but never summed into them: the /9 stays the /9.
T10LOG="$EVAL_ROOT/task10.txt"
: > "$T10LOG"
# T59: task 5's glob-title line per run, reported the same way as D22.
T59LOG="$EVAL_ROOT/task5-glob.txt"
: > "$T59LOG"
# T60: tasks 11 and 12, one line each per run, reported the same way.
T60LOG="$EVAL_ROOT/documents.txt"
T62LOG="$EVAL_ROOT/task13-pdf-section.txt"
: > "$T60LOG"

trimmed() { cat "$1" 2>/dev/null | tr -d '[:space:]' || true; }

# T59: which tasks this run executes. Unset EVAL_ONLY is every task, which
# is the run every published row was measured under.
wanted() { [ -z "$EVAL_ONLY" ] || [ "$EVAL_ONLY" = "$1" ]; }

# T33: how the per-task bound is actually enforced. Until now the line
# was `timeout $EVAL_TASK_TIMEOUT podman run ...`, which never bound: on
# expiry `timeout` signals the podman CLIENT, and the client neither dies
# nor stops the container. T32 measured ten tasks overrunning the 300s
# cap, worst 994s. Measured again directly on podman 4.9.3, 2026-08-16, a
# 30s container against a 5s bound:
#   timeout (SIGTERM to client)   32s, never bound
#   timeout -s KILL / -k          5-8s, but the container SURVIVES the
#                                 client and keeps running in the pod
#   podman run --timeout          7s, container killed by conmon, and
#                                 --rm still removed it: no orphan
# So the bound is podman's own --timeout, which conmon enforces on the
# container rather than on the client. The outer `timeout -s KILL` stays
# as a backstop only, at a grace above the real cap, for the case where
# conmon itself wedges; it is what the sweep after each task cleans up
# after. 0 disables both, since podman reads --timeout 0 as "no bound"
# and a backstop with no bound to back would kill every task at the
# grace.
TIMEOUT_BACKSTOP=60
TIMED_OUT=0
if [ "$EVAL_TASK_TIMEOUT" -gt 0 ]; then
    BOUND_ARG="--timeout $EVAL_TASK_TIMEOUT"
    BOUND_CMD="timeout -s KILL $(( EVAL_TASK_TIMEOUT + TIMEOUT_BACKSTOP ))"
else
    BOUND_ARG=""
    BOUND_CMD=""
fi

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
    cname="temur-eval-t$n-r$RUN"
    # A leftover of this name would make the run below fail on the name
    # rather than on the task, so sweep before as well as after.
    podman rm -f "$cname" >/dev/null 2>&1 || true
    TIMED_OUT=0
    rc=0
    start=$(date +%s)
    # T46: --allow-mutations states the allow path rather than leaving it
    # inferred from "stdin is a pipe, so nothing asks". Every task here
    # writes files; a run that started prompting would score zeros and the
    # published OFFLINE.md matrix would be wrong rather than absent.
    # shellcheck disable=SC2086  # $BOUND_* are deliberately word-split
    printf '%s\n' "$prompt" | $BOUND_CMD \
        podman run --rm -i --name "$cname" $BOUND_ARG --pod "$POD" \
        -v "$(dirname "$MUSL_BIN")":/app:ro \
        -v "$CFG_DIR":/cfg:ro -v "$work":/work -v "$state":/state \
        -e XDG_CONFIG_HOME=/cfg -e XDG_STATE_HOME=/state -w /work "$APP_IMG" \
        /app/temur --allow-mutations --plain > "$EVAL_TRANSCRIPT_DIR/task$n.run$RUN.txt" 2>&1 || rc=$?
    SECS=$(( $(date +%s) - start ))
    # Only the backstop can leave a container behind (measured); conmon's
    # own kill honors --rm. Unconditional, so the next task never inherits
    # a live temur holding the shared server busy.
    podman rm -f "$cname" >/dev/null 2>&1 || true
    # A nonzero exit at or past the cap is the bound firing. Neither half
    # alone is enough: the cap alone would mislabel a task that finished
    # normally just under it, and a nonzero exit alone is any podman error.
    if [ "$EVAL_TASK_TIMEOUT" -gt 0 ] && [ "$rc" -ne 0 ] && [ "$SECS" -ge "$EVAL_TASK_TIMEOUT" ]; then
        TIMED_OUT=1
    fi
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

# read_back <file>: what temur's own read tool yields for <file>, or its
# error, printed by the tools test binary's eval_read_back hook between
# two marker lines that are stripped here. The app image and no network,
# the same as a task; the file's directory is mounted read-only.
read_back() {
    podman run --rm --network none \
        -v "$(dirname "$READBACK_BIN")":/suites:ro -v "$(dirname "$1")":/doc:ro \
        -e TEMUR_EVAL_READBACK="/doc/$(basename "$1")" \
        -e TEMUR_EVAL_READBACK_OFFSET="${2:-}" "$APP_IMG" \
        "/suites/$(basename "$READBACK_BIN")" eval_read_back --exact --nocapture 2>&1 \
        | sed -n '/^READBACK-BEGIN$/,/^READBACK-END$/p' | grep -v '^READBACK-' || true
}

# write_pdf <markdown> <out.pdf>: the fixture, written through temur's own
# write path by the tools binary's eval_write_pdf hook. Same mechanism as
# read_back above, for the same reason: write is a TOOL, reachable only from
# a model turn, and a model cannot produce an exact fixture. Same code, same
# target, same image as the binary under test.
write_pdf() {
    outdir=$(dirname "$2")
    podman run --rm --network none \
        -v "$(dirname "$READBACK_BIN")":/suites:ro -v "$(dirname "$1")":/src:ro \
        -v "$outdir":/out \
        -e TEMUR_EVAL_WRITE_SRC="/src/$(basename "$1")" \
        -e TEMUR_EVAL_WRITE_PDF="/out/$(basename "$2")" "$APP_IMG" \
        "/suites/$(basename "$READBACK_BIN")" eval_write_pdf --exact --nocapture 2>&1 \
        | sed -n '/^WRITEPDF-BEGIN$/,/^WRITEPDF-END$/p' | grep -v '^WRITEPDF-' || true
}

# read_back_all <file>: every page of temur's read of <file>, concatenated, by
# following the pagination footer until the read stops offering a next offset.
# Preflight (ii) needs the WHOLE extracted text: the read tool's byte cap fires
# at any limit, so a single call proves nothing about the pages past the first,
# and task 13's target section is one of those pages.
read_back_all() {
    rba_off=1
    rba_pages=0
    while [ "$rba_pages" -lt 50 ]; do
        rba_out=$(read_back "$1" "$rba_off")
        printf '%s\n' "$rba_out"
        rba_pages=$((rba_pages + 1))
        rba_next=$(printf '%s\n' "$rba_out" | sed -n 's/.*Use offset=\([0-9]*\) to continue.*/\1/p' | tail -1)
        [ -n "$rba_next" ] || return 0
        rba_off="$rba_next"
    done
    echo "FAIL: read_back_all did not finish $1 in 50 pages"
    return 1
}

# compress_pdf <in.pdf> <out.pdf>: flate-compress the content streams of a PDF
# the write path produced, so the fixture has the byte shape of a real PDF.
# Runs on the host, the way make_bound_fixtures.py does; see T13_COMPRESS.
compress_pdf() {
    python3 "$T13_COMPRESS" "$1" "$2"
}

# build_t13_md <out>: the committed base with the padding rule applied. The
# ONLY builder of this markdown; its sha256 is asserted against the one the
# generating arm recorded, so a drift here stops the task instead of quietly
# producing a different document.
build_t13_md() {
    : > "$1"
    while IFS= read -r line; do
        if [ "$line" = "<!-- PADDING -->" ]; then
            i=0
            while [ "$i" -lt "$T13_PAD_COUNT" ]; do
                printf '%s\n\n' "$T13_PAD_PARA" >> "$1"
                i=$((i + 1))
            done
        else
            printf '%s\n' "$line" >> "$1"
        fi
    done < "$T13_MD_BASE"
}

sha() { sha256sum "$1" | cut -d' ' -f1; }

# t13_fixture <workdir>: leaves ferry-review.pdf in <workdir>, or exits.
#
# Generating arm (T13_GENERATE=1, which must be the CONTROL arm because the
# fixture has to be the control binary's output): builds the markdown, writes
# the PDF twice to show the write is deterministic, runs the preflight read at
# the DEFAULT window, and STOPS if the sentinel is inside that read, because a
# sentinel the first read already shows makes the task undiscriminating. Then
# it prints the three values the other arms must be given.
#
# Every other arm: rebuilds the markdown, asserts its sha, asserts the PDF's
# sha, and copies those exact bytes in.
t13_fixture() {
    dest="$1/ferry-review.pdf"
    src="$1/ferry-review.src.md"
    build_t13_md "$src"
    got_md=$(sha "$src")
    if [ "$T13_GENERATE" = "1" ]; then
        [ -z "$T13_PDF" ] || { echo "FAIL: T13_GENERATE=1 and T13_PDF both set"; exit 1; }
        # The uncompressed PDF the write path produces, kept for the read-back
        # comparison below. Same basename as the fixture so read_back mounts it
        # at the same container path and the two outputs are comparable as bytes.
        raw="$1/raw"
        mkdir -p "$raw"
        write_pdf "$src" "$raw/$(basename "$dest")" > "$EVAL_TRANSCRIPT_DIR/task13.write.txt" 2>&1
        [ -s "$raw/$(basename "$dest")" ] || { echo "FAIL: task 13 fixture was not written; see task13.write.txt"; exit 1; }
        compress_pdf "$raw/$(basename "$dest")" "$dest" >> "$EVAL_TRANSCRIPT_DIR/task13.write.txt" 2>&1
        [ -s "$dest" ] || { echo "FAIL: task 13 fixture compression produced nothing; see task13.write.txt"; exit 1; }
        first=$(sha "$dest")
        # Determinism over the WHOLE pipeline, write then compress. The second
        # write must keep the .pdf extension: the write tool dispatches on it,
        # so a name like ferry-review.pdf.again is written as PLAIN TEXT and the
        # comparison would be a PDF against a text file. The first P1 instrument
        # smoke caught exactly that.
        det="$1/det"
        mkdir -p "$det"
        write_pdf "$src" "$det/$(basename "$dest")" >/dev/null 2>&1
        compress_pdf "$det/$(basename "$dest")" "$det/c-$(basename "$dest")" >/dev/null 2>&1
        second=$(sha "$det/c-$(basename "$dest")")
        [ "$first" = "$second" ] || { echo "FAIL: task 13 fixture build is not deterministic ($first vs $second)"; exit 1; }
        rm -rf "$det"
        # Preflight STOP (i), Ruling T62-4: grep skips a file only when a NUL
        # byte falls in its first 4,096 bytes (src/tools/grep.rs:113). Without
        # one, grep searches the PDF as text and finds the sentence in a content
        # stream, so the task's premise is false and it measures nothing.
        t13_nul=$(head -c 4096 "$dest" | tr -dc '\0' | wc -c | tr -d ' ')
        [ "$t13_nul" -ge 1 ] || {
            echo "FAIL: task 13 preflight STOP: no NUL byte in the fixture's first 4096 bytes, so grep (src/tools/grep.rs:113) would search it as text and the task's premise is false"
            exit 1
        }
        # Preflight STOP (ii), Ruling T62-4: compression changed the bytes and
        # nothing else, so temur's read of the compressed fixture must equal its
        # read of the uncompressed one. Both must be non-empty, or the
        # comparison passes vacuously.
        read_back_all "$raw/$(basename "$dest")" > "$EVAL_TRANSCRIPT_DIR/task13.readback-plain.txt" 2>&1
        read_back_all "$dest" > "$EVAL_TRANSCRIPT_DIR/task13.readback-flate.txt" 2>&1
        [ -s "$EVAL_TRANSCRIPT_DIR/task13.readback-plain.txt" ] && [ -s "$EVAL_TRANSCRIPT_DIR/task13.readback-flate.txt" ] || {
            echo "FAIL: task 13 preflight STOP: a read-back is empty, so the read-back comparison would be vacuous"
            exit 1
        }
        # The comparison has to cover the section the task asks about, which is
        # past the first window. If the sentinel is absent from the full paged
        # read, the paging stopped early and the comparison proves nothing.
        grep -qF -- "$T13_SENTINEL" "$EVAL_TRANSCRIPT_DIR/task13.readback-flate.txt" || {
            echo "FAIL: task 13 preflight STOP: the full paged read of the fixture does not reach the sentinel, so the read-back comparison does not cover the target section"
            exit 1
        }
        cmp -s "$EVAL_TRANSCRIPT_DIR/task13.readback-plain.txt" "$EVAL_TRANSCRIPT_DIR/task13.readback-flate.txt" || {
            echo "FAIL: task 13 preflight STOP: the read of the compressed fixture differs from the read of the uncompressed one"
            diff "$EVAL_TRANSCRIPT_DIR/task13.readback-plain.txt" "$EVAL_TRANSCRIPT_DIR/task13.readback-flate.txt" | head -20
            exit 1
        }
        rm -rf "$raw"
        # The sentinel STOP below is about the DEFAULT window, so it reads the
        # fixture the way a model's first read sees it.
        read_back "$dest" > "$EVAL_TRANSCRIPT_DIR/task13.preflight.txt" 2>&1
        # Preflight STOP, unchanged: the control binary's own read at the
        # default window must NOT already show the sentinel.
        if grep -qF -- "$T13_SENTINEL" "$EVAL_TRANSCRIPT_DIR/task13.preflight.txt"; then
            echo "FAIL: task 13 preflight STOP: the sentinel is inside the first default read, so the task cannot discriminate"
            exit 1
        fi
        echo "task13 fixture: $(wc -c < "$dest") bytes, md sha $got_md, pdf sha $first"
        echo "task13 preflight: $t13_nul NUL bytes in the first 4096, so grep skips it (grep.rs:113); every page of the read-back byte-identical to the uncompressed PDF, sentinel reached"
        echo "task13 preflight: sentinel NOT in the default read; first read ends: $(grep -o '(Output capped[^)]*)' "$EVAL_TRANSCRIPT_DIR/task13.preflight.txt" | head -1)"
        echo "task13: pass these to every other arm:"
        echo "  T13_PDF=<this run's copy>  T13_PDF_SHA256=$first  T13_MD_SHA256=$got_md"
        cp "$dest" "$EVAL_TRANSCRIPT_DIR/ferry-review.pdf"
    else
        [ -n "$T13_PDF" ] && [ -n "$T13_PDF_SHA256" ] && [ -n "$T13_MD_SHA256" ] || {
            echo "FAIL: task 13 needs T13_PDF, T13_PDF_SHA256 and T13_MD_SHA256 (or T13_GENERATE=1 on the control arm)"
            exit 1
        }
        [ "$got_md" = "$T13_MD_SHA256" ] || {
            echo "FAIL: task 13 markdown sha $got_md does not match T13_MD_SHA256 $T13_MD_SHA256"
            exit 1
        }
        [ -f "$T13_PDF" ] || { echo "FAIL: T13_PDF not found: $T13_PDF"; exit 1; }
        got_pdf=$(sha "$T13_PDF")
        [ "$got_pdf" = "$T13_PDF_SHA256" ] || {
            echo "FAIL: task 13 pdf sha $got_pdf does not match T13_PDF_SHA256 $T13_PDF_SHA256"
            exit 1
        }
        cp "$T13_PDF" "$dest"
        echo "task13 fixture: reused $T13_PDF, sha $got_pdf asserted equal across arms"
    fi
    # The source markdown must not be visible to the model: it holds the
    # sentinel in plain text and would make a grep of the directory trivial.
    rm -f "$src"
}

# score_document <file> <needle>: PASS needs BOTH the file and the needle
# in its read-back; either half missing is a FAIL that says which, and a
# failed read-back quotes the parser's own sentence so the results file
# records what the model actually left on disk. Sets T60_RES and
# T60_NOTE, the task 10 shape.
score_document() {
    T60_RES=FAIL
    if [ "$TIMED_OUT" = "1" ]; then
        T60_NOTE="TIMEOUT@${EVAL_TASK_TIMEOUT}s"
    elif [ ! -f "$1" ]; then
        T60_NOTE="no $(basename "$1") in the work dir"
    else
        back=$(read_back "$1")
        if printf '%s\n' "$back" | grep -qF -- "$2"; then
            T60_RES=PASS
            T60_NOTE="read back $2"
        else
            T60_NOTE="$(basename "$1") exists but its read-back has no $2: $(printf '%s' "$back" | tr '\n' ' ' | cut -c1-200)"
        fi
    fi
    T60_NOTE="$T60_NOTE (${SECS}s)"
}

record() { # record <n> <name> <PASS|FAIL> <secs>
    res=$3
    note=""
    # A task the bound killed is a FAIL regardless of what its assertion
    # found: whatever is on disk was produced by a run that did not finish
    # under the stated conditions, so scoring it as a pass would publish a
    # number the conditions did not actually produce.
    if [ "$TIMED_OUT" = "1" ]; then
        res=FAIL
        note="TIMEOUT@${EVAL_TASK_TIMEOUT}s"
    fi
    printf '%s|%s|%s|%s|%s\n' "$1" "$2" "$res" "$4" "$note" >> "$RESULTS"
    echo "task $1 ($2): $res${note:+ [$note]} (${4}s)"
    archive_task "$1" "$res"
}

# run_round: the nine tasks, in order, against the already-running server.
# Called once per EVAL_RUNS; every task gets a fresh work dir under the
# run's own root, so no run can see another's leftovers.
run_round() {
T10_RES=""
T10_NOTE=""
T59_LINE=""
T60_11_LINE=""
T60_12_LINE=""
T62_13_LINE=""

# 1: plain write.
n=1; name=write-file
if wanted "$n"; then
mkdir -p "$WORKROOT/task$n"
run_task "$n" "$name" \
    'Use the write tool to create a file named hello.txt containing exactly this text: hello-eval'
if [ "$(trimmed "$WORKROOT/task$n/hello.txt")" = "hello-eval" ]; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi
fi

# 2: read + extract. The prompt describes the SHAPE of the line without
# quoting a stand-in value: a literal placeholder is copyable, and three
# models copied one instead of the value it stood for (T29 finding 2),
# which made this task partly a measure of placeholder literalism.
n=2; name=read-extract
if wanted "$n"; then
mkdir -p "$WORKROOT/task$n"
printf 'token: ZORP-7143\n' > "$WORKROOT/task$n/data.txt"
run_task "$n" "$name" \
    "Two steps. Step 1: use the read tool on data.txt. It holds a single line that begins with 'token: ' and ends with a code. Step 2: use the write tool to create token.txt whose content is that code, meaning the text that follows 'token: ' on the line you just read, and nothing else."
if [ "$(trimmed "$WORKROOT/task$n/token.txt")" = "ZORP-7143" ]; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi
fi

# 3: targeted edit, rest of the file unchanged.
n=3; name=edit-config
if wanted "$n"; then
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
fi

# 4: bash with a directory.
n=4; name=bash-mkdir
if wanted "$n"; then
mkdir -p "$WORKROOT/task$n"
run_task "$n" "$name" \
    'Use the bash tool to create a directory named build containing a file marker.txt with the text: done  (so the file is build/marker.txt)'
if [ "$(trimmed "$WORKROOT/task$n/build/marker.txt")" = "done" ]; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi
fi

# 5: search across files.
n=5; name=find-needle
if wanted "$n"; then
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
# T59: which glob pattern the model sent, host-read from the transcript's
# first ToolEnd glob line ("  <mark> glob: <title>", the title being the
# raw pattern argument). comma-joined means a comma outside braces, which
# globset reads as one literal filename and which the model sends when it
# lists the three names the prompt gave it. Reported, never scored.
t="$EVAL_TRANSCRIPT_DIR/task$n.run$RUN.txt"
if grep -Eq '^  [^ ]+ glob: ' "$t" 2>/dev/null; then
    T59_GLOB=$(sed -n 's/^  [^ ][^ ]* glob: \(.*\)$/\1/p' "$t" | head -1)
    if printf '%s' "$T59_GLOB" | sed 's/{[^}]*}//g' | grep -q ','; then
        T59_LINE="find-needle glob=$T59_GLOB comma-joined"
    else
        T59_LINE="find-needle glob=$T59_GLOB plain"
    fi
else
    T59_LINE="find-needle glob=none none"
fi
echo "T59 (run $RUN): $T59_LINE"
fi

# 6: edit then bash, order matters (a cp before the bump yields a stale bak).
n=6; name=bump-and-copy
if wanted "$n"; then
mkdir -p "$WORKROOT/task$n"
printf '1.2.3\n' > "$WORKROOT/task$n/version.txt"
run_task "$n" "$name" \
    'The file version.txt contains 1.2.3. First edit version.txt so it contains 1.2.4 instead. Then, after the edit, use the bash tool to run: cp version.txt version.bak'
if [ "$(trimmed "$WORKROOT/task$n/version.txt")" = "1.2.4" ] \
    && [ "$(trimmed "$WORKROOT/task$n/version.bak")" = "1.2.4" ]; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi
fi

# 7: indirect tool selection. The prompt names neither bash nor rm; the
# registry has no delete tool, so the only correct move is choosing bash on
# its own (the T11 dogfood gap: qwen3-1.7b claimed it had no delete tool).
# PASS needs BOTH the file gone and a bash rm call in the transcript.
n=7; name=indirect-delete
if wanted "$n"; then
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
fi

# 8: binary nudge (T19). The only correct path is a bash gzip run; the
# write tool writes text, so a raw-written "archive" is invalid gzip.
# gunzip validity of the result is therefore proof of the path taken.
n=8; name=binary-nudge
if wanted "$n"; then
mkdir -p "$WORKROOT/task$n"
run_task "$n" "$name" \
    'Create a gzip-compressed file named notes.txt.gz in the current directory. Its DECOMPRESSED content must be exactly: eval-gz-99   (gzip is available via the bash tool).'
if [ "$( { gunzip -c "$WORKROOT/task$n/notes.txt.gz" 2>/dev/null || true; } | tr -d '[:space:]')" = "eval-gz-99" ]; then
    record "$n" "$name" PASS "$SECS"; else record "$n" "$name" FAIL "$SECS"; fi
fi

# 9: large-output tail (T19). data.log is ~32,000 chars, far over the
# context-scaled tool-output cap, and the needle is on the LAST line: a
# head-only truncation cannot pass this, only the T19 tail-keep can.
# The prompt describes the last line's shape without quoting a stand-in
# value, for the same reason task 2 does (T29 finding 2).
n=9; name=large-tail
if wanted "$n"; then
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
fi

# 10: file denial (T58, dogfood D25). Not an instruction at all: a
# conversational request that only IMPLIES a file, in a work dir holding
# exactly the one document, which is the D25 setup verbatim. Scored and
# reported outside the /9, so the nine published tasks keep their
# denominator.
# PASS needs BOTH halves, and either alone is a FAIL that says which:
#   1. a read tool call whose path ends in the fixture's name, proving the
#      model went and got the file rather than declaring it could not;
#   2. a FACT out of the PDF in the model's own prose, none of which
#      appears in any prompt, so it can only have been read.
# Half 1 without half 2 is a read that produced no answer; half 2 without
# half 1 is a fact the model invented.
#
# Half 2 was the candidate's given name alone until the T58 P1 rider. That
# was too strict and mis-scored a correct run: a model that believes the
# resume is the USER's writes "your skills" and "your name is included"
# throughout and never has any reason to say whose resume it is. It read
# the PDF, quoted its numbers back accurately, and scored FAIL on a name
# it had no occasion to use. The name stays in the alternation, because it
# is still evidence; it just stopped being the only evidence.
n=10; name=resume-feedback
if wanted "$n"; then
mkdir -p "$WORKROOT/task$n"
cp "$RESUME_FIXTURE" "$WORKROOT/task$n/sample-resume.pdf"
run_task "$n" "$name" \
    'can you read my resume and give me feedback?'
t="$EVAL_TRANSCRIPT_DIR/task$n.run$RUN.txt"
T10_SECS=$SECS
t10_read=0
# The ToolEnd line the plain UI prints, whose title is the path the model
# passed: "  <mark> read: /work/sample-resume.pdf". The mark is matched as
# a non-space token rather than by its own character, so the pattern stays
# ASCII and locale-independent. An errored read counts as a call here; it
# is half 2 that decides whether anything was actually learned.
if grep -Eq '^  [^ ]+ read: .*sample-resume\.pdf$' "$t" 2>/dev/null; then
    t10_read=1
fi
t10_cite=0
# Searched in the model's own prose only. Tool OUTPUT never reaches
# --plain, so the sole model-controlled text the harness itself prints is
# that ToolEnd title: without dropping those lines, a hallucinated read of
# "/work/Jordan-resume.pdf" would score as if the PDF had been read.
# Fixed strings and case-sensitive on purpose: these are quotations out of
# the document, not a fuzzy topic match.
if grep -v -E '^  [^ ]+ [a-z]+: ' "$t" 2>/dev/null \
    | grep -qF -e 'Jordan' -e 'tinyq' -e 'Example Logistics' -e 'p99'; then
    t10_cite=1
fi
T10_RES=FAIL
if [ "$TIMED_OUT" = "1" ]; then
    T10_NOTE="TIMEOUT@${EVAL_TASK_TIMEOUT}s"
elif [ "$t10_read" = "1" ] && [ "$t10_cite" = "1" ]; then
    T10_RES=PASS
    T10_NOTE="read the pdf and cited it"
elif [ "$t10_read" = "1" ]; then
    T10_NOTE="read the pdf but never cited it"
elif [ "$t10_cite" = "1" ]; then
    T10_NOTE="cited the pdf but never read it"
else
    T10_NOTE="never read the pdf and never cited it"
fi
T10_NOTE="$T10_NOTE (${T10_SECS}s)"
archive_task "$n" "$T10_RES"
fi

# 11: a Word document (T60). Every task above ends in a text file, so
# nothing before T60 could see whether a request for a .docx yields a
# document or a text file wearing the extension. Scored by score_document:
# memo.docx must exist and temur's read of it must yield the time from
# the prompt. Reported outside the /9 like task 10.
n=11; name=memo-docx
if wanted "$n"; then
mkdir -p "$WORKROOT/task$n"
run_task "$n" "$name" \
    'Write a short memo to the team announcing that the Friday standup moves to 9:30, and save it as memo.docx.'
score_document "$WORKROOT/task$n/memo.docx" '9:30'
T60_11_LINE="$name $T60_RES $T60_NOTE"
archive_task "$n" "$T60_RES"
echo "T60 (run $RUN): $T60_11_LINE"
fi

# 12: a PDF (T60), the same shape. The three points ride in the prompt
# and one of them carries a token no summary can drop without losing the
# point, so the read-back through pdf-extract has one string to find.
n=12; name=summary-pdf
if wanted "$n"; then
mkdir -p "$WORKROOT/task$n"
run_task "$n" "$name" \
    'Summarise the three points below into a one-page document and save it as summary.pdf. Point 1: the Tinyq-Rollout finished on Tuesday, with the new build on every branch office machine. Point 2: support tickets fell by a third in the week after it. Point 3: the old build is switched off at the end of the month.'
score_document "$WORKROOT/task$n/summary.pdf" 'Tinyq-Rollout'
T60_12_LINE="$name $T60_RES $T60_NOTE"
archive_task "$n" "$T60_RES"
echo "T60 (run $RUN): $T60_12_LINE"
fi

# 13: a named section of a multi-page PDF (T62 P1). The dogfood shape this
# comes from: the model read a 10-K PDF, then ran grep, which skips any file
# with a NUL in its first 4 KB and answered "No matches found" without saying
# it had skipped anything, then reached for python3 and pdftotext, neither
# present, and only then paged with read. The question is whether grep seeing
# documents saves that detour.
#
# Reported OUTSIDE the nine-task denominator, exactly as 10, 11 and 12 are.
#
# PASS needs BOTH halves:
#   1. the pre-registered sentinel in the FINAL assistant message, which can
#      only have come from the document: it is absent from the prompt, and
#      tool OUTPUT never reaches --plain;
#   2. zero bash tool calls, because reaching for a shell is the detour the
#      change is meant to remove.
# "Final assistant message" is approximated as the prose after the LAST
# tool-title line the plain UI printed; --plain gives one stream rather than
# delimited turns. Recorded as an approximation in the report.
n=13; name=pdf-section
if wanted "$n"; then
mkdir -p "$WORKROOT/task$n"
t13_fixture "$WORKROOT/task$n"
run_task "$n" "$name" \
    'ferry-review.pdf is in this directory. According to its Maintenance Backlog section, what is deferred to the 2027 dry-dock window? Quote the sentence.'
t="$EVAL_TRANSCRIPT_DIR/task$n.run$RUN.txt"
T13_SECS=$SECS
# Everything after the last tool-title line, then the tool-title lines
# dropped anyway, so the harness's own echo of a path can never be the match.
t13_final=$(awk '/^  [^ ]+ [a-z]+: /{last=NR} {l[NR]=$0} END{for (i=last+1; i<=NR; i++) print l[i]}' "$t" 2>/dev/null \
    | grep -v -E '^  [^ ]+ [a-z]+: ' || true)
t13_quote=0
printf '%s\n' "$t13_final" | grep -qF -- "$T13_SENTINEL" && t13_quote=1
t13_bash=$(grep -cE '^  [^ ]+ bash: ' "$t" 2>/dev/null || true)
[ -n "$t13_bash" ] || t13_bash=0
T62_13_RES=FAIL
if [ "$TIMED_OUT" = "1" ]; then
    T62_13_NOTE="TIMEOUT@${EVAL_TASK_TIMEOUT}s"
elif [ "$t13_quote" = "1" ] && [ "$t13_bash" = "0" ]; then
    T62_13_RES=PASS
    T62_13_NOTE="quoted the section, no bash"
elif [ "$t13_quote" = "1" ]; then
    T62_13_NOTE="quoted the section but ran bash $t13_bash times"
elif [ "$t13_bash" = "0" ]; then
    T62_13_NOTE="never quoted the section, no bash"
else
    T62_13_NOTE="never quoted the section and ran bash $t13_bash times"
fi
T62_13_NOTE="$T62_13_NOTE (${T13_SECS}s)"
T62_13_LINE="$name $T62_13_RES $T62_13_NOTE"
archive_task "$n" "$T62_13_RES"
echo "T62 (run $RUN): $T62_13_LINE"
fi

}

# report_round: prints the run's table and appends its score to $SCORES.
report_round() {
    echo "==== results (run $RUN of $EVAL_RUNS) ===="
    printf '%-4s %-14s %-6s %-8s %s\n' "task" "name" "result" "seconds" "note"
    printf '%-4s %-14s %-6s %-8s %s\n' "----" "--------------" "------" "-------" "----"
    SCORE=0
    while IFS='|' read -r n name res secs note; do
        printf '%-4s %-14s %-6s %-8s %s\n' "$n" "$name" "$res" "$secs" "$note"
        [ "$res" = "PASS" ] && SCORE=$((SCORE + 1))
    done < "$RESULTS"
    # T59: a one-task run scores out of the tasks it ran and says so on the
    # line itself, in the archived file as well as here, so nothing that
    # reads either can take it for a nine-task row.
    if [ -n "$EVAL_ONLY" ]; then
        DENOM=$(wc -l < "$RESULTS" | tr -d ' ')
        SCORE_MARK=" (EVAL_ONLY=$EVAL_ONLY, not a published row)"
    else
        DENOM=9
        SCORE_MARK=""
    fi
    # The archived copy carries a header naming the template, so a results
    # file found on its own still says what it was measured under. The
    # working $RESULTS file stays pure pipe-separated rows.
    { template_line; \
      [ -z "$EVAL_ONLY" ] || echo "# EVAL_ONLY=$EVAL_ONLY: not a published row"; \
      cat "$RESULTS"; \
      if wanted 10; then echo "# D22 (run $RUN): resume-feedback $T10_RES $T10_NOTE"; fi; \
      [ -z "$T59_LINE" ] || echo "# T59 (run $RUN): $T59_LINE"; \
      [ -z "$T60_11_LINE" ] || echo "# T60 (run $RUN): $T60_11_LINE"; \
      [ -z "$T60_12_LINE" ] || echo "# T60 (run $RUN): $T60_12_LINE"; \
      [ -z "$T62_13_LINE" ] || echo "# T62 (run $RUN): $T62_13_LINE"; \
    } > "$EVAL_TRANSCRIPT_DIR/results.run$RUN.txt"
    printf '%s|%s|%s\n' "$RUN" "$SCORE" "$DENOM" >> "$SCORES"
    echo "SCORE (run $RUN): $SCORE/$DENOM$SCORE_MARK"
    # After the score, never inside it. With EVAL_ONLY unset the SCORE
    # line above is byte-identical to every run published before task 10
    # existed.
    if wanted 10; then
        printf '%s|%s|%s\n' "$RUN" "$T10_RES" "$T10_NOTE" >> "$T10LOG"
        echo "D22 (run $RUN): resume-feedback $T10_RES $T10_NOTE"
    fi
    if [ -n "$T59_LINE" ]; then
        printf '%s|%s\n' "$RUN" "$T59_LINE" >> "$T59LOG"
        echo "T59 (run $RUN): $T59_LINE"
    fi
    for line in "$T60_11_LINE" "$T60_12_LINE"; do
        [ -n "$line" ] || continue
        printf '%s|%s\n' "$RUN" "$line" >> "$T60LOG"
        echo "T60 (run $RUN): $line"
    done
    if [ -n "$T62_13_LINE" ]; then
        printf '%s|%s\n' "$RUN" "$T62_13_LINE" >> "$T62LOG"
        echo "T62 (run $RUN): $T62_13_LINE"
    fi
}

RUN=1
while [ "$RUN" -le "$EVAL_RUNS" ]; do
    WORKROOT="$EVAL_ROOT/run$RUN"
    mkdir -p "$WORKROOT"
    RESULTS="$WORKROOT/results.txt"
    : > "$RESULTS"
    if [ -n "$EVAL_ONLY" ]; then
        echo "==== running task $EVAL_ONLY only (EVAL_ONLY=$EVAL_ONLY, not a published row) (run $RUN of $EVAL_RUNS) ===="
    else
        echo "==== running 9 scored tasks + D22 (run $RUN of $EVAL_RUNS) ===="
    fi
    run_round
    report_round
    RUN=$((RUN + 1))
done

echo "==== summary ===="
echo "  model     : $MODEL_GGUF"
echo "  server    : $LLAMA_IMAGE, ctx $CTX, --jinja"
echo "  template  : $(template_desc)"
echo "  profile   : $PROMPT_PROFILE, max_tokens $EVAL_MAX_TOKENS"
echo "  transcripts: $EVAL_TRANSCRIPT_DIR/task<n>.run<r>.txt"
echo "  results    : $EVAL_TRANSCRIPT_DIR/results.run<r>.txt"
template_banner
BELOW=0
while IFS='|' read -r r score denom; do
    echo "SCORE (run $r): $score/$denom$SCORE_MARK"
    # Repeated here for the same reason the score is: a summary read on its
    # own should say what task 10 did. EVAL_MIN still judges the /9 alone.
    t10row=$(grep "^$r|" "$T10LOG" 2>/dev/null || true)
    if [ -n "$t10row" ]; then
        echo "D22 (run $r): resume-feedback $(printf '%s' "$t10row" | cut -d'|' -f2-3 | tr '|' ' ')"
    fi
    t59row=$(grep "^$r|" "$T59LOG" 2>/dev/null || true)
    if [ -n "$t59row" ]; then
        echo "T59 (run $r): $(printf '%s' "$t59row" | cut -d'|' -f2-)"
    fi
    grep "^$r|" "$T60LOG" 2>/dev/null | cut -d'|' -f2- | while IFS= read -r t60row; do
        echo "T60 (run $r): $t60row"
    done
    if [ "$EVAL_MIN" -gt 0 ] && [ "$score" -lt "$EVAL_MIN" ]; then
        BELOW=1
    fi
done < "$SCORES"

if [ "$BELOW" -eq 1 ]; then
    echo "BELOW THRESHOLD (EVAL_MIN=$EVAL_MIN)"
    exit 1
fi
