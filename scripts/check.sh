#!/bin/sh
# Per-milestone verification: build + test as i686, forbidden-dep check,
# then exercise the binary on the host and inside the i386/debian container.
set -eu
cd "$(dirname "$0")/.."

BIN=/home/dev/rustcode-target/i686-unknown-linux-gnu/debug/opencode-rust
IMG=docker.io/i386/debian:stable

echo "== build (i686) =="
cargo build --quiet

echo "== tests (i686, run on host) =="
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
file "$BIN" | grep -q "ELF 32-bit" || { echo "FAIL: not a 32-bit ELF"; exit 1; }

echo "== host: --version =="
"$BIN" --version

echo "== host: tls-probe =="
"$BIN" tls-probe

echo "== container: --version =="
podman run --rm -v "$(dirname "$BIN")":/app:ro "$IMG" /app/opencode-rust --version

echo "== container: tls-probe =="
podman run --rm -v "$(dirname "$BIN")":/app:ro "$IMG" /app/opencode-rust tls-probe

echo "== container: test suites =="
DEPS="$(dirname "$BIN")/deps"
PROJ="$(pwd)"
for suite in sse_parser provider tools agent live_conformance skills tui; do
    TBIN=$(ls -t "$DEPS/${suite}"-* 2>/dev/null | grep -v '\.d$' | head -1 || true)
    [ -n "$TBIN" ] || continue
    echo "-- $suite ($(basename "$TBIN")) --"
    OUT=$(podman run --rm -v "$DEPS":/suites:ro -v "$PROJ":"$PROJ":ro "$IMG" \
        "/suites/$(basename "$TBIN")" --test-threads=2) || { echo "FAIL: $suite in container"; echo "$OUT"; exit 1; }
    echo "$OUT" | grep 'test result'
done

echo "== container: mock REPL end-to-end =="
MOCK_OUT=$(printf 'do the smoke task\n' | podman run --rm -i \
    -v "$(dirname "$BIN")":/app:ro -v "$PROJ":"$PROJ":ro "$IMG" \
    /app/opencode-rust --mock "$PROJ/tests/fixtures/tool_use_parallel.sse,$PROJ/tests/fixtures/text_simple.sse")
echo "$MOCK_OUT" | grep -q "read the file and list the directory" || { echo "FAIL: no streamed text"; echo "$MOCK_OUT"; exit 1; }
echo "$MOCK_OUT" | grep -q "bash" || { echo "FAIL: no tool activity"; echo "$MOCK_OUT"; exit 1; }
echo "$MOCK_OUT" | grep -q "Hello, world!" || { echo "FAIL: no second-round response"; echo "$MOCK_OUT"; exit 1; }
echo "mock REPL OK"

# TUI pty smokes: the real binary through the real crossterm path (raw
# mode + alternate screen), which TestBackend can't prove. stty sizes the
# pty (script(1)/podman -t leave it 0x0 when stdin is a pipe); the first
# sleep lets the app enter raw mode before input arrives; the second lets
# the mock turn finish (Enter is deliberately ignored while a turn runs).
# ratatui draws diffs with per-word cursor jumps, so greps use single
# tokens only.
ESC=$(printf '\033')
# Single-quote the fixture list so the inner sh -c survives spaces in $PROJ
# (e.g. a checkout under ".../RustCode - Copy").
MOCKARGS="--tui --mock '$PROJ/tests/fixtures/tool_use_parallel.sse,$PROJ/tests/fixtures/text_simple.sse'"
tui_input() { sleep 1; printf 'do the smoke task\r'; sleep 2; printf 'exit\r'; sleep 1; }
check_tui_log() {
    grep -aq "${ESC}\[?1049h" "$1" || { echo "FAIL($2): no alt-screen enter"; exit 1; }
    grep -aq "${ESC}\[?1049l" "$1" || { echo "FAIL($2): no alt-screen leave"; exit 1; }
    for tok in "working" "bash" "Hello," "world!" "▣"; do
        grep -aq "$tok" "$1" || { echo "FAIL($2): missing '$tok'"; exit 1; }
    done
}

echo "== host: TUI pty smoke =="
tui_input | script -qec "stty rows 24 cols 100; $BIN $MOCKARGS" /tmp/tui-check-host.log >/dev/null
check_tui_log /tmp/tui-check-host.log host
echo "host TUI pty smoke OK"

echo "== container: TUI pty smoke =="
tui_input | podman run --rm -i -t \
    -v "$(dirname "$BIN")":/app:ro -v "$PROJ":"$PROJ":ro "$IMG" \
    sh -c "stty rows 24 cols 100; /app/opencode-rust $MOCKARGS" \
    > /tmp/tui-check-cont.log
check_tui_log /tmp/tui-check-cont.log container
echo "container TUI pty smoke OK"

echo "== ALL CHECKS PASSED =="
