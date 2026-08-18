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
# Usage:  scripts/serve.sh start [model]|stop|status
# The optional [model] name selects a .gguf from MODELS_DIR by basename,
# case-insensitively: an exact match ("name" or "name.gguf") wins, else a
# unique substring match; zero or several matches fail and list the
# candidates. A running server keeps its current model: switching models
# is `stop` then `start <name>`.
# Knobs:  MODEL_GGUF   explicit path to the .gguf model file; mutually
#                      exclusive with the [model] argument
#         MODELS_DIR   directory searched for models (default $HOME/models);
#                      with exactly one .gguf there and no argument, that
#                      file is auto-selected
#         LLAMA_IMAGE  server image (pinned default below; never auto-pulled)
#         CTX          server context size in tokens (default 8192)
#         PORT         published host port (default 8080)
#         BIND         published host address (default 127.0.0.1, loopback only)
#         CONTAINER_NAME  container name (default temur-llama)
#         MEMINFO      meminfo file read by the RAM fit warning (default
#                      /proc/meminfo; a knob so the check is testable)
#         CHAT_TEMPLATE_FILE  path to a .jinja chat template to serve the
#                      model with INSTEAD of its bundled one. Unset (the
#                      default) is byte-identical to before. See the loud
#                      warning printed when it is set: a template the model
#                      was not trained on can produce confident, wrong
#                      output. Measured 2026-08-17 (T34).
set -eu
cd "$(dirname "$0")/.."

MODEL_GGUF="${MODEL_GGUF:-}"
# Directory searched when MODEL_GGUF is not set: the start argument picks
# among its *.gguf files by name, and a lone .gguf there is auto-selected.
MODELS_DIR="${MODELS_DIR:-$HOME/models}"
# Pinned llama.cpp server build (tag scheme: server-b<build>) — same pin as
# offline_demo.sh; update deliberately, never track latest.
LLAMA_IMAGE="${LLAMA_IMAGE:-ghcr.io/ggml-org/llama.cpp:server-b10438}"
CTX="${CTX:-8192}"
PORT="${PORT:-8080}"
BIND="${BIND:-127.0.0.1}"
# CONTAINER_NAME, not NAME: WSL exports NAME=<hostname> into login shells,
# so a NAME knob would silently rename the container on every WSL box.
CONTAINER_NAME="${CONTAINER_NAME:-temur-llama}"
# T34: substitute chat template, off unless set. The mount destination is
# fixed so a running server can be inspected for it (see start_cmd).
CHAT_TEMPLATE_FILE="${CHAT_TEMPLATE_FILE:-}"
TMPL_DEST=/tmpl.jinja

# The loud banner every path prints while a substitute template is active.
# Measured, not hypothetical: under a substitute Qwen2.5 template on
# 2026-08-17, gemma-3-4b produced zero tool calls and spent 150-430s per
# task inventing plausible tool results, including the contents of a file
# that does not exist.
template_banner() {
    [ -n "$CHAT_TEMPLATE_FILE" ] || return 0
    echo "WARNING: substitute chat template in use ($CHAT_TEMPLATE_FILE)."
    echo "  A template the model was not trained on can produce confident, WRONG"
    echo "  output: under a substitute template gemma-3-4b produced zero tool calls"
    echo "  and spent minutes per task hallucinating plausible tool results."
    echo "  Scores are NOT comparable to native-template runs."
}

usage() {
    echo "Usage: scripts/serve.sh start [model]|stop|status" >&2
    echo "  start [model]  select a .gguf from \$MODELS_DIR by name (exact or" >&2
    echo "                 unique substring, case-insensitive), or set" >&2
    echo "                 MODEL_GGUF=/path/to/model.gguf explicitly" >&2
}

# Human-readable file size for candidate listings; byte math in awk (POSIX
# sh integer width is not guaranteed).
human_size() { # $1 = file
    wc -c < "$1" | awk '{ b = $1
        if (b >= 1073741824) printf "%.1fG", b / 1073741824
        else printf "%.0fM", b / 1048576 }'
}

# Resolve a model name argument against the basenames of $MODELS_DIR/*.gguf,
# case-insensitively. An exact basename match ("name" or "name.gguf") wins;
# else a unique substring match selects; zero or several matches fail and
# list every candidate (glob order is already name-sorted), marking the
# matches when ambiguous. Sets MODEL_GGUF on success.
select_model() { # $1 = requested name
    req_raw=$1
    req=$(printf '%s' "$req_raw" | tr '[:upper:]' '[:lower:]')
    set -- "$MODELS_DIR"/*.gguf
    if [ ! -e "$1" ]; then
        echo "FAIL: no .gguf files in $MODELS_DIR"
        exit 1
    fi
    exact=""
    first_match=""
    match_count=0
    for f in "$@"; do
        lower=$(basename "$f" | tr '[:upper:]' '[:lower:]')
        if [ "$lower" = "$req" ] || [ "$lower" = "$req.gguf" ]; then
            exact=$f
        fi
        case "$lower" in
            *"$req"*)
                [ -n "$first_match" ] || first_match=$f
                match_count=$((match_count + 1)) ;;
        esac
    done
    if [ -n "$exact" ]; then
        MODEL_GGUF=$exact
        echo "OK: selected $MODEL_GGUF"
        return 0
    fi
    if [ "$match_count" -eq 1 ]; then
        MODEL_GGUF=$first_match
        echo "OK: selected $MODEL_GGUF"
        return 0
    fi
    if [ "$match_count" -eq 0 ]; then
        echo "FAIL: no .gguf in $MODELS_DIR matches '$req_raw'; candidates:"
    else
        echo "FAIL: '$req_raw' is ambiguous in $MODELS_DIR ($match_count matches, marked *); candidates:"
    fi
    for f in "$@"; do
        lower=$(basename "$f" | tr '[:upper:]' '[:lower:]')
        mark=" "
        case "$lower" in *"$req"*) mark="*" ;; esac
        echo "  $mark $(basename "$f")  ($(human_size "$f"))"
    done
    exit 1
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

# The host path behind one container mount, selected by DESTINATION.
# T34: this used to be a bare `{{range .Mounts}}{{.Source}}{{end}}`, which
# concatenated every mount source; correct while the model was the only
# mount, wrong the moment a template joins it. Empty = no such mount.
mount_source() { # $1 = destination inside the container
    podman inspect --format \
        "{{range .Mounts}}{{if eq .Destination \"$1\"}}{{.Source}}{{end}}{{end}}" \
        "$CONTAINER_NAME" 2>/dev/null || true
}

summary() {
    echo "  container: $CONTAINER_NAME ($(podman ps --filter "name=$CONTAINER_NAME" --format '{{.Status}}'))"
    echo "  image    : $(podman inspect --format '{{.ImageName}}' "$CONTAINER_NAME")"
    echo "  model    : $(mount_source /model.gguf)"
    running_tmpl=$(mount_source "$TMPL_DEST")
    if [ -n "$running_tmpl" ]; then
        echo "  template : SUBSTITUTE $running_tmpl (not the model's bundled template)"
    else
        echo "  template : bundled (model default)"
    fi
    echo "  published: $(podman ps --filter "name=$CONTAINER_NAME" --format '{{.Ports}}')"
}

start_cmd() { # $1 (optional) = model name resolved via select_model
    [ "$#" -le 1 ] || { usage; exit 2; }
    model_arg="${1:-}"
    # Did the CALLER name a model? Recorded before any defaulting, because
    # by the time the already-running check runs, MODEL_GGUF is always set.
    # T31 (D4, operator dogfood 2026-08-14): `start <name>` against a
    # running server printed OK and kept serving the OLD model, which
    # silently poisoned a measurement.
    model_requested=""
    if [ -n "$MODEL_GGUF" ] || [ -n "$model_arg" ]; then
        model_requested=yes
    fi
    echo "==== serve: preflight ===="
    command -v podman >/dev/null 2>&1 || { echo "FAIL: podman not found"; exit 1; }
    # NEVER auto-pull (offline_demo.sh precedent): missing image => print
    # the exact command and stop.
    podman image exists "$LLAMA_IMAGE" || { echo "FAIL: image not present locally: $LLAMA_IMAGE"; echo "  fetch it first (on a connected machine):  podman pull $LLAMA_IMAGE"; exit 1; }
    # Model resolution order: an explicit MODEL_GGUF path plus a name
    # argument is a contradiction, fail; MODEL_GGUF alone wins untouched;
    # a name argument alone goes through select_model; neither falls back
    # to the T9 lone-gguf auto-default. POSIX glob via set -- (an unmatched
    # pattern stays literal under set -u; the -e test rejects it as zero).
    if [ -n "$MODEL_GGUF" ] && [ -n "$model_arg" ]; then
        echo "FAIL: both MODEL_GGUF and a model name argument are set; choose one, not both"
        exit 1
    fi
    if [ -z "$MODEL_GGUF" ] && [ -n "$model_arg" ]; then
        select_model "$model_arg"
    fi
    if [ -z "$MODEL_GGUF" ]; then
        set -- "$MODELS_DIR"/*.gguf
        if [ "$#" -eq 1 ] && [ -e "$1" ]; then
            MODEL_GGUF=$1
            echo "OK: defaulted MODEL_GGUF=$MODEL_GGUF"
        elif [ ! -e "$1" ]; then
            echo "FAIL: no .gguf files in $MODELS_DIR; set MODEL_GGUF=/path/to/model.gguf or pass a model name"
            exit 1
        else
            echo "FAIL: $# .gguf files in $MODELS_DIR, need exactly 1 to default; set MODEL_GGUF=/path/to/model.gguf or pass a model name:"
            for f in "$@"; do
                echo "    $(basename "$f")  ($(human_size "$f"))"
            done
            exit 1
        fi
    fi
    [ -n "$MODEL_GGUF" ] || { echo "FAIL: set MODEL_GGUF=/path/to/model.gguf"; exit 1; }
    [ -f "$MODEL_GGUF" ] || { echo "FAIL: model file not found: $MODEL_GGUF (set the MODEL_GGUF knob)"; exit 1; }
    # RAM fit check, WARN only: weights are mmap'd, so the model file plus
    # KV cache and compute buffers should fit in MemAvailable or the server
    # thrashes. CTX * 131072 bytes is a deliberately generous per-token
    # allowance for f16 KV plus compute buffers at these defaults (CPU
    # only, one slot). Unreadable meminfo or no MemAvailable line: skip
    # silently. Byte math in awk (POSIX sh integer width is not guaranteed).
    meminfo="${MEMINFO:-/proc/meminfo}"
    if [ -r "$meminfo" ] && grep -q '^MemAvailable:' "$meminfo"; then
        model_bytes=$(wc -c < "$MODEL_GGUF")
        avail_kb=$(awk '/^MemAvailable:/ { print $2; exit }' "$meminfo")
        awk -v m="$model_bytes" -v c="$CTX" -v a="$avail_kb" 'BEGIN {
            over = c * 131072
            if (m + over > a * 1024)
                printf "WARN: model %.1f GiB + overhead %.1f GiB at ctx %d exceeds available %.1f GiB RAM; expect thrashing or OOM\n",
                    m / 1073741824, over / 1073741824, c, a * 1024 / 1073741824
        }'
    fi
    if [ -n "$CHAT_TEMPLATE_FILE" ]; then
        [ -f "$CHAT_TEMPLATE_FILE" ] || { echo "FAIL: CHAT_TEMPLATE_FILE not found: $CHAT_TEMPLATE_FILE"; exit 1; }
        [ -r "$CHAT_TEMPLATE_FILE" ] || { echo "FAIL: CHAT_TEMPLATE_FILE not readable: $CHAT_TEMPLATE_FILE"; exit 1; }
    fi
    echo "OK: image and model present (nothing will be pulled)"
    template_banner

    if is_running; then
        # A running server keeps its model. Saying OK to a request for a
        # DIFFERENT one hands back a server that answers as the old model,
        # and nothing downstream can tell. Fail loudly instead; with no
        # model requested, "already running" stands exactly as before.
        running_model=$(mount_source /model.gguf)
        if [ -n "$model_requested" ] && [ -n "$running_model" ] && [ "$running_model" != "$MODEL_GGUF" ]; then
            echo "FAIL: $CONTAINER_NAME is already serving a different model"
            echo "  running  : $running_model"
            echo "  requested: $MODEL_GGUF"
            echo "  a running server keeps its model; switch with:  $0 stop  then re-run this command"
            exit 1
        fi
        # T34, the same defect as D4 one level down: a running server keeps
        # its TEMPLATE too, and a server serving the bundled template while
        # the caller asked for a substitute (or the reverse) poisons a
        # measurement exactly the way the wrong model does, silently. The
        # mount destination is fixed, so this costs one more inspect.
        running_tmpl=$(mount_source "$TMPL_DEST")
        if [ "$running_tmpl" != "$CHAT_TEMPLATE_FILE" ]; then
            echo "FAIL: $CONTAINER_NAME is already serving a different chat template"
            echo "  running  : ${running_tmpl:-bundled (model default)}"
            echo "  requested: ${CHAT_TEMPLATE_FILE:-bundled (model default)}"
            echo "  a running server keeps its template; switch with:  $0 stop  then re-run this command"
            exit 1
        fi
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
    TMPL_MOUNT=""
    TMPL_ARG=""
    if [ -n "$CHAT_TEMPLATE_FILE" ]; then
        TMPL_MOUNT="-v $CHAT_TEMPLATE_FILE:$TMPL_DEST:ro"
        TMPL_ARG="--chat-template-file $TMPL_DEST"
    fi
    # shellcheck disable=SC2086  # $TMPL_* are deliberately word-split
    podman run -d --name "$CONTAINER_NAME" -p "$BIND:$PORT:8080" \
        -v "$MODEL_GGUF":/model.gguf:ro $TMPL_MOUNT "$LLAMA_IMAGE" \
        -m /model.gguf -c "$CTX" --jinja $TMPL_ARG --host 0.0.0.0 --port 8080 >/dev/null
    echo "server starting (ctx $CTX, --jinja${CHAT_TEMPLATE_FILE:+, template $CHAT_TEMPLATE_FILE}); waiting on /health"

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
    template_banner
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
    start)  shift; start_cmd "$@" ;;
    stop)   stop_cmd ;;
    status) status_cmd ;;
    *)      usage; exit 2 ;;
esac
