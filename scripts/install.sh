#!/bin/sh
# temur installer: detect arch, download the release binary, verify its
# checksum, install. Never installs unverified: missing sha256sum or a
# failed check is a hard stop, not a warning.
# Overrides: TEMUR_INSTALL_DIR (default ~/.local/bin), TEMUR_BASE_URL
# (artifact base, for testing against a local mirror), TEMUR_CPUINFO
# (cpuinfo path, test seam for the SSE2 check).
set -eu

VERSION=0.16.0
TAG="v$VERSION"
BASE="${TEMUR_BASE_URL:-https://github.com/thekeoni1/Temur/releases/download/$TAG}"
DIR="${TEMUR_INSTALL_DIR:-$HOME/.local/bin}"

fail() { echo "temur-install: $*" >&2; exit 1; }

case "$(uname -m)" in
    x86_64|amd64)   TRIPLE=x86_64-unknown-linux-musl ;;
    aarch64|arm64)  TRIPLE=aarch64-unknown-linux-musl ;;
    armv7l|armv8l)  TRIPLE=armv7-unknown-linux-musleabihf ;;
    armv6l|armv5*)  fail "no prebuilt binary for pre-armv7 ARM (VFP hard-float needed).
Build from source instead: see the README Install section." ;;
    i686|i586|i486|i386)
        grep -q '\bsse2\b' "${TEMUR_CPUINFO:-/proc/cpuinfo}" \
            || fail "this 32-bit x86 CPU lacks SSE2, which the i686 build requires.
Build from source instead: see the README Install section."
        TRIPLE=i686-unknown-linux-musl ;;
    *) fail "unsupported architecture '$(uname -m)'.
Build from source instead: see the README Install section." ;;
esac

command -v sha256sum >/dev/null 2>&1 \
    || fail "sha256sum not found; refusing to install unverified binaries."

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -q -O "$2" "$1"; }
else
    fail "need curl or wget."
fi

ART="temur-$TAG-$TRIPLE"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

echo "downloading $ART ..."
fetch "$BASE/$ART" "$TMP/$ART"           || fail "download failed: $BASE/$ART"
fetch "$BASE/SHA256SUMS" "$TMP/SHA256SUMS" || fail "download failed: $BASE/SHA256SUMS"

# Portable verify (busybox/Alpine ship a sha256sum without --ignore-missing
# or --quiet): extract our artifact's expected hash from SHA256SUMS and
# compare strings. An artifact missing from SHA256SUMS is a hard fail — an
# empty expectation must never verify.
EXPECTED=$(awk -v f="$ART" '$2 == f { print $1; exit }' "$TMP/SHA256SUMS")
[ -n "$EXPECTED" ] || fail "$ART is not listed in SHA256SUMS — not installing."
ACTUAL=$(sha256sum "$TMP/$ART" | awk '{ print $1 }')
[ "$ACTUAL" = "$EXPECTED" ] || fail "CHECKSUM VERIFICATION FAILED — not installing."
echo "checksum verified."

mkdir -p "$DIR"
install -m 755 "$TMP/$ART" "$DIR/temur"
echo "installed: $DIR/temur"

case ":$PATH:" in
    *":$DIR:"*) ;;
    *) echo "note: $DIR is not on your PATH." ;;
esac

"$DIR/temur" --version
