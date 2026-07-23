#!/bin/sh
# Plain-REPL SIGINT black-box tests (F4, v0.1.1), against the real binary
# with a mock bash-sleep turn (tests/fixtures/interrupt_sleep.sse — a
# tool_use `sleep 987`, distinctive on purpose for the /proc scan).
#
#   case 1: one SIGINT mid-turn — the turn lands ("turn interrupted"), the
#           bash cell closed as an error, /proc shows NO orphaned sleep
#           (T6-style scan), and EOF then exits 0 cleanly. (The
#           "(interrupted by user)" result content is history-level, not
#           stdout — pinned by the bash unit and agent tests.)
#   case 2: second SIGINT while the flag is still set — exit code 130,
#           still no orphan.
#
# usage: scripts/sigint_test.sh [binary]
#   binary defaults to the gnu-debug build.
set -eu
cd "$(dirname "$0")/.."
BIN="${1:-/home/dev/rustcode-target/i686-unknown-linux-gnu/debug/temur}"
FIX="$(pwd)/tests/fixtures/interrupt_sleep.sse"
[ -x "$BIN" ] || { echo "FAIL: binary not found/executable: $BIN"; exit 1; }

fail() { echo "FAIL: $*" >&2; exit 1; }

# T6-style orphan scan: any process whose cmdline contains our distinctive
# sleep. The [9] defeats self-matching (this grep's own cmdline).
sleeper_running() {
    for d in /proc/[0-9]*/cmdline; do
        tr '\0' ' ' < "$d" 2>/dev/null | grep -q "sleep [9]87" && return 0
    done
    return 1
}

wait_for() { # $1 = attempts (x100ms), $2... = command
    n="$1"; shift
    i=0
    while [ "$i" -lt "$n" ]; do
        "$@" && return 0
        sleep 0.1
        i=$((i + 1))
    done
    return 1
}

grep_out() { grep -q "$1" "$T/out"; }

run_case() { # $1 = case label, $2 = number of SIGINTs
    T=$(mktemp -d)
    mkfifo "$T/in"
    "$BIN" --plain --mock "$FIX" < "$T/in" > "$T/out" 2>&1 &
    PID=$!
    exec 9> "$T/in" # hold the write end open so stdin stays live
    printf 'go\n' >&9

    wait_for 100 sleeper_running || fail "$1: mock bash sleep never started"
    kill -INT "$PID"
    wait_for 100 grep_out "turn interrupted" || fail "$1: turn did not land after SIGINT: $(cat "$T/out")"
    grep_out "✗ bash" || fail "$1: bash cell not closed as an error: $(cat "$T/out")"
    if sleeper_running; then fail "$1: orphaned sleep survives the group kill"; fi

    if [ "$2" = "2" ]; then
        kill -INT "$PID"
        RC=0; wait "$PID" || RC=$?
        [ "$RC" -eq 130 ] || fail "$1: second SIGINT must exit 130, got $RC"
        echo "PASS($1): second SIGINT exited 130, no orphan"
    else
        exec 9>&- # EOF -> clean exit
        RC=0; wait "$PID" || RC=$?
        [ "$RC" -eq 0 ] || fail "$1: clean exit expected, got $RC: $(cat "$T/out")"
        grep_out "bye" || fail "$1: no clean prompt/exit after the interrupt"
        echo "PASS($1): interrupt landed, no orphan, clean exit"
    fi
    rm -rf "$T"
}

run_case single 1
run_case double 2

echo "== SIGINT TESTS PASSED (interrupt lands, group kill leaves no orphan, 130 on second press) =="
