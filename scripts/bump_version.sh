#!/bin/sh
# Stage-1 version bump helper. Rewrites the four files that pin the
# release version (Cargo.toml, Cargo.lock via cargo update, the
# scripts/install.sh VERSION line, and the README tag pins), prints
# the resulting diff, and commits NOTHING. release.sh gate 3 stays the
# authority on version skew; this script only mechanizes the edit and
# is deliberately not wired into release.sh or check.sh. Run by hand:
#   scripts/bump_version.sh 0.11.0
set -eu

[ "$#" -eq 1 ] || { echo "usage: scripts/bump_version.sh NEW_VERSION (bare, e.g. 0.11.0)" >&2; exit 2; }
NEW=$1
echo "$NEW" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' \
    || { echo "refusing: \"$NEW\" is not a bare MAJOR.MINOR.PATCH version" >&2; exit 2; }

cd "$(dirname "$0")/.."

[ -z "$(git status --porcelain)" ] \
    || { echo "refusing: working tree is not clean" >&2; exit 1; }

OLD=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
[ -n "$OLD" ] || { echo "cannot read the current version from Cargo.toml" >&2; exit 1; }
[ "$OLD" != "$NEW" ] || { echo "refusing: already at version $NEW" >&2; exit 1; }

INST=$(sed -n 's/^VERSION=//p' scripts/install.sh)
[ "$INST" = "$OLD" ] \
    || { echo "refusing: scripts/install.sh VERSION=$INST but Cargo.toml says $OLD; fix the skew first" >&2; exit 1; }

PINS=$(grep -c "v$OLD" README.md) \
    || { echo "refusing: no v$OLD tag pins found in README.md" >&2; exit 1; }

echo "bump $OLD -> $NEW (README carries $PINS v$OLD pin lines)"

sed -i "s/^version = \"$OLD\"\$/version = \"$NEW\"/" Cargo.toml
cargo update -p temur --offline
sed -i "s/^VERSION=$OLD\$/VERSION=$NEW/" scripts/install.sh
sed -i "s/v$OLD/v$NEW/g" README.md

LEFT=$(grep -c "v$OLD" README.md || true)
[ "$LEFT" -eq 0 ] \
    || echo "WARNING: $LEFT v$OLD pin lines remain in README.md, inspect them" >&2

git --no-pager diff --stat
git --no-pager diff
echo "== staged nothing, committed nothing: review the diff above, then commit it as the stage-1 bump =="
