#!/bin/sh
# T37 matrix runner: all three harnesses x N runs against one served model,
# with a FRESH llama.cpp server per scored cell.
#
# Per-cell server hygiene exists because a dead server does not produce an
# obvious failure, it produces a plausible-looking score. Measured during
# T37: against a dead server temur fails a task in ~6s while Codex, which
# retries connections without bound, burns the entire 1200s per-task
# timeout. Identical cause, opposite-looking results, and the Codex cell
# reads as a capability finding. So every cell gets a fresh server, a
# liveness check before it, and a liveness check after it; a cell whose
# server died at any point is VOID and is quarantined rather than scored.
#
# The model is chosen here and the server is started here, but the CHOICE of
# gguf is still verified against the label (the T31 D4 lesson: a stale or
# mismatched server silently measures the wrong thing).
#
# Usage: scripts/harness_compare/matrix.sh <model-label> [runs]
# Env:   CTX (default 12288, the T37 Decision C amended value)
#        MODELS_DIR, LLAMA_IMAGE, ARCHIVE_DIR
set -eu
cd "$(dirname "$0")/../.."

MODEL_LABEL=${1:?usage: matrix.sh <model-label> [runs]}
RUNS=${2:-2}
CONTAINER="${CONTAINER_NAME:-temur-llama}"
ARCHIVE_DIR="${ARCHIVE_DIR:-$HOME/temur-eval-archive/t37-harness-compare}"
MODELS_DIR="${MODELS_DIR:-$HOME/models}"
LLAMA_IMAGE="${LLAMA_IMAGE:-ghcr.io/ggml-org/llama.cpp:server-b10438}"
# ctx 12288, not 16384. 16384 was tried and the kernel OOM-killed
# llama-server three times on this 7.98GB machine, the last time inside a
# single temur cell even with --parallel 1. The authoritative evidence is
# dmesg, which reports what the OOM killer actually acted on:
#   Out of memory: Killed process ... (llama-server) anon-rss:6969312kB
#   Out of memory: Killed process ... (llama-server) anon-rss:7065400kB
#   Out of memory: Killed process ... (llama-server) anon-rss:7105452kB
# Quoted in dmesg's own units, which are KiB: that is 6.65, 6.74 and 6.78
# GiB against a 7.43 GiB machine. Every memory figure in T37 is GiB from
# here on, because earlier prose mixed GiB and decimal GB for the same
# readings and the numbers stopped agreeing with each other.
#
# Two memory figures were misreported during T37 before this settled, and
# both mistakes are recorded here so the next reader does not repeat them:
# a 4.24GB reading was taken at STARTUP rather than under load, and a
# 6.29GB reading was cgroup memory.peak, which counts anon PLUS reclaimable
# page cache and is therefore not comparable to an anon-rss kill figure.
# See cgroup_mem() below, which now reports both quantities by name.
#
# 12288 keeps enough headroom above OpenCode's ~7.2k-token system prompt
# that the table measures capability rather than context exhaustion, which
# was Decision C's actual requirement. Verified: a full three-harness run 1
# completed at 12288 with every server alive at the end of its cell.
CTX="${CTX:-12288}"
export ARCHIVE_DIR MODEL_LABEL CTX

MODEL_GGUF=$(ls "$MODELS_DIR"/*"$MODEL_LABEL"*.gguf 2>/dev/null | head -1 || true)
[ -n "$MODEL_GGUF" ] || { echo "FAIL: no gguf in $MODELS_DIR matching '$MODEL_LABEL'" >&2; exit 1; }

# Memory is reported as TWO figures because one number here is misleading,
# and was: cgroup memory.peak counts anon PLUS page cache, and page cache is
# reclaimable, so it is not the quantity the OOM killer acts on. Measured
# during T37, a cell recorded memory.peak 6.43GB at ctx 12288 and survived,
# while the ctx 16384 kills were anon-rss 6.97GB and 7.05GB per dmesg. Both
# numbers were real and they measure different things, so both are recorded
# with their units named rather than one being passed off as "RSS".
cgroup_mem() { # prints "peak=<GB>|anon=<GB>", or "unmeasured" fields, never a guess
    cid=$(podman inspect "$CONTAINER" --format '{{.Id}}' 2>/dev/null || true)
    if [ -z "$cid" ]; then echo "peak=unmeasured|anon=unmeasured"; return; fi
    d=$(find /sys/fs/cgroup -maxdepth 8 -name memory.peak -path "*libpod-$cid.scope*" 2>/dev/null \
        | grep -v conmon | head -1)
    if [ -z "$d" ]; then echo "peak=unmeasured|anon=unmeasured"; return; fi
    d=$(dirname "$d")
    pk=$(awk '{printf "%.2f", $1/1073741824}' "$d/memory.peak" 2>/dev/null || echo unmeasured)
    an=$(awk '/^anon /{printf "%.2f", $2/1073741824}' "$d/memory.stat" 2>/dev/null || echo unmeasured)
    [ -n "$an" ] || an=unmeasured
    echo "peak=${pk}|anon=${an}"
}

server_alive() { curl -s -o /dev/null --max-time 10 "http://127.0.0.1:8080/health" 2>/dev/null; }

start_server() {
    podman rm -f "$CONTAINER" >/dev/null 2>&1 || true
    podman run -d --name "$CONTAINER" -p 127.0.0.1:8080:8080 \
        -v "$MODEL_GGUF":/model.gguf:ro "$LLAMA_IMAGE" \
        -m /model.gguf -c "$CTX" --parallel 1 --jinja --host 0.0.0.0 --port 8080 >/dev/null
    i=0
    until server_alive; do
        i=$((i + 1))
        [ "$i" -ge 60 ] && { echo "FAIL: server unhealthy after ~120s" >&2
                             podman logs --tail 15 "$CONTAINER" >&2 || true; return 1; }
        sleep 2
    done
    # Verify what is ACTUALLY mounted, never what was intended.
    mounted=$(podman inspect "$CONTAINER" \
        --format '{{range .Mounts}}{{if eq .Destination "/model.gguf"}}{{.Source}}{{end}}{{end}}' 2>/dev/null)
    case "$mounted" in
        *"$MODEL_LABEL"*) ;;
        *) echo "FAIL: mounted $mounted does not match label '$MODEL_LABEL'" >&2; return 1 ;;
    esac
    # The load_model line is written at ~0.08s but is not necessarily
    # readable through `podman logs` the moment /health first answers, so
    # this read RACED and came back empty on the first T37 cells. An empty
    # read must never satisfy the guard below: "could not verify" is not
    # "verified fine", so retry briefly and then FAIL CLOSED.
    SLOTS=""; CTX_SEEN=""
    i=0
    while [ "$i" -lt 15 ]; do
        SLOTS=$(podman logs "$CONTAINER" 2>&1 | grep -o 'n_slots = [0-9]*' | tail -1 | grep -o '[0-9]*' || true)
        CTX_SEEN=$(podman logs "$CONTAINER" 2>&1 | grep -o 'n_ctx_slot = [0-9]*' | tail -1 | grep -o '[0-9]*' || true)
        [ -n "$SLOTS" ] && [ -n "$CTX_SEEN" ] && break
        i=$((i + 1)); sleep 1
    done
    if [ -z "$SLOTS" ] || [ -z "$CTX_SEEN" ]; then
        echo "FAIL: could not read n_slots/n_ctx_slot from the server log" >&2
        echo "  Refusing to run a scored cell against an unverified server." >&2
        return 1
    fi
    # KV is sized for n_slots x n_ctx_slot; extra slots multiply it for
    # nothing, and that multiplication is what caused the first two kills.
    if [ "$SLOTS" -gt 1 ]; then
        echo "FAIL: server has n_slots = $SLOTS; expected 1" >&2; return 1
    fi
    if [ "$CTX_SEEN" != "$CTX" ]; then
        echo "FAIL: server reports n_ctx_slot = $CTX_SEEN but CTX is $CTX" >&2; return 1
    fi
    return 0
}

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
    echo "runs_requested: $RUNS"
    echo "server_policy: fresh server per cell, liveness checked before and after"
    echo "--- per-cell ---"
} > "$LEDGER"
cat "$LEDGER"

r=1
while [ "$r" -le "$RUNS" ]; do
    for h in temur codex opencode; do
        echo "---- $MODEL_LABEL / $h / run $r ----"
        if ! start_server; then
            echo "  VOID: server would not start; cell not run" | tee -a "$LEDGER"
            continue
        fi
        echo "  server: $CTX_SEEN, n_slots=$SLOTS"
        rc=0
        scripts/harness_compare/run.sh "$h" "$r" || rc=$?
        MEM=$(cgroup_mem)
        if server_alive; then
            printf '%s run%s: ctx=%s slots=%s mem_%s (GB; peak=anon+cache, anon=rss-like) status=SCORED\n' \
                "$h" "$r" "$CTX_SEEN" "$SLOTS" "$MEM" >> "$LEDGER"
            echo "  cell OK (mem $MEM GB)"
        else
            # The server died mid-cell. Whatever the results file says, it
            # is not a measurement of this harness.
            DEST="$ARCHIVE_DIR/aborted-blocks/${MODEL_LABEL}-${h}-run${r}-serverdied-$(date +%H%M%S)"
            mkdir -p "$DEST"
            mv "$ARCHIVE_DIR/$MODEL_LABEL/$h/run$r" "$DEST/" 2>/dev/null || true
            {
                echo "VOID CELL, NOT MATRIX DATA."
                echo "The llama.cpp server was not alive at the end of this cell."
                echo "harness=$h run=$r ctx=$CTX_SEEN mem_${MEM} (GB)"
                echo "container: $(podman inspect "$CONTAINER" --format 'status={{.State.Status}} exit={{.State.ExitCode}}' 2>/dev/null)"
                echo
                echo "A dead server does not fail loudly, it fails plausibly:"
                echo "temur fails a task in ~6s while Codex retries without bound"
                echo "and burns the whole per-task timeout. Neither number is a"
                echo "capability result, so this cell is quarantined, not scored."
            } > "$DEST/WHY-ABORTED.txt"
            printf '%s run%s: ctx=%s mem_%s (GB) status=VOID-SERVER-DIED\n' \
                "$h" "$r" "$CTX_SEEN" "$MEM" >> "$LEDGER"
            echo "  VOID: server died during the cell; quarantined to $DEST"
        fi
        [ "$rc" -eq 0 ] || echo "  (run.sh exited $rc)"
    done
    r=$((r + 1))
done

echo "==== $MODEL_LABEL matrix complete ===="
find "$ARCHIVE_DIR/$MODEL_LABEL" -name results.txt -exec grep -h SCORE {} \; 2>/dev/null | sort
echo "---- ledger ----"
cat "$LEDGER"
