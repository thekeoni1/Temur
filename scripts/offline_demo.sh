#!/bin/sh
# T3 acceptance demo, operator-run (NOT part of check.sh): temur's
# musl-static binary drives a real llama.cpp server with zero internet BY
# CONSTRUCTION — both containers share one podman pod created with
# --network none (loopback-only namespace), so isolation is a property of
# the setup, not a promise.
#
# Proof structure:
#   NEGATIVE  tls-probe from inside the pod MUST fail (no route out);
#   POSITIVE  the model must use the bash tool to run exactly
#             `echo offline-demo-ok > proof.txt`, and the file's CONTENT is
#             asserted from the host — model prose is never evidence.
#
# Nothing is ever pulled or downloaded here: preflight prints the exact
# pull command and exits if an image is missing.
#
# Usage:  MODEL_GGUF=/path/to/model.gguf scripts/offline_demo.sh
# Knobs:  MUSL_BIN     path to the musl-static temur binary
#         LLAMA_IMAGE  server image (pinned default below)
#         CTX          server context size, mirrored into context_window
#         DEMO_TURN_TIMEOUT  seconds allowed for the agent turn (default 300)
#         DEMO_TRANSCRIPT    where the session transcript is kept
set -eu
cd "$(dirname "$0")/.."

# Same build-target convention check.sh uses; override for other layouts.
MUSL_BIN="${MUSL_BIN:-/home/dev/rustcode-target/i686-unknown-linux-musl/release/temur}"
# Pinned llama.cpp server build (tag scheme: server-b<build>). Verify the
# pin still exists before pulling; update deliberately, never track latest.
LLAMA_IMAGE="${LLAMA_IMAGE:-ghcr.io/ggml-org/llama.cpp:server-b10438}"
APP_IMG=docker.io/i386/debian:stable
BARE_IMG=docker.io/library/busybox:stable
CTX="${CTX:-8192}"
POD=temur-offline-demo
TRANSCRIPT="${DEMO_TRANSCRIPT:-/tmp/temur-offline-demo-transcript.txt}"

WORK_DIR=""
CFG_DIR=""
teardown() {
    podman pod rm -f "$POD" >/dev/null 2>&1 || true
    [ -n "$WORK_DIR" ] && rm -rf "$WORK_DIR"
    [ -n "$CFG_DIR" ] && rm -rf "$CFG_DIR"
}
trap teardown EXIT INT TERM

echo "==== offline demo: preflight ===="

[ -x "$MUSL_BIN" ] || { echo "FAIL: musl binary not found at $MUSL_BIN (build with: cargo build --release --target i686-unknown-linux-musl)"; exit 1; }
readelf -l "$MUSL_BIN" | grep -q 'INTERP' && { echo "FAIL: INTERP present — binary is not static"; exit 1; }
readelf -d "$MUSL_BIN" 2>/dev/null | grep -q 'NEEDED' && { echo "FAIL: NEEDED entries — binary is not static"; exit 1; }
echo "OK: musl binary static (no INTERP, no NEEDED)"

[ -n "${MODEL_GGUF:-}" ] || { echo "FAIL: set MODEL_GGUF=/path/to/model.gguf"; exit 1; }
[ -f "$MODEL_GGUF" ] || { echo "FAIL: MODEL_GGUF not found: $MODEL_GGUF"; exit 1; }
echo "OK: model file present ($MODEL_GGUF)"

# NEVER auto-pull: an offline demo that pulls mid-run is lying about
# offline. Missing image => print the exact command and stop.
for img in "$LLAMA_IMAGE" "$APP_IMG" "$BARE_IMG"; do
    podman image exists "$img" || { echo "FAIL: image not present locally: $img"; echo "  fetch it first (on a connected machine):  podman pull $img"; exit 1; }
done
echo "OK: all images present locally (nothing will be pulled)"

echo "==== pod bring-up (--network none) ===="

podman pod rm -f "$POD" >/dev/null 2>&1 || true
podman pod create --name "$POD" --network none >/dev/null
podman run -d --pod "$POD" --name "$POD-llama" \
    -v "$MODEL_GGUF":/model.gguf:ro "$LLAMA_IMAGE" \
    -m /model.gguf -c "$CTX" --jinja --host 127.0.0.1 --port 8080 >/dev/null
echo "server starting (ctx $CTX, --jinja)"

# Health: llama.cpp serves /health; busybox wget probes it from inside the
# pod's loopback-only namespace. ~60s budget for model load.
i=0
until podman run --rm --pod "$POD" "$BARE_IMG" \
    wget -q -O /dev/null http://127.0.0.1:8080/health 2>/dev/null; do
    i=$((i + 1))
    [ "$i" -ge 30 ] && { echo "FAIL: server not healthy after ~60s; last logs:"; podman logs --tail 15 "$POD-llama" || true; exit 1; }
    sleep 2
done
echo "OK: server healthy"

echo "==== NEGATIVE assertion: no internet inside the pod ===="

if podman run --rm --pod "$POD" -v "$(dirname "$MUSL_BIN")":/app:ro "$APP_IMG" \
    /app/temur tls-probe >/dev/null 2>&1; then
    echo "FAIL: tls-probe SUCCEEDED inside the pod — network is not isolated"
    exit 1
fi
echo "OK: tls-probe failed inside the pod (isolation is real)"

echo "==== POSITIVE assertion: temur drives a real tool call ===="

WORK_DIR=$(mktemp -d)
CFG_DIR=$(mktemp -d)
mkdir -p "$CFG_DIR/temur"
# Keyless local config; max_tokens sized for a small window (see
# docs/OFFLINE.md); model id is informational to llama.cpp.
printf '{"provider":"openai-compat","max_tokens":1024,"openai_compat":{"model":"local-gguf","context_window":%s}}\n' "$CTX" \
    > "$CFG_DIR/temur/config.json"

# T46: --allow-mutations below. The demo's whole assertion is that the
# model's bash call really wrote proof.txt, so the one thing it must not do
# is stop at an approval prompt. Piping to tee already means no approver is
# installed; the flag says so out loud.
PROMPT='Use the bash tool to run exactly this command: echo offline-demo-ok > proof.txt'
printf '%s\n' "$PROMPT" | timeout "${DEMO_TURN_TIMEOUT:-300}" \
    podman run --rm -i --pod "$POD" \
    -v "$(dirname "$MUSL_BIN")":/app:ro \
    -v "$CFG_DIR":/cfg:ro -v "$WORK_DIR":/work \
    -e XDG_CONFIG_HOME=/cfg -w /work "$APP_IMG" \
    /app/temur --allow-mutations --plain | tee "$TRANSCRIPT"

# The file's content is the proof; the transcript is context, never the
# assertion.
GOT=$(cat "$WORK_DIR/proof.txt" 2>/dev/null | tr -d '[:space:]' || true)
[ "$GOT" = "offline-demo-ok" ] || { echo "FAIL: proof.txt missing or wrong (got: '$GOT') — transcript at $TRANSCRIPT"; exit 1; }
echo "OK: proof.txt written by the model's tool call and verified from the host"

echo "==== summary ===="
echo "  isolation : pod --network none; in-pod tls-probe failed (required)"
echo "  server    : $LLAMA_IMAGE, ctx $CTX, --jinja"
echo "  model     : $MODEL_GGUF"
echo "  agent     : $MUSL_BIN (musl-static, verified no INTERP/NEEDED)"
echo "  proof     : proof.txt == offline-demo-ok (host-verified)"
echo "  transcript: $TRANSCRIPT"
echo "OFFLINE DEMO PASSED"
