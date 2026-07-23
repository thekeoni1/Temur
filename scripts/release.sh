#!/bin/sh
# Release builder/gater for the multi-arch musl-static artifacts (T7).
#
# Builds all four release targets, applies per-target staticness and
# architecture gates, asserts every runnable binary reports the Cargo.toml
# version, stages bare binaries + SHA256SUMS at /home/dev/dist/release/v<ver>/,
# and refuses to proceed past any red step. check.sh (the i686 acceptance
# gate) runs first and stays untouched by this script; SKIP_CHECK=1 skips it
# for iteration only — a real release run never sets it.
#
# Leak gate: requires an operator-provided patterns file at
#   ${LEAK_PATTERNS:-$HOME/.config/temur-release/leak-patterns.txt}
# (one extended-regex pattern per line, comments/# allowed). The file is
# machine configuration, never committed. Missing file = hard fail. On top of
# it, a generic key-shape scan (repo-safe, embedded below) always runs.
set -eu
cd "$(dirname "$0")/.."

TDIR=/home/dev/rustcode-target
STAGE_ROOT=/home/dev/dist/release

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$VERSION" ] || { echo "FAIL: cannot extract version from Cargo.toml"; exit 1; }
TAG="v$VERSION"
STAGE="$STAGE_ROOT/$TAG"

TARGETS="i686-unknown-linux-musl x86_64-unknown-linux-musl aarch64-unknown-linux-musl armv7-unknown-linux-musleabihf"

# --- gate 1: the standing acceptance gate ------------------------------------

if [ "${SKIP_CHECK:-0}" = "1" ]; then
    echo "== check.sh SKIPPED (SKIP_CHECK=1 — iteration only, not a release) =="
else
    echo "== gate: scripts/check.sh =="
    scripts/check.sh
fi

# --- gate 2: leak grep -------------------------------------------------------

echo "== gate: leak grep =="
PATTERNS="${LEAK_PATTERNS:-$HOME/.config/temur-release/leak-patterns.txt}"
if [ ! -f "$PATTERNS" ]; then
    echo "FAIL: leak patterns file missing: $PATTERNS"
    echo "  Provide the operator patterns file (one extended regex per line)."
    echo "  It is machine configuration — never commit it to the repo."
    exit 1
fi

LEAK_FAIL=0

# Strip comments/blanks: git grep -f would treat a comment line as a literal
# pattern and an empty line as match-everything.
CLEAN_PATTERNS=$(mktemp)
trap 'rm -f "$CLEAN_PATTERNS"' EXIT
grep -v -E '^[[:space:]]*(#|$)' "$PATTERNS" > "$CLEAN_PATTERNS"
[ -s "$CLEAN_PATTERNS" ] || { echo "FAIL: patterns file has no active patterns"; exit 1; }

# Operator patterns over all tracked files (case-insensitive, extended RE).
if git grep -i -E -f "$CLEAN_PATTERNS" -- . >/dev/null 2>&1; then
    echo "FAIL: operator leak pattern matched tracked files:"
    git grep -i -E -f "$CLEAN_PATTERNS" -- . | head -20
    LEAK_FAIL=1
fi

# Operator patterns over all history (commit messages).
while IFS= read -r pat; do
    case "$pat" in ''|'#'*) continue ;; esac
    HITS=$(git log --all -i --extended-regexp --grep="$pat" --format='%h %s' | head -5)
    if [ -n "$HITS" ]; then
        echo "FAIL: operator leak pattern matched commit messages: $pat"
        echo "$HITS"
        LEAK_FAIL=1
    fi
done < "$CLEAN_PATTERNS"

# Embedded generic key-shape scan (repo-safe: each shape requires the key
# body, so doc mentions of a bare prefix — or this very line — don't match).
GENERIC='sk-ant-[a-zA-Z0-9_-]{8}|AKIA[0-9A-Z]{16}|ghp_[A-Za-z0-9]{36}|BEGIN [A-Z ]*PRIVATE KEY'
if git grep -E "$GENERIC" -- . >/dev/null 2>&1; then
    echo "FAIL: generic key-shape scan matched tracked files:"
    git grep -E "$GENERIC" -- . | head -20
    LEAK_FAIL=1
fi
HITS=$(git log --all --extended-regexp --grep="$GENERIC" --format='%h %s' | head -5)
if [ -n "$HITS" ]; then
    echo "FAIL: generic key-shape scan matched commit messages:"
    echo "$HITS"
    LEAK_FAIL=1
fi

[ "$LEAK_FAIL" = "0" ] || exit 1
echo "OK: leak grep clean (operator patterns + generic shapes, files + history)"

# --- per-target build + gates ------------------------------------------------

mkdir -p "$STAGE"

# expected readelf Class / Machine per target
expect_class() {
    case "$1" in
        i686-*|armv7-*) echo ELF32 ;;
        *)              echo ELF64 ;;
    esac
}
expect_machine() {
    case "$1" in
        i686-*)    echo "Intel 80386" ;;
        x86_64-*)  echo "Advanced Micro Devices X86-64" ;;
        aarch64-*) echo "AArch64" ;;
        armv7-*)   echo "ARM" ;;
    esac
}
# how to execute the artifact on this host, if at all
runner() {
    case "$1" in
        i686-*|x86_64-*) echo "" ;;
        aarch64-*) command -v qemu-aarch64-static || echo "SKIP" ;;
        armv7-*)   command -v qemu-arm-static    || echo "SKIP" ;;
    esac
}

SUMMARY=""
for T in $TARGETS; do
    echo "== target: $T =="
    cargo build --quiet --release --target "$T"
    BIN="$TDIR/$T/release/temur"
    [ -f "$BIN" ] || { echo "FAIL($T): binary not found at $BIN"; exit 1; }

    # staticness
    if readelf -l "$BIN" | grep -q 'INTERP'; then
        echo "FAIL($T): INTERP present — not static"; exit 1
    fi
    if readelf -d "$BIN" 2>/dev/null | grep -q 'NEEDED'; then
        echo "FAIL($T): NEEDED entries present"; exit 1
    fi
    file "$BIN" | grep -Eq 'statically linked|static-pie linked' \
        || { echo "FAIL($T): file(1) does not report static linking"; exit 1; }

    # architecture
    WANT_CLASS=$(expect_class "$T"); WANT_MACHINE=$(expect_machine "$T")
    readelf -h "$BIN" | grep -q "Class:.*$WANT_CLASS" \
        || { echo "FAIL($T): wrong ELF class (want $WANT_CLASS)"; exit 1; }
    readelf -h "$BIN" | grep -q "Machine:.*$WANT_MACHINE" \
        || { echo "FAIL($T): wrong machine (want $WANT_MACHINE)"; exit 1; }

    # armv7 hard-float ABI tag (blocking)
    if [ "$T" = "armv7-unknown-linux-musleabihf" ]; then
        readelf -A "$BIN" | grep -q 'Tag_ABI_VFP_args: VFP registers' \
            || { echo "FAIL($T): missing VFP-registers ABI tag"; exit 1; }
    fi

    # version assertion on every runnable binary (native or qemu)
    RUN=$(runner "$T")
    if [ "$RUN" = "SKIP" ]; then
        VNOTE="version: not asserted (no emulator)"
    else
        GOT=$($RUN "$BIN" --version)
        [ "$GOT" = "temur $VERSION" ] \
            || { echo "FAIL($T): --version says '$GOT', want 'temur $VERSION'"; exit 1; }
        VNOTE="version: temur $VERSION ($([ -n "$RUN" ] && echo qemu || echo native))"
    fi

    ART="temur-$TAG-$T"
    cp "$BIN" "$STAGE/$ART"
    chmod 755 "$STAGE/$ART"
    SIZE=$(wc -c < "$STAGE/$ART")
    SUMMARY="$SUMMARY$ART  $(readelf -h "$BIN" | sed -n 's/.*Class: *//p')/$WANT_MACHINE  ${SIZE}B  $VNOTE
"
    echo "OK($T): gated + staged as $ART"
done

# --- checksums ---------------------------------------------------------------

( cd "$STAGE" && sha256sum temur-$TAG-* > SHA256SUMS && sha256sum -c SHA256SUMS )

# --- summary -----------------------------------------------------------------

echo ""
echo "== staged at $STAGE =="
printf '%s' "$SUMMARY"
echo ""
N=$(printf '%s\n' $TARGETS | wc -w)
echo "== RELEASE $TAG: $N/$N ARTIFACTS GATED == (ARM verified at build level per ROADMAP T7; hardware smoke pending hardware)"
