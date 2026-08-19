#!/bin/sh
# Cross-check temur's baked Anthropic model metadata against models.dev.
#
# REPORT-ONLY, BY DESIGN. This script never edits anything. models.dev is
# community-maintained data, so it is a cross-check, not an oracle: a DRIFT
# line is a prompt for a human to go read Anthropic's own pricing page and
# decide, not an instruction to change the baked table. It exists because
# T35 P1 found the baked sonnet price stale by twelve days and nothing in
# the repo would have noticed.
#
# Deliberately NOT wired into scripts/check.sh or scripts/release.sh: a gate
# that reaches the network fails for reasons that have nothing to do with the
# thing it gates. docs/RUNBOOK.md's stage-2 ship procedure calls it by hand
# and records the outcome in the ship record.
#
# The baked values are read out of src/init.rs at run time. This script does
# NOT keep its own copy of the table: a second copy would be one more thing
# that goes stale, which is the exact defect being checked for.
#
# Exit 0 when every model PASSes; nonzero on any drift, any model missing
# from models.dev, or any fetch/parse failure.
#
# Env knobs (for exercising the failure arms):
#   MODELS_DEV_URL   override the endpoint (a bad URL exercises fetch-fail)
#   TEMUR_INIT_RS    override the path to init.rs (a mangled copy
#                    exercises parse-fail)
set -eu

URL="${MODELS_DEV_URL:-https://models.dev/api.json}"
INIT_RS="${TEMUR_INIT_RS:-$(dirname "$0")/../src/init.rs}"

if [ ! -f "$INIT_RS" ]; then
    echo "metadata_drift: no such file: $INIT_RS" >&2
    echo "  (set TEMUR_INIT_RS to point at the file holding ANTHROPIC_PROFILES)" >&2
    exit 2
fi

command -v python3 >/dev/null 2>&1 || {
    echo "metadata_drift: python3 not found; this script parses JSON with python3, not jq" >&2
    exit 2
}

JSON=$(mktemp)
trap 'rm -f "$JSON"' EXIT

echo "== metadata drift: baked ($INIT_RS) vs $URL =="

if ! curl -fsS --max-time 30 "$URL" -o "$JSON"; then
    echo "FETCH-FAIL: could not fetch $URL within 30s" >&2
    echo "  Offline, or models.dev is down or moved. This check needs the network;" >&2
    echo "  it is report-only, so a ship may proceed with the outcome recorded as" >&2
    echo "  'not run (offline)'." >&2
    exit 3
fi

INIT_RS="$INIT_RS" python3 - "$JSON" <<'PY'
import json, os, re, sys

init_path = os.environ["INIT_RS"]
src = open(init_path, encoding="utf-8").read()

# Parse the ANTHROPIC_PROFILES const block. A miss here is LOUD: it means the
# const was renamed, moved, or reshaped, and a silent pass would report
# "nothing drifted" while checking nothing at all.
m = re.search(
    r"const\s+ANTHROPIC_PROFILES\s*:\s*\[\([^\]]*?\)\s*;\s*(\d+)\s*\]\s*=\s*\[(.*?)\n\];",
    src,
    re.S,
)
if not m:
    sys.exit(
        "PARSE-FAIL: could not find the ANTHROPIC_PROFILES const block in "
        f"{init_path}\n"
        "  Expected `const ANTHROPIC_PROFILES: [(...); N] = [ ... ];`.\n"
        "  If the const was renamed or moved, update this script to match; do\n"
        "  NOT let it fall through, or it reports a clean run while checking\n"
        "  nothing."
    )

declared = int(m.group(1))
rows = re.findall(
    r'\(\s*"([^"]+)"\s*,\s*"([^"]+)"\s*,\s*([0-9_]+)\s*,\s*([0-9._]+)\s*,\s*([0-9._]+)\s*\)',
    m.group(2),
)
if len(rows) != declared:
    sys.exit(
        f"PARSE-FAIL: ANTHROPIC_PROFILES declares {declared} entries but "
        f"{len(rows)} parsed out of {init_path}\n"
        "  The table and this script's regex have diverged. Fix one or the other."
    )
if declared != 4:
    sys.exit(
        f"PARSE-FAIL: expected 4 baked Anthropic profiles, found {declared} in "
        f"{init_path}\n"
        "  A tier was added or removed. Confirm the new set is intentional, then\n"
        "  update this expectation."
    )

try:
    data = json.load(open(sys.argv[1], encoding="utf-8"))
except Exception as e:
    sys.exit(f"PARSE-FAIL: models.dev payload is not valid JSON: {e}")

if not isinstance(data, dict) or "anthropic" not in data:
    sys.exit(
        "PARSE-FAIL: models.dev payload has no top-level 'anthropic' key.\n"
        "  The feed's shape changed; update this script before trusting it."
    )
remote = data["anthropic"].get("models")
if not isinstance(remote, dict) or not remote:
    sys.exit("PARSE-FAIL: models.dev 'anthropic.models' is missing or empty.")


def num(v):
    return float(v) if isinstance(v, (int, float)) else None


def fmt(v):
    if v is None:
        return "absent"
    return str(int(v)) if float(v).is_integer() else str(v)


bad = 0
for name, model_id, window, pin, pout in rows:
    baked = {
        "context": float(window.replace("_", "")),
        "input": float(pin.replace("_", "")),
        "output": float(pout.replace("_", "")),
    }
    entry = remote.get(model_id)
    if entry is None:
        print(
            f"MISSING: {name} ({model_id}) is not in the models.dev anthropic "
            "listing; nothing to compare"
        )
        bad += 1
        continue

    live = {
        "context": num((entry.get("limit") or {}).get("context")),
        "input": num((entry.get("cost") or {}).get("input")),
        "output": num((entry.get("cost") or {}).get("output")),
    }

    drifts = [
        f"{field} baked {fmt(baked[field])} vs models.dev {fmt(live[field])}"
        for field in ("context", "input", "output")
        if live[field] is None or abs(live[field] - baked[field]) > 1e-9
    ]
    if drifts:
        print(f"DRIFT: {name} ({model_id}): " + "; ".join(drifts))
        bad += 1
    else:
        print(
            f"PASS: {name} ({model_id}): context {fmt(baked['context'])}, "
            f"input ${fmt(baked['input'])}/MTok, output ${fmt(baked['output'])}/MTok"
        )

print()
if bad:
    print(
        f"{bad} of {len(rows)} baked profiles differ from models.dev. This is a\n"
        "REPORT, not a verdict: models.dev is community data and can itself be\n"
        "stale or wrong. Check Anthropic's pricing page and the authenticated\n"
        "/v1/models listing before changing anything, then record the decision\n"
        "in the ship record."
    )
    sys.exit(1)
print(f"all {len(rows)} baked profiles match models.dev")
PY
