#!/bin/sh
# Installer test matrix (v0.1.1 / review finding F2): exercises
# scripts/install.sh against local mirrors of a staged release dir, on the
# GNU host (curl, GNU sha256sum) AND inside the bare busybox container
# (busybox sh + wget + busybox sha256sum — the musl-audience environment the
# v0.1.0 installer's GNU-only `sha256sum -c --ignore-missing` broke on).
#
# usage: scripts/install_test.sh [staged-dir]
#   staged-dir defaults to /home/dev/dist/release/v<Cargo.toml version>
#   (run scripts/release.sh first to stage it).
#
# Cases, each run in BOTH environments:
#   pass      — clean mirror: installs, installed `temur --version` matches
#   corrupt   — artifact corrupted: hard fail, nothing installed
#   unlisted  — artifact missing from SHA256SUMS: hard fail, nothing installed
set -eu
cd "$(dirname "$0")/.."
PROJ="$(pwd)"

BARE_IMG=docker.io/library/busybox:stable

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$VERSION" ] || { echo "FAIL: cannot extract version from Cargo.toml"; exit 1; }
STAGE="${1:-/home/dev/dist/release/v$VERSION}"
ART="temur-v$VERSION-x86_64-unknown-linux-musl"
[ -f "$STAGE/$ART" ] || { echo "FAIL: $STAGE/$ART missing — run scripts/release.sh first"; exit 1; }
[ -f "$STAGE/SHA256SUMS" ] || { echo "FAIL: $STAGE/SHA256SUMS missing"; exit 1; }

TMPROOT=$(mktemp -d)
trap 'rm -rf "$TMPROOT"' EXIT
chmod 755 "$TMPROOT" # the container's httpd runs as a different uid

# Three mirrors: clean, corrupted artifact, artifact dropped from SHA256SUMS.
for m in good corrupt unlisted; do
    mkdir "$TMPROOT/$m"
    cp "$STAGE"/temur-v"$VERSION"-* "$TMPROOT/$m/"
    cp "$STAGE"/SHA256SUMS "$TMPROOT/$m/"
done
printf 'X' >> "$TMPROOT/corrupt/$ART"
grep -v "$ART\$" "$STAGE/SHA256SUMS" > "$TMPROOT/unlisted/SHA256SUMS"

# One case-runner, shared verbatim by host and container.
cat > "$TMPROOT/runner.sh" <<'EOF'
#!/bin/sh
# $1=base-url  $2=expected-version  $3=case  $4=install.sh path  $5=label
set -u
BASE="$1"; VER="$2"; CASE="$3"; SCRIPT="$4"; LABEL="$5"
H=$(mktemp -d)
OUT=$(HOME="$H" TEMUR_BASE_URL="$BASE" sh "$SCRIPT" 2>&1); RC=$?
BIN="$H/.local/bin/temur"
case "$CASE" in
    pass)
        [ "$RC" -eq 0 ] || { echo "FAIL($LABEL/$CASE): rc=$RC"; echo "$OUT"; exit 1; }
        echo "$OUT" | grep -q "checksum verified" \
            || { echo "FAIL($LABEL/$CASE): no verify line"; echo "$OUT"; exit 1; }
        V=$("$BIN" --version) || { echo "FAIL($LABEL/$CASE): installed binary won't run"; exit 1; }
        [ "$V" = "temur $VER" ] \
            || { echo "FAIL($LABEL/$CASE): version '$V', want 'temur $VER'"; exit 1; }
        ;;
    corrupt)
        [ "$RC" -ne 0 ] || { echo "FAIL($LABEL/$CASE): unexpectedly succeeded"; echo "$OUT"; exit 1; }
        echo "$OUT" | grep -q "CHECKSUM VERIFICATION FAILED" \
            || { echo "FAIL($LABEL/$CASE): wrong error"; echo "$OUT"; exit 1; }
        [ ! -e "$BIN" ] || { echo "FAIL($LABEL/$CASE): installed despite bad checksum"; exit 1; }
        ;;
    unlisted)
        [ "$RC" -ne 0 ] || { echo "FAIL($LABEL/$CASE): unexpectedly succeeded"; echo "$OUT"; exit 1; }
        echo "$OUT" | grep -q "not listed in SHA256SUMS" \
            || { echo "FAIL($LABEL/$CASE): wrong error"; echo "$OUT"; exit 1; }
        [ ! -e "$BIN" ] || { echo "FAIL($LABEL/$CASE): installed despite unlisted artifact"; exit 1; }
        ;;
    *) echo "FAIL($LABEL): unknown case $CASE"; exit 1 ;;
esac
rm -rf "$H"
echo "PASS($LABEL/$CASE)"
EOF

echo "== installer matrix: GNU host (curl + GNU sha256sum, file:// mirrors) =="
for c in pass corrupt unlisted; do
    case "$c" in pass) m=good ;; *) m=$c ;; esac
    sh "$TMPROOT/runner.sh" "file://$TMPROOT/$m" "$VERSION" "$c" scripts/install.sh host
done

echo "== installer matrix: busybox container (busybox sh + wget + sha256sum) =="
podman run --rm -v "$TMPROOT":/m:ro -v "$PROJ":"$PROJ":ro "$BARE_IMG" sh -c "
    httpd -p 127.0.0.1:8081 -h /m/good &&
    httpd -p 127.0.0.1:8082 -h /m/corrupt &&
    httpd -p 127.0.0.1:8083 -h /m/unlisted &&
    sh /m/runner.sh http://127.0.0.1:8081 '$VERSION' pass     '$PROJ/scripts/install.sh' busybox &&
    sh /m/runner.sh http://127.0.0.1:8082 '$VERSION' corrupt  '$PROJ/scripts/install.sh' busybox &&
    sh /m/runner.sh http://127.0.0.1:8083 '$VERSION' unlisted '$PROJ/scripts/install.sh' busybox
"

echo "== INSTALLER MATRIX PASSED (6/6: pass+corrupt+unlisted on host and busybox) =="
