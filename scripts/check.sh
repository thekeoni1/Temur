#!/bin/sh
# Per-milestone verification, two paths:
#   gnu-debug    — fast inner loop: host build + tests, container suites and
#                  smokes against the debug binary.
#   musl-release — acceptance gate for the shipped artifact: staticness
#                  (readelf: no INTERP, no NEEDED), suites + smokes in the
#                  container against the musl binary, and a bare busybox
#                  container where a dynamic binary could not even load.
set -eu
cd "$(dirname "$0")/.."

TDIR="${TEMUR_TARGET_DIR:-/home/dev/rustcode-target}"
CHECK_TMP="${TEMUR_CHECK_TMP:-/tmp}"
GNU_BIN=$TDIR/i686-unknown-linux-gnu/debug/temur
MUSL_BIN=$TDIR/i686-unknown-linux-musl/release/temur
IMG=docker.io/i386/debian:stable
BARE_IMG=docker.io/library/busybox:stable
PROJ="$(pwd)"
FIXTURES="$PROJ/tests/fixtures/tool_use_parallel.sse,$PROJ/tests/fixtures/text_simple.sse"

# Host-side product invocations must never read the operator's real config
# or state (the host pty smoke used to fail whenever ~/.config/temur selected
# a non-default provider): every one of them runs with isolated XDG dirs
# inside this run's temp dir. Container invocations already mount their own
# config (or none) and are left alone.
HOST_XDG=$(mktemp -d)
trap 'rm -rf "$HOST_XDG"' EXIT
mkdir -p "$HOST_XDG/config" "$HOST_XDG/state"
HOST_ISOLATION="XDG_CONFIG_HOME=$HOST_XDG/config XDG_STATE_HOME=$HOST_XDG/state"

# --- shared checks, parameterized by binary/deps dir -------------------------

container_suites() { # $1 = deps dir, $2 = label
    # The cli suite (T14) spawns the temur binary via its baked-in
    # CARGO_BIN_EXE path, so the bin dir is mounted read-only at that same
    # path inside the container.
    BINDIR=$(dirname "$1")
    for suite in sse_parser provider openai_compat request_golden tools agent live_conformance live_conformance_openai weak_model session_store skills tui sigint cli approval; do
        TBIN=$(ls -t "$1/${suite}"-* 2>/dev/null | grep -v '\.d$' | head -1 || true)
        [ -n "$TBIN" ] || continue
        echo "-- $suite ($(basename "$TBIN")) --"
        OUT=$(podman run --rm -v "$1":/suites:ro -v "$BINDIR":"$BINDIR":ro -v "$PROJ":"$PROJ":ro "$IMG" \
            "/suites/$(basename "$TBIN")" --test-threads=2) || { echo "FAIL($2): $suite in container"; echo "$OUT"; exit 1; }
        echo "$OUT" | grep 'test result'
    done
}

mock_repl() { # $1 = bin dir, $2 = image, $3 = label
    MOCK_OUT=$(printf 'do the smoke task\n' | podman run --rm -i \
        -v "$1":/app:ro -v "$PROJ":"$PROJ":ro "$2" \
        /app/temur --mock "$FIXTURES")
    echo "$MOCK_OUT" | grep -q "read the file and list the directory" || { echo "FAIL($3): no streamed text"; echo "$MOCK_OUT"; exit 1; }
    echo "$MOCK_OUT" | grep -q "bash" || { echo "FAIL($3): no tool activity"; echo "$MOCK_OUT"; exit 1; }
    echo "$MOCK_OUT" | grep -q "Hello, world!" || { echo "FAIL($3): no second-round response"; echo "$MOCK_OUT"; exit 1; }
    echo "mock REPL OK ($3)"
}

# Same smoke through the config-selected OpenAI-compat provider: a mounted
# config picks provider "openai-compat" (keyless, so no secret plumbing) and
# the fixtures are OpenAI chunk streams. Proves selection + the second wire
# end-to-end in the real binary.
OPENAI_FIXTURES="$PROJ/tests/fixtures/openai/tool_parallel.sse,$PROJ/tests/fixtures/openai/text_simple.sse"
mock_repl_openai() { # $1 = bin dir, $2 = image, $3 = label
    CFG_DIR=$(mktemp -d)
    mkdir -p "$CFG_DIR/temur"
    printf '{"provider":"openai-compat","openai_compat":{"model":"mock-local"}}\n' \
        > "$CFG_DIR/temur/config.json"
    MOCK_OUT=$(printf 'do the smoke task\n' | podman run --rm -i \
        -v "$1":/app:ro -v "$PROJ":"$PROJ":ro -v "$CFG_DIR":/cfg:ro \
        -e XDG_CONFIG_HOME=/cfg "$2" \
        /app/temur --mock "$OPENAI_FIXTURES")
    rm -rf "$CFG_DIR"
    # No model banner in mock mode; selection is proven by the fixtures
    # themselves — OpenAI chunk streams only assemble through the compat
    # provider (the Anthropic parser rejects them, yielding no output).
    echo "$MOCK_OUT" | grep -q "read the file and list the directory" || { echo "FAIL($3/openai): no streamed text"; echo "$MOCK_OUT"; exit 1; }
    echo "$MOCK_OUT" | grep -q "bash" || { echo "FAIL($3/openai): no tool activity"; echo "$MOCK_OUT"; exit 1; }
    echo "$MOCK_OUT" | grep -q "Hello, world!" || { echo "FAIL($3/openai): no second-round response"; echo "$MOCK_OUT"; exit 1; }
    echo "mock REPL OK ($3, openai-compat)"
}

# TUI pty smokes: the real binary through the real crossterm path (raw
# mode + alternate screen), which TestBackend can't prove. stty sizes the
# pty (script(1)/podman -t leave it 0x0 when stdin is a pipe). ratatui
# draws diffs with per-word cursor jumps, so greps use single tokens only.
ESC=$(printf '\033')
# Single-quote the fixture list so the inner sh -c survives spaces in $PROJ.
MOCKARGS="--tui --mock '$FIXTURES'"
# Wall-clock bound on any one pty smoke. Nothing here should take close to
# this; it exists so a stall fails in minutes with a diagnosis instead of
# sitting until someone notices.
TUI_TIMEOUT=180
tui_input() { sleep 1; printf 'do the smoke task\r'; sleep 2; printf 'exit\r'; sleep 1; }
check_tui_log() {
    grep -aq "${ESC}\[?1049h" "$1" || { echo "FAIL($2): no alt-screen enter"; exit 1; }
    grep -aq "${ESC}\[?1049l" "$1" || { echo "FAIL($2): no alt-screen leave"; exit 1; }
    for tok in "working" "bash" "Hello," "world!" "▣"; do
        grep -aq "$tok" "$1" || { echo "FAIL($2): missing '$tok'"; exit 1; }
    done
}
tui_diagnose() { # $1 = log file
    echo "  log $1 holds $(wc -c < "$1" 2>/dev/null || echo 0) bytes; last 400 below"
    tail -c 400 "$1" 2>/dev/null | cat -v
    echo ""
}
# Block until the app proves it reached a state, rather than guessing how
# long it takes to get there. CT_DEADLINE bounds the whole smoke, not each
# gate separately, so a stall cannot add up past TUI_TIMEOUT.
tui_wait() { # $1 = log, $2 = marker, $3 = what we're waiting for, $4 = label
    while [ "$(date +%s)" -lt "$CT_DEADLINE" ]; do
        grep -aq "$2" "$1" 2>/dev/null && return 0
        sleep 0.1
    done
    echo "FAIL($4): never saw $3 within the ${TUI_TIMEOUT}s bound"
    exec 3>&- 2>/dev/null || true
    podman rm -f "$CT_NAME" >/dev/null 2>&1 || true
    tui_diagnose "$1"
    exit 1
}
# Input is gated on what the app has actually done, not on blind sleeps.
# Container startup was measured between 1.8s and 3.0s while the mock turn
# itself takes 0.2s, so the old fixed schedule (keys at 1s, "exit" at 3s)
# raced: a slow start pushed the "exit" Enter into the running turn, where
# the TUI ignores Enter by design, and the run then sat at its idle redraw
# tick with nothing to stop it. The fifo stays open for the life of the
# run so stdin never closes under the app.
container_tui() { # $1 = bin dir, $2 = label, $3 = log file
    CT_DIR=$(mktemp -d)
    CT_NAME="temur-tui-$2-$$"
    CT_DEADLINE=$(( $(date +%s) + TUI_TIMEOUT ))
    mkfifo "$CT_DIR/in"
    : > "$3"
    timeout -k 5 "$TUI_TIMEOUT" podman run --rm -i -t --name "$CT_NAME" \
        -v "$1":/app:ro -v "$PROJ":"$PROJ":ro "$IMG" \
        sh -c "stty rows 24 cols 100; /app/temur $MOCKARGS" \
        < "$CT_DIR/in" > "$3" 2>&1 &
    CT_PID=$!
    # Read-write so the open cannot itself block if podman never starts.
    exec 3<> "$CT_DIR/in"
    tui_wait "$3" "${ESC}\[?1049h" "the alternate screen" "$2"
    printf 'do the smoke task\r' >&3
    tui_wait "$3" "world!" "the turn output" "$2"
    printf 'exit\r' >&3
    CT_RC=0
    wait "$CT_PID" || CT_RC=$?
    exec 3>&-
    podman rm -f "$CT_NAME" >/dev/null 2>&1 || true
    rm -rf "$CT_DIR"
    [ "$CT_RC" -eq 0 ] || {
        echo "FAIL($2): container run exited $CT_RC (timeout is ${TUI_TIMEOUT}s)"
        tui_diagnose "$3"; exit 1; }
    check_tui_log "$3" "$2"
    echo "TUI pty smoke OK ($2)"
}

# --- path 1: gnu-debug (fast inner loop) -------------------------------------

echo "==== PATH 1: gnu-debug (fast inner loop) ===="

echo "== build (i686-gnu debug) =="
cargo build --quiet

echo "== tests (i686-gnu, run on host) =="
cargo test --quiet

echo "== T37 harness-compare prompt drift =="
tests/harness_compare_drift.sh

echo "== forbidden deps =="
if cargo tree -i openssl-sys >/dev/null 2>&1; then
    echo "FAIL: openssl-sys in dependency graph"; exit 1
fi
if cargo tree -i aws-lc-sys >/dev/null 2>&1; then
    echo "FAIL: aws-lc-sys in dependency graph (ring provider expected)"; exit 1
fi
echo "OK: no openssl-sys, no aws-lc-sys"

echo "== binary is 32-bit =="
file "$GNU_BIN" | grep -q "ELF 32-bit" || { echo "FAIL: not a 32-bit ELF"; exit 1; }

echo "== host: --version =="
env $HOST_ISOLATION "$GNU_BIN" --version

echo "== host: tls-probe =="
env $HOST_ISOLATION "$GNU_BIN" tls-probe

echo "== container: --version =="
podman run --rm -v "$(dirname "$GNU_BIN")":/app:ro "$IMG" /app/temur --version

echo "== container: tls-probe =="
podman run --rm -v "$(dirname "$GNU_BIN")":/app:ro "$IMG" /app/temur tls-probe

echo "== container: test suites (gnu-debug) =="
container_suites "$(dirname "$GNU_BIN")/deps" gnu

echo "== container: mock REPL end-to-end (gnu-debug) =="
mock_repl "$(dirname "$GNU_BIN")" "$IMG" gnu

echo "== container: mock REPL via openai-compat provider (gnu-debug) =="
mock_repl_openai "$(dirname "$GNU_BIN")" "$IMG" gnu

echo "== host: TUI pty smoke =="
tui_input | timeout -k 5 "$TUI_TIMEOUT" script -qec "stty rows 24 cols 100; env $HOST_ISOLATION $GNU_BIN $MOCKARGS" "$CHECK_TMP/tui-check-host.log" >/dev/null || {
    echo "FAIL(host): pty smoke exited nonzero or outlived the ${TUI_TIMEOUT}s bound"
    tui_diagnose "$CHECK_TMP/tui-check-host.log"; exit 1; }
check_tui_log "$CHECK_TMP/tui-check-host.log" host
echo "TUI pty smoke OK (host)"

echo "== container: TUI pty smoke (gnu-debug) =="
container_tui "$(dirname "$GNU_BIN")" gnu "$CHECK_TMP/tui-check-cont.log"

# --- path 2: musl-release (acceptance gate for the shipped artifact) ---------

echo "==== PATH 2: musl-release (acceptance gate) ===="

echo "== build (i686-musl release) =="
cargo build --quiet --release --target i686-unknown-linux-musl

echo "== build test suites (i686-musl release, run in container below) =="
cargo test --quiet --release --target i686-unknown-linux-musl --no-run

echo "== staticness (readelf) =="
if readelf -l "$MUSL_BIN" | grep -q 'INTERP'; then
    echo "FAIL: INTERP program header present — binary is not static"; exit 1
fi
echo "OK: no INTERP program header"
if readelf -d "$MUSL_BIN" 2>/dev/null | grep -q 'NEEDED'; then
    echo "FAIL: NEEDED entries present — binary has dynamic library deps"; exit 1
fi
echo "OK: no NEEDED entries"
file "$MUSL_BIN" | grep -q "ELF 32-bit" || { echo "FAIL: musl binary is not a 32-bit ELF"; exit 1; }

echo "== container: --version (musl) =="
podman run --rm -v "$(dirname "$MUSL_BIN")":/app:ro "$IMG" /app/temur --version

echo "== container: tls-probe (musl) =="
podman run --rm -v "$(dirname "$MUSL_BIN")":/app:ro "$IMG" /app/temur tls-probe

echo "== container: test suites (musl-release) =="
container_suites "$(dirname "$MUSL_BIN")/deps" musl

echo "== container: mock REPL end-to-end (musl) =="
mock_repl "$(dirname "$MUSL_BIN")" "$IMG" musl

echo "== container: mock REPL via openai-compat provider (musl) =="
mock_repl_openai "$(dirname "$MUSL_BIN")" "$IMG" musl

echo "== container: TUI pty smoke (musl) =="
container_tui "$(dirname "$MUSL_BIN")" musl "$CHECK_TMP/tui-check-musl.log"

echo "== bare container (busybox): --version =="
podman run --rm -v "$(dirname "$MUSL_BIN")":/app:ro "$BARE_IMG" /app/temur --version

echo "== bare container (busybox): mock REPL =="
mock_repl "$(dirname "$MUSL_BIN")" "$BARE_IMG" bare

echo "== ALL CHECKS PASSED =="
