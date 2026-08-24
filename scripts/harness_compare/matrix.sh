#!/bin/sh
# T37 matrix runner: all three harnesses x N runs against ONE served model.
#
# The model is NOT switched here on purpose. llama.cpp serves one model at a
# time and the T31 D4 lesson is that a stale server silently measures the
# wrong thing, so switching models is an explicit operator step
# (scripts/serve.sh stop; CTX=16384 scripts/serve.sh start <model>) and this
# script VERIFIES which gguf is actually mounted before running anything.
#
# Usage: scripts/harness_compare/matrix.sh <model-label> [runs]
set -eu
cd "$(dirname "$0")/../.."

MODEL_LABEL=${1:?usage: matrix.sh <model-label> [runs]}
RUNS=${2:-2}
CONTAINER="${CONTAINER_NAME:-temur-llama}"
ARCHIVE_DIR="${ARCHIVE_DIR:-$HOME/temur-eval-archive/t37-harness-compare}"
export ARCHIVE_DIR MODEL_LABEL

# Verify the served model matches the label before spending hours on it.
SERVING=$(podman inspect "$CONTAINER" \
    --format '{{range .Mounts}}{{if eq .Destination "/model.gguf"}}{{.Source}}{{end}}{{end}}' 2>/dev/null || true)
[ -n "$SERVING" ] || { echo "FAIL: no $CONTAINER serving; start it first" >&2; exit 1; }
case "$SERVING" in
    *"$MODEL_LABEL"*) ;;
    *) echo "FAIL: server is mounting $SERVING, which does not match label '$MODEL_LABEL'" >&2
       echo "  Refusing to attribute these runs to the wrong model." >&2
       exit 1 ;;
esac
CTX_SEEN=$(podman logs "$CONTAINER" 2>&1 | grep -o 'n_ctx_slot = [0-9]*' | tail -1)

mkdir -p "$ARCHIVE_DIR/$MODEL_LABEL"
LEDGER="$ARCHIVE_DIR/$MODEL_LABEL/ledger.txt"
{
    echo "model_gguf: $SERVING"
    echo "model_sha256: $(sha256sum "$SERVING" | cut -d' ' -f1)"
    echo "server_ctx: $CTX_SEEN"
    echo "llama_image: $(podman inspect "$CONTAINER" --format '{{.ImageName}} {{.Image}}' 2>/dev/null)"
    echo "temur: $("${TEMUR_BIN:-$HOME/harnesses/temur/temur}" --version 2>&1)"
    echo "opencode: $("${OPENCODE_BIN:-$HOME/harnesses/opencode-glibc/opencode}" --version 2>&1)"
    echo "codex: $("${CODEX_BIN:-$HOME/harnesses/codex/codex}" --version 2>&1)"
    echo "runs_requested: $RUNS"
} > "$LEDGER"
cat "$LEDGER"

r=1
while [ "$r" -le "$RUNS" ]; do
    for h in temur codex opencode; do
        echo "---- $MODEL_LABEL / $h / run $r ----"
        scripts/harness_compare/run.sh "$h" "$r" || echo "  (run exited nonzero; results file holds what landed)"
    done
    r=$((r + 1))
done

echo "==== $MODEL_LABEL matrix complete ===="
find "$ARCHIVE_DIR/$MODEL_LABEL" -name results.txt -exec grep -h SCORE {} \; | sort
