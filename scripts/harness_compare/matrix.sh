#!/bin/sh
# T37 matrix runner: all three harnesses x N runs against one served model,
# with a FRESH llama.cpp server per TASK (see server.sh for why).
#
# Server hygiene exists because a dead server does not produce an obvious
# failure, it produces a plausible-looking score. Measured during T37:
# against a dead server temur fails a task in ~6s while Codex, which retries
# connections without bound, burns the entire 1200s per-task timeout.
# Identical cause, opposite-looking results, and the Codex cell reads as a
# capability finding. So liveness is checked around every task, and a cell
# in which any server died is VOID and quarantined rather than scored.
#
# The gguf actually mounted is verified against the label on every start
# (the T31 D4 lesson: a stale or mismatched server silently measures the
# wrong thing).
#
# Usage: scripts/harness_compare/matrix.sh <model-label> [runs]
# Env:   CTX (default 12288, the T37 Decision C amended value)
#        RUN_START, FORCE, MODELS_DIR, LLAMA_IMAGE, ARCHIVE_DIR
set -eu
cd "$(dirname "$0")/../.."

MODEL_LABEL=${1:?usage: matrix.sh <model-label> [runs]}
RUNS=${2:-2}
CONTAINER="${CONTAINER_NAME:-temur-llama}"
# v2 subtree: the per-CELL-server artifacts in t37-harness-compare/ are the
# record of attempts 1-6 and are not overwritten. Everything scored under
# the per-TASK methodology lands here instead, so the two procedures can
# never be silently mixed in one table.
ARCHIVE_DIR="${ARCHIVE_DIR:-$HOME/temur-eval-archive/t37-harness-compare-v2-pertask}"
MODELS_DIR="${MODELS_DIR:-$HOME/models}"
LLAMA_IMAGE="${LLAMA_IMAGE:-ghcr.io/ggml-org/llama.cpp:server-b10438}"
# ctx 12288, not 16384. 16384 was tried and the kernel OOM-killed
# llama-server three times on this 7.61 GiB machine, the last time inside a
# single temur cell even with --parallel 1. The authoritative evidence is
# dmesg, which reports what the OOM killer actually acted on:
#   Out of memory: Killed process ... (llama-server) anon-rss:6969312kB
#   Out of memory: Killed process ... (llama-server) anon-rss:7065400kB
#   Out of memory: Killed process ... (llama-server) anon-rss:7105452kB
# Quoted in dmesg's own units, which are KiB: that is 6.65, 6.74 and 6.78
# GiB against a 7.61 GiB machine (MemTotal 7979196 kB, and /proc/meminfo's
# "kB" is KiB too).
#
# FOUR memory figures were misreported during T37 before this settled, and
# all four are recorded so the next reader does not repeat them:
#   1. 4.24  was a STARTUP reading quoted as an operating figure.
#   2. 6.29  was cgroup memory.peak, which counts anon PLUS reclaimable
#            page cache, quoted as if comparable to an anon-rss kill.
#   3. the kills were quoted as 6.97/7.07 by dividing KiB by 1e6.
#   4. the machine was quoted as 7.43 GiB by converting the same KiB with
#      x1000 and THEN dividing by 1024^3, a double conversion error.
# The lesson is not "GiB not GB": it is that kB from the kernel is ALWAYS
# KiB, in /proc/meminfo and dmesg alike, so one conversion rule covers
# every one of these readings. See cgroup_mem(), which reports the two
# distinct quantities by name so they cannot be conflated again.
#
# 12288 keeps enough headroom above the competitors' first requests, which
# are the large ones (measured server-side: codex 7413 tokens, opencode
# 7276, temur 2761), so that the table measures capability rather than
# context exhaustion. That was Decision C's actual requirement. The
# rationale was first written as an OpenCode-specific figure; it is not
# OpenCode-specific, and codex carries the largest prompt of the three.
# Verified: a full three-harness run 1 completed at 12288 with every
# server alive at the end of its cell.
CTX="${CTX:-12288}"
export ARCHIVE_DIR MODEL_LABEL CTX

MODEL_GGUF=$(ls "$MODELS_DIR"/*"$MODEL_LABEL"*.gguf 2>/dev/null | head -1 || true)
[ -n "$MODEL_GGUF" ] || { echo "FAIL: no gguf in $MODELS_DIR matching '$MODEL_LABEL'" >&2; exit 1; }

# Heavy-job lock. A matrix run holds this box near its memory ceiling for
# hours, and a cargo build starting alongside it took the llama.cpp server
# out with the kernel OOM killer partway through a cell (T37). Stating the
# rule in prose was not enough, so it is mechanical: check.sh refuses to
# start while this pidfile names a live process. One direction only, since
# matrix starts are deliberate and the gate is the thing that gets run
# absent-mindedly.
HEAVY_LOCK="${TEMUR_HEAVY_LOCK:-$HOME/.temur-heavy-job.pid}"
printf '%s\n' "$$" > "$HEAVY_LOCK"
trap 'rm -f "$HEAVY_LOCK"' EXIT INT TERM

# Server lifecycle lives in server.sh and is driven PER TASK by run.sh.
# matrix.sh no longer starts a per-cell server: six OOM kills established
# that llama-server's memory climbs across prompt-processing cycles WITHIN a
# cell, so a per-cell server is the mechanism that had to go.
# shellcheck disable=SC1091
. scripts/harness_compare/server.sh
export MODEL_GGUF

mkdir -p "$ARCHIVE_DIR/$MODEL_LABEL" "$ARCHIVE_DIR/aborted-blocks"
LEDGER="$ARCHIVE_DIR/$MODEL_LABEL/ledger.txt"
{
    echo "model_gguf: $MODEL_GGUF"
    echo "model_sha256: $(sha256sum "$MODEL_GGUF" | cut -d' ' -f1)"
    echo "requested_ctx: $CTX"
    echo "llama_image: $LLAMA_IMAGE"
    echo "temur: $("${TEMUR_BIN:-$HOME/harnesses/temur/temur}" --version 2>&1)"
    echo "opencode: $("${OPENCODE_BIN:-$HOME/harnesses/opencode-glibc/opencode}" --version 2>&1)"
    echo "codex: $("${CODEX_BIN:-$HOME/harnesses/codex/codex}" --version 2>&1)"
    echo "runs: $RUNS starting at ${RUN_START:-1}"
    echo "server_policy: fresh server per TASK, liveness checked around each"
    echo "--- per-cell ---"
} >> "$LEDGER"
# Appended, never truncated: re-invoking for a later run must not erase the
# provenance of cells that already ran.
tail -12 "$LEDGER"

# RUN_START exists because re-invoking this script used to restart at run 1
# and truncate a completed cell's results file before the replacement had
# produced anything. Hours of finished work are not something a re-run
# should be able to destroy by default, so a cell that already carries a
# SCORE line is skipped unless FORCE=1 says otherwise.
r="${RUN_START:-1}"
LAST=$((r + RUNS - 1))
while [ "$r" -le "$LAST" ]; do
    for h in temur codex opencode; do
        EXISTING="$ARCHIVE_DIR/$MODEL_LABEL/$h/run$r/results.txt"
        if [ "${FORCE:-0}" != "1" ] && grep -q '^SCORE' "$EXISTING" 2>/dev/null; then
            echo "---- $MODEL_LABEL / $h / run $r: SKIPPED, already scored ----"
            echo "  $(grep '^SCORE' "$EXISTING")"
            echo "  (FORCE=1 to re-run and overwrite)"
            continue
        fi
        echo "---- $MODEL_LABEL / $h / run $r ----"
        rc=0
        scripts/harness_compare/run.sh "$h" "$r" || rc=$?
        CELLDIR="$ARCHIVE_DIR/$MODEL_LABEL/$h/run$r"
        if grep -q '^SCORE' "$CELLDIR/results.txt" 2>/dev/null; then
            printf '%s run%s: ctx=%s %s\n' "$h" "$r" "$CTX" \
                "$(grep '^SCORE' "$CELLDIR/results.txt" | cut -f5) status=SCORED" >> "$LEDGER"
            [ -f "$CELLDIR/per-task-mem.txt" ] && sed 's/^/    /' "$CELLDIR/per-task-mem.txt" >> "$LEDGER"
            echo "  cell OK"
        else
            # run.sh writes no SCORE line when a server died anywhere in the
            # cell. With per-task restarts this should not happen; the ruling
            # is to stop and report rather than re-run into the same wall.
            DEST="$ARCHIVE_DIR/aborted-blocks/${MODEL_LABEL}-${h}-run${r}-serverdied-$(date +%H%M%S)"
            mkdir -p "$DEST"
            mv "$CELLDIR" "$DEST/" 2>/dev/null || true
            {
                echo "VOID CELL, NOT MATRIX DATA."
                echo "harness=$h run=$r ctx=$CTX"
                echo "A server died during at least one task despite per-task"
                echo "restarts. That means the accumulation this methodology was"
                echo "adopted to remove has survived it. STOP AND REPORT rather"
                echo "than re-running: a cell that only sometimes completes is a"
                echo "sampling bias waiting to be published."
            } > "$DEST/WHY-ABORTED.txt"
            printf '%s run%s: ctx=%s status=VOID-SERVER-DIED\n' "$h" "$r" "$CTX" >> "$LEDGER"
            echo "  VOID: quarantined to $DEST"
            echo "  STOPPING: per-task restarts did not remove the accumulation." >&2
            exit 1
        fi
        [ "$rc" -eq 0 ] || echo "  (run.sh exited $rc)"
    done
    r=$((r + 1))
done

echo "==== $MODEL_LABEL matrix complete ===="
find "$ARCHIVE_DIR/$MODEL_LABEL" -name results.txt -exec grep -h SCORE {} \; 2>/dev/null | sort
echo "---- ledger ----"
cat "$LEDGER"
