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

TDIR=/home/dev/rustcode-target
GNU_BIN=$TDIR/i686-unknown-linux-gnu/debug/temur
MUSL_BIN=$TDIR/i686-unknown-linux-musl/release/temur
IMG=docker.io/i386/debian:stable
BARE_IMG=docker.io/library/busybox:stable
PROJ="$(pwd)"
FIXTURES="$PROJ/tests/fixtures/tool_use_parallel.sse,$PROJ/tests/fixtures/text_simple.sse"

# --- shared checks, parameterized by binary/deps dir -------------------------

container_suites() { # $1 = deps dir, $2 = label
    for suite in sse_parser provider request_golden tools agent live_conformance skills tui; do
        TBIN=$(ls -t "$1/${suite}"-* 2>/dev/null | grep -v '\.d$' | head -1 || true)
        [ -n "$TBIN" ] || continue
        echo "-- $suite ($(basename "$TBIN")) --"
        OUT=$(podman run --rm -v "$1":/suites:ro -v "$PROJ":"$PROJ":ro "$IMG" \
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

# TUI pty smokes: the real binary through the real crossterm path (raw
# mode + alternate screen), which TestBackend can't prove. stty sizes the
# pty (script(1)/podman -t leave it 0x0 when stdin is a pipe); the first
# sleep lets the app enter raw mode before input arrives; the second lets
# the mock turn finish (Enter is deliberately ignored while a turn runs).
# ratatui draws diffs with per-word cursor jumps, so greps use single
# tokens only.
ESC=$(printf '\033')
# Single-quote the fixture list so the inner sh -c survives spaces in $PROJ.
MOCKARGS="--tui --mock '$FIXTURES'"
tui_input() { sleep 1; printf 'do the smoke task\r'; sleep 2; printf 'exit\r'; sleep 1; }
check_tui_log() {
    grep -aq "${ESC}\[?1049h" "$1" || { echo "FAIL($2): no alt-screen enter"; exit 1; }
    grep -aq "${ESC}\[?1049l" "$1" || { echo "FAIL($2): no alt-screen leave"; exit 1; }
    for tok in "working" "bash" "Hello," "world!" "▣"; do
        grep -aq "$tok" "$1" || { echo "FAIL($2): missing '$tok'"; exit 1; }
    done
}
container_tui() { # $1 = bin dir, $2 = label, $3 = log file
    tui_input | podman run --rm -i -t \
        -v "$1":/app:ro -v "$PROJ":"$PROJ":ro "$IMG" \
        sh -c "stty rows 24 cols 100; /app/temur $MOCKARGS" \
        > "$3"
    check_tui_log "$3" "$2"
    echo "TUI pty smoke OK ($2)"
}

# --- path 1: gnu-debug (fast inner loop) -------------------------------------

echo "==== PATH 1: gnu-debug (fast inner loop) ===="

echo "== build (i686-gnu debug) =="
cargo build --quiet

echo "== tests (i686-gnu, run on host) =="
cargo test --quiet

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
"$GNU_BIN" --version

echo "== host: tls-probe =="
"$GNU_BIN" tls-probe

echo "== container: --version =="
podman run --rm -v "$(dirname "$GNU_BIN")":/app:ro "$IMG" /app/temur --version

echo "== container: tls-probe =="
podman run --rm -v "$(dirname "$GNU_BIN")":/app:ro "$IMG" /app/temur tls-probe

echo "== container: test suites (gnu-debug) =="
container_suites "$(dirname "$GNU_BIN")/deps" gnu

echo "== container: mock REPL end-to-end (gnu-debug) =="
mock_repl "$(dirname "$GNU_BIN")" "$IMG" gnu

echo "== host: TUI pty smoke =="
tui_input | script -qec "stty rows 24 cols 100; $GNU_BIN $MOCKARGS" /tmp/tui-check-host.log >/dev/null
check_tui_log /tmp/tui-check-host.log host
echo "TUI pty smoke OK (host)"

echo "== container: TUI pty smoke (gnu-debug) =="
container_tui "$(dirname "$GNU_BIN")" gnu /tmp/tui-check-cont.log

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

echo "== container: TUI pty smoke (musl) =="
container_tui "$(dirname "$MUSL_BIN")" musl /tmp/tui-check-musl.log

echo "== bare container (busybox): --version =="
podman run --rm -v "$(dirname "$MUSL_BIN")":/app:ro "$BARE_IMG" /app/temur --version

echo "== bare container (busybox): mock REPL =="
mock_repl "$(dirname "$MUSL_BIN")" "$BARE_IMG" bare

echo "== ALL CHECKS PASSED =="
