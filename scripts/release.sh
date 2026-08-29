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
# STAGE_ROOT is overridable for iteration only (stage to a scratch dir without
# touching the real release area); a real release run never sets it.
STAGE_ROOT="${STAGE_ROOT:-/home/dev/dist/release}"

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
HITS=$(git grep -i -E -f "$CLEAN_PATTERNS" -- . 2>/dev/null | head -20)
if [ -n "$HITS" ]; then
    echo "FAIL: operator leak pattern matched tracked files:"
    echo "$HITS"
    LEAK_FAIL=1
fi

# Operator patterns over all history (commit messages).
while IFS= read -r pat; do
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
HITS=$(git grep -E "$GENERIC" -- . 2>/dev/null | head -20)
if [ -n "$HITS" ]; then
    echo "FAIL: generic key-shape scan matched tracked files:"
    echo "$HITS"
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

# --- gate 3: version/target skew ---------------------------------------------
# install.sh and the README pin the version and triples outside Cargo.toml;
# assert they match so "tag, filename, and binary can never skew" covers the
# installer and docs too, not just the binaries.

echo "== gate: install.sh / README skew =="
INST_VER=$(sed -n 's/^VERSION=//p' scripts/install.sh)
[ "$INST_VER" = "$VERSION" ] \
    || { echo "FAIL: scripts/install.sh VERSION=$INST_VER but Cargo.toml says $VERSION"; exit 1; }
grep -q "Temur/$TAG/scripts/install.sh" README.md \
    || { echo "FAIL: README one-liner does not pin tag $TAG"; exit 1; }
grep -q "releases/download/$TAG/" README.md \
    || { echo "FAIL: README manual install does not pin tag $TAG"; exit 1; }
for T in $TARGETS; do
    grep -q "$T" scripts/install.sh \
        || { echo "FAIL: scripts/install.sh has no mapping for target $T"; exit 1; }
done
echo "OK: install.sh + README match version $VERSION and all targets"

# --- build + per-target gates ------------------------------------------------

mkdir -p "$STAGE"

echo "== build: all release targets =="
# One invocation, one job graph: cargo overlaps the targets' long serial
# tails (final-crate codegen + link) instead of running four builds end to end.
# shellcheck disable=SC2046
cargo build --quiet --release $(for T in $TARGETS; do printf -- '--target %s ' "$T"; done)

SUMMARY=""
for T in $TARGETS; do
    echo "== target: $T =="
    BIN="$TDIR/$T/release/temur"
    [ -f "$BIN" ] || { echo "FAIL($T): binary not found at $BIN"; exit 1; }

    # per-target gate facts — one arm per target; unknown targets fail closed
    # rather than weakening a grep to "Machine:.*"
    case "$T" in
        i686-*)    WANT_CLASS=ELF32; WANT_MACHINE="Intel 80386";                   RUN="" ;;
        x86_64-*)  WANT_CLASS=ELF64; WANT_MACHINE="Advanced Micro Devices X86-64"; RUN="" ;;
        aarch64-*) WANT_CLASS=ELF64; WANT_MACHINE="AArch64"
                   RUN=$(command -v qemu-aarch64-static || echo SKIP) ;;
        armv7-*)   WANT_CLASS=ELF32; WANT_MACHINE="ARM"
                   RUN=$(command -v qemu-arm-static || echo SKIP) ;;
        *) echo "FAIL($T): no gate facts for this target — add a case arm"; exit 1 ;;
    esac

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
    HDR=$(readelf -h "$BIN")
    echo "$HDR" | grep -q "Class:.*$WANT_CLASS" \
        || { echo "FAIL($T): wrong ELF class (want $WANT_CLASS)"; exit 1; }
    echo "$HDR" | grep -q "Machine:.*$WANT_MACHINE" \
        || { echo "FAIL($T): wrong machine (want $WANT_MACHINE)"; exit 1; }

    # armv7 hard-float ABI tag (blocking)
    if [ "$T" = "armv7-unknown-linux-musleabihf" ]; then
        readelf -A "$BIN" | grep -q 'Tag_ABI_VFP_args: VFP registers' \
            || { echo "FAIL($T): missing VFP-registers ABI tag"; exit 1; }
    fi

    # version assertion on every runnable binary (native or qemu)
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
    SUMMARY="$SUMMARY$ART  $WANT_CLASS/$WANT_MACHINE  ${SIZE}B  $VNOTE
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
set -- $TARGETS; N=$#
echo "== RELEASE $TAG: $N/$N ARTIFACTS GATED == (ARM verified at build level per ROADMAP T7; hardware smoke pending hardware)"

# --- the publish command -----------------------------------------------------
#
# This script stages and gates; the operator publishes. Printing the exact
# invocation here is what keeps the RELEASE TITLE equal to the TAG MESSAGE:
# v0.21 through v0.28 shipped with bare titles ("v0.24.0") because --title was
# omitted and gh defaulted to the tag name. Old releases are NOT retitled.
echo ""
echo "== next step: publish =="
#
# %(contents:subject) alone is NOT enough to prove there is a tag message:
# for a LIGHTWEIGHT tag, git dereferences to the commit and reports the
# COMMIT subject, which is non-empty and looks exactly like a success here
# (verified: a lightweight tag on a "v9.9.9: close-out" commit reports that
# line, objecttype "commit"). So the one mistake this block exists to catch,
# `git tag` instead of `git tag -a`, would have sailed through with the
# close-out commit's subject as the release title. Gate on the object type.
TAG_TYPE=$(git tag -l --format='%(objecttype)' "$TAG" 2>/dev/null || true)
TAG_SUBJECT=$(git tag -l --format='%(contents:subject)' "$TAG" 2>/dev/null || true)
if [ "$TAG_TYPE" = "tag" ] && [ -n "$TAG_SUBJECT" ]; then
    echo "  (title below is the annotated tag message of $TAG, read back just now)"
    TITLE="$TAG_SUBJECT"
elif [ -n "$TAG_TYPE" ]; then
    echo "  NOTE: tag $TAG exists but is LIGHTWEIGHT (objecttype: $TAG_TYPE), so it"
    echo "  has no message of its own and the title below is a placeholder, NOT a"
    echo "  tag message. Delete it, create the ANNOTATED tag (git tag -a), then"
    echo "  re-run this script."
    TITLE="temur $TAG - <name> (T<n>)"
else
    echo "  NOTE: tag $TAG does not exist yet. Create the annotated tag FIRST,"
    echo "  then re-run this script so the title is read from it rather than typed."
    TITLE="temur $TAG - <name> (T<n>)"
fi
echo "  run from inside the repo worktree (gh resolves the repo from git):"
echo ""
echo "    gh release create $TAG \\"
echo "      --title \"$TITLE\" \\"
echo "      --notes-file <notes> \\"
echo "      $STAGE/temur-$TAG-* $STAGE/SHA256SUMS"
echo ""
echo "  <notes> = the CHANGELOG section for $VERSION, written to a file first."
