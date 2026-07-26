#!/bin/sh
# Background llama.cpp server for one-window local operation: start the
# inference server detached in this terminal, then run temur right here —
# no second WSL window. Operator infrastructure for the third-party
# server; temur itself gains no server behavior.
#
# This deliberately inverts offline_demo.sh's bring-up: a plain
# `podman run -d` with a published port instead of a --network none pod
# (the host cannot reach into those — the demo probes health from inside),
# a container-side bind of 0.0.0.0 (a loopback bind inside the container
# is unreachable through a published port), and NO exit trap — the server
# must survive this script exiting.
#
# The publish is loopback-only by default (127.0.0.1), so nothing is
# exposed to the LAN, and port 8080 matches temur's default openai-compat
# base_url (http://127.0.0.1:8080/v1) — the keyless "local" profile works
# with zero endpoint config. The container-internal port is always 8080;
# PORT changes only the published host side.
#
# Usage:  MODEL_GGUF=/path/to/model.gguf scripts/serve.sh start|stop|status
# Knobs:  MODEL_GGUF   path to the .gguf model file (required for start;
#                      defaulted from MODELS_DIR when exactly one .gguf
#                      lives there — see below)
#         MODELS_DIR   directory searched for that default (default
#                      $HOME/models); with zero or several .gguf files
#                      MODEL_GGUF stays required, nothing is guessed
#         LLAMA_IMAGE  server image (pinned default below; never auto-pulled)
#         CTX          server context size in tokens (default 8192)
#         PORT         published host port (default 8080)
#         BIND         published host address (default 127.0.0.1, loopback only)
#         CONTAINER_NAME  container name (default temur-llama)
set -eu
cd "$(dirname "$0")/.."

MODEL_GGUF="${MODEL_GGUF:-}"
# Pinned llama.cpp server build (tag scheme: server-b<build>) — same pin as
# offline_demo.sh; update deliberately, never track latest.
LLAMA_IMAGE="${LLAMA_IMAGE:-ghcr.io/ggml-org/llama.cpp:server-b10068}"
CTX="${CTX:-8192}"
PORT="${PORT:-8080}"
BIND="${BIND:-127.0.0.1}"
# CONTAINER_NAME, not NAME: WSL exports NAME=<hostname> into login shells,
# so a NAME knob would silently rename the container on every WSL box.
CONTAINER_NAME="${CONTAINER_NAME:-temur-llama}"

usage() {
    echo "Usage: [MODEL_GGUF=/path/to/model.gguf] scripts/serve.sh start|stop|status" >&2
}

# install.sh-style tool fallback, repurposed as an HTTP health probe.
if command -v curl >/dev/null 2>&1; then
    probe() { curl -fsS -o /dev/null "$1" 2>/dev/null; }
elif command -v wget >/dev/null 2>&1; then
    probe() { wget -q -O /dev/null "$1" 2>/dev/null; }
else
    probe() { echo "FAIL: need curl or wget for the health probe"; exit 1; }
fi

is_running() { podman ps --format '{{.Names}}' | grep -qx "$CONTAINER_NAME"; }

summary() {
    echo "  container: $CONTAINER_NAME ($(podman ps --filter "name=$CONTAINER_NAME" --format '{{.Status}}'))"
    echo "  image    : $(podman inspect --format '{{.ImageName}}' "$CONTAINER_NAME")"
    echo "  model    : $(podman inspect --format '{{range .Mounts}}{{.Source}}{{end}}' "$CONTAINER_NAME")"
    echo "  published: $(podman ps --filter "name=$CONTAINER_NAME" --format '{{.Ports}}')"
}

start_cmd() {
    echo "==== serve: preflight ===="
    command -v podman >/dev/null 2>&1 || { echo "FAIL: podman not found"; exit 1; }
    # NEVER auto-pull (offline_demo.sh precedent): missing image => print
    # the exact command and stop.
    podman image exists "$LLAMA_IMAGE" || { echo "FAIL: image not present locally: $LLAMA_IMAGE"; echo "  fetch it first (on a connected machine):  podman pull $LLAMA_IMAGE"; exit 1; }
    # T9 quality-of-life: with MODEL_GGUF unset, default it when MODELS_DIR
    # holds EXACTLY one .gguf — one file is unambiguous, anything else stays
    # an explicit choice. POSIX glob via set -- (an unmatched pattern stays
    # literal under set -u; the -e test below rejects it, counting as zero).
    MODELS_DIR="${MODELS_DIR:-$HOME/models}"
    if [ -z "$MODEL_GGUF" ]; then
        set -- "$MODELS_DIR"/*.gguf
        if [ "$#" -eq 1 ] && [ -e "$1" ]; then
            MODEL_GGUF=$1
            echo "OK: defaulted MODEL_GGUF=$MODEL_GGUF"
        else
            [ -e "$1" ] || set -- # unmatched literal pattern = zero files
            echo "FAIL: set MODEL_GGUF=/path/to/model.gguf (searched $MODELS_DIR: found $# .gguf files, need exactly 1 to default)"
            exit 1
        fi
    fi
    [ -n "$MODEL_GGUF" ] || { echo "FAIL: set MODEL_GGUF=/path/to/model.gguf"; exit 1; }
    [ -f "$MODEL_GGUF" ] || { echo "FAIL: model file not found: $MODEL_GGUF (set the MODEL_GGUF knob)"; exit 1; }
    echo "OK: image and model present (nothing will be pulled)"

    if is_running; then
        echo "OK: already running"
        summary
        exit 0
    fi
    # A stale stopped/exited container would squat the name — clear it.
    if podman container exists "$CONTAINER_NAME"; then
        podman rm -f "$CONTAINER_NAME" >/dev/null
        echo "removed stale non-running container $CONTAINER_NAME"
    fi

    echo "==== serve: start ===="
    podman run -d --name "$CONTAINER_NAME" -p "$BIND:$PORT:8080" \
        -v "$MODEL_GGUF":/model.gguf:ro "$LLAMA_IMAGE" \
        -m /model.gguf -c "$CTX" --jinja --host 0.0.0.0 --port 8080 >/dev/null
    echo "server starting (ctx $CTX, --jinja); waiting on /health"

    # ~60s budget for model load, probed from the HOST through the
    # published port. On timeout, fail closed: never leave a dead
    # container squatting the name.
    i=0
    until probe "http://127.0.0.1:$PORT/health"; do
        i=$((i + 1))
        if [ "$i" -ge 30 ]; then
            echo "FAIL: server not healthy after ~60s; last logs:"
            podman logs --tail 15 "$CONTAINER_NAME" || true
            podman rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
            exit 1
        fi
        sleep 2
    done

    echo "OK: llama.cpp serving $MODEL_GGUF on http://127.0.0.1:$PORT/v1"
    if [ "$PORT" = "8080" ]; then
        echo "  matches temur's default base_url — a keyless openai-compat profile needs no base_url"
    else
        echo "  non-default port: set base_url \"http://127.0.0.1:$PORT/v1\" in your temur profile"
    fi
}

stop_cmd() {
    if podman container exists "$CONTAINER_NAME"; then
        podman rm -f "$CONTAINER_NAME" >/dev/null
        echo "OK: stopped ($CONTAINER_NAME removed)"
    else
        echo "OK: not running"
    fi
}

status_cmd() {
    if ! is_running; then
        echo "not running"
        exit 1
    fi
    if probe "http://127.0.0.1:$PORT/health"; then
        echo "OK: healthy"
        summary
        exit 0
    fi
    echo "FAIL: container $CONTAINER_NAME is running but /health does not answer on http://127.0.0.1:$PORT"
    exit 1
}

case "${1:-}" in
    start)  start_cmd ;;
    stop)   stop_cmd ;;
    status) status_cmd ;;
    *)      usage; exit 2 ;;
esac
