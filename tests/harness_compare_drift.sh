#!/bin/sh
# T37 drift pin: the nine task prompts in scripts/harness_compare/tasks.sh
# must stay BYTE-IDENTICAL to the literals in scripts/weak_model_eval.sh.
#
# Why a pin rather than a shared sourced file: weak_model_eval.sh is
# gate-covered and its wording underpins the published OFFLINE.md matrix,
# so it is left untouched and this test carries the no-drift guarantee.
# Rewording a prompt on either side invalidates every cross-harness score
# already published (the T32 lesson), so that must fail loudly here rather
# than quietly produce a table nobody can compare.
#
# The comparison is on the RAW SOURCE LITERAL including its quoting, not on
# an evaluated string: no eval runs over file content, and identical source
# bytes are a strictly stronger claim than identical expansions.
set -eu
cd "$(dirname "$0")/.."

EVAL=scripts/weak_model_eval.sh
TASKS=scripts/harness_compare/tasks.sh
fail() { echo "FAIL: $*" >&2; exit 1; }

[ -f "$EVAL" ] || fail "missing $EVAL"
[ -f "$TASKS" ] || fail "missing $TASKS"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# Left side: the prompt literal is the line following each run_task call.
grep -A1 'run_task "\$n" "\$name" \\' "$EVAL" \
    | grep -v 'run_task\|^--' | sed 's/^    //' > "$TMP/from_eval"

# Right side: everything after the first '=' on each PROMPT_n line.
grep '^PROMPT_[1-9]=' "$TASKS" | sed 's/^PROMPT_[1-9]=//' > "$TMP/from_tasks"

N_EVAL=$(wc -l < "$TMP/from_eval")
N_TASK=$(wc -l < "$TMP/from_tasks")

# A zero-length extraction must never pass as "identical": that is how a
# refactor of run_task's call shape would silently disable this pin.
[ "$N_EVAL" -eq 9 ] \
    || fail "extracted $N_EVAL prompts from $EVAL, expected 9 (did run_task's call shape change?)"
[ "$N_TASK" -eq 9 ] \
    || fail "found $N_TASK PROMPT_n lines in $TASKS, expected 9"

if ! cmp -s "$TMP/from_eval" "$TMP/from_tasks"; then
    echo "FAIL: task prompts have DRIFTED between $EVAL and $TASKS" >&2
    echo "  Any score table built from the drifted text is not comparable." >&2
    diff -u "$TMP/from_eval" "$TMP/from_tasks" >&2 || true
    exit 1
fi

# The task-name list must match the eval's `name=` values, same order.
grep -o '^n=[1-9]; name=[a-z-]*' "$EVAL" | sed 's/.*name=//' > "$TMP/names_eval"
# shellcheck disable=SC1090
. "$TASKS"
printf '%s\n' $TASK_NAMES > "$TMP/names_tasks"
cmp -s "$TMP/names_eval" "$TMP/names_tasks" \
    || { echo "FAIL: task NAMES drifted" >&2; diff -u "$TMP/names_eval" "$TMP/names_tasks" >&2 || true; exit 1; }

echo "OK: 9 task prompts and 9 task names byte-identical between $EVAL and $TASKS"
