#!/bin/sh
# T37 llama.cpp server lifecycle, shared by matrix.sh and run.sh so that
# both manage the server the same way. Sourced, never executed.
#
# The server is restarted PER TASK, not per cell. Six kernel OOM kills
# during T37 established why: llama-server's memory climbs across
# prompt-processing cycles within a single cell until the kernel kills it.
#
# What the climb IS was never established. An earlier attribution, "server-
# side heap accumulation rather than KV sizing", is retracted here as
# unverified inference: nothing in T37 instrumented llama-server's
# allocator, so that sentence stated a diagnosis the evidence did not
# support. Two things were actually observed. A smaller context moves where
# the climb STARTS without stopping it. Restarting per task holds anon flat
# across a whole cell. Per-task restarts are therefore adopted on evidence
# of effect, not on a mechanism.
#
# Two causes WERE ruled out. Kills 5 and 6 rule out build concurrency: kill
# 6 happened with the heavy-job lock held and check.sh actively refusing to
# start. And it is not OpenCode-specific: temur's surviving cell ended at
# anon 5.94 GiB, just under the ~6.2 GiB line the kills cluster on.
#
# The cost is deliberate and disclosed: restarting forfeits llama.cpp's
# cross-task prefix cache, so every task pays its own full prefill. That
# cost belongs to the harness whose prompt it is, which is the measurement
# this milestone exists to make.

LLAMA_IMAGE="${LLAMA_IMAGE:-ghcr.io/ggml-org/llama.cpp:server-b10438}"
CONTAINER="${CONTAINER_NAME:-temur-llama}"

server_alive() {
    curl -s -o /dev/null --max-time 10 http://127.0.0.1:8080/health 2>/dev/null
}

server_stop() {
    podman rm -f "$CONTAINER" >/dev/null 2>&1 || true
}

# server_start: brings the server up and VERIFIES it, setting
# SERVER_READY_SECS. Returns nonzero rather than leaving a half-checked
# server running, because an unverified server is how a cell gets
# attributed to a context or a model it did not run at.
server_start() {
    _gguf=$1; _ctx=$2; _label=$3
    server_stop
    _t0=$(date +%s)
    podman run -d --name "$CONTAINER" -p 127.0.0.1:8080:8080 \
        -v "$_gguf":/model.gguf:ro "$LLAMA_IMAGE" \
        -m /model.gguf -c "$_ctx" --parallel 1 --jinja \
        --host 0.0.0.0 --port 8080 >/dev/null 2>&1 || return 1
    _i=0
    until server_alive; do
        _i=$((_i + 1))
        if [ "$_i" -ge 90 ]; then
            echo "FAIL: server unhealthy after ~180s" >&2
            podman logs --tail 15 "$CONTAINER" >&2 || true
            return 1
        fi
        sleep 2
    done
    SERVER_READY_SECS=$(( $(date +%s) - _t0 ))

    _mounted=$(podman inspect "$CONTAINER" \
        --format '{{range .Mounts}}{{if eq .Destination "/model.gguf"}}{{.Source}}{{end}}{{end}}' 2>/dev/null)
    case "$_mounted" in
        *"$_label"*) ;;
        *) echo "FAIL: mounted $_mounted does not match label '$_label'" >&2; return 1 ;;
    esac

    # An empty read must never satisfy the checks below: "could not verify"
    # is not "verified fine", so retry briefly and then fail closed.
    SLOTS=""; CTX_SEEN=""
    _i=0
    while [ "$_i" -lt 15 ]; do
        SLOTS=$(podman logs "$CONTAINER" 2>&1 | grep -o 'n_slots = [0-9]*' | tail -1 | grep -o '[0-9]*' || true)
        CTX_SEEN=$(podman logs "$CONTAINER" 2>&1 | grep -o 'n_ctx_slot = [0-9]*' | tail -1 | grep -o '[0-9]*' || true)
        [ -n "$SLOTS" ] && [ -n "$CTX_SEEN" ] && break
        _i=$((_i + 1)); sleep 1
    done
    [ -n "$SLOTS" ] && [ -n "$CTX_SEEN" ] || {
        echo "FAIL: could not read n_slots/n_ctx_slot from the server log" >&2; return 1; }
    [ "$SLOTS" -eq 1 ] || { echo "FAIL: n_slots = $SLOTS, expected 1" >&2; return 1; }
    [ "$CTX_SEEN" = "$_ctx" ] || {
        echo "FAIL: n_ctx_slot = $CTX_SEEN but ctx is $_ctx" >&2; return 1; }
    return 0
}

# cgroup_mem: "peak=<GiB>|anon=<GiB>". Two figures, because one is
# misleading: memory.peak counts anon PLUS reclaimable page cache and is
# therefore not comparable to the anon-rss figure the OOM killer acts on.
# Fields read "unmeasured" rather than reporting a guess.
cgroup_mem() {
    _cid=$(podman inspect "$CONTAINER" --format '{{.Id}}' 2>/dev/null || true)
    [ -n "$_cid" ] || { echo "peak=unmeasured|anon=unmeasured"; return; }
    _d=$(find /sys/fs/cgroup -maxdepth 8 -name memory.peak -path "*libpod-$_cid.scope*" 2>/dev/null \
        | grep -v conmon | head -1)
    [ -n "$_d" ] || { echo "peak=unmeasured|anon=unmeasured"; return; }
    _d=$(dirname "$_d")
    _pk=$(awk '{printf "%.2f", $1/1073741824}' "$_d/memory.peak" 2>/dev/null || echo unmeasured)
    _an=$(awk '/^anon /{printf "%.2f", $2/1073741824}' "$_d/memory.stat" 2>/dev/null || echo unmeasured)
    [ -n "$_an" ] || _an=unmeasured
    echo "peak=${_pk}|anon=${_an}"
}
