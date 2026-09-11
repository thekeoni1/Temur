#!/bin/sh
# T37 drift pin: the nine task prompts in scripts/harness_compare/tasks.sh
# must stay BYTE-IDENTICAL to the literals in scripts/weak_model_eval.sh.
#
# T58: the eval now has TEN run_task calls. The tenth (D22
# resume-feedback) is deliberately not carried into tasks.sh, because
# harness_compare scores the nine imperative tasks ACROSS harnesses and
# task 10 measures temur's own file-denial recovery, which no other
# harness has. So the extraction below expects ten, compares the first
# nine, and pins the tenth by name and by text: that way task 10 can
# neither drift into the compared set nor be quietly renumbered into it.
#
# T60: tasks 11 (memo-docx) and 12 (summary-pdf) join task 10 on the held
# out side, for the same reason: they score temur's own document writing,
# which no other harness has. Twelve extracted, nine compared, three
# pinned by name and text.
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
[ "$N_EVAL" -eq 12 ] \
    || fail "extracted $N_EVAL prompts from $EVAL, expected 12 (did run_task's call shape change?)"
[ "$N_TASK" -eq 9 ] \
    || fail "found $N_TASK PROMPT_n lines in $TASKS, expected 9"

# The tenth, eleventh and twelfth prompts, pinned verbatim and then held
# out of the comparison.
D22_PROMPT="'can you read my resume and give me feedback?'"
[ "$(sed -n 10p "$TMP/from_eval")" = "$D22_PROMPT" ] \
    || fail "the tenth eval prompt is not the D22 literal: $(sed -n 10p "$TMP/from_eval")"
T60_DOCX_PROMPT="'Write a short memo to the team announcing that the Friday standup moves to 9:30, and save it as memo.docx.'"
[ "$(sed -n 11p "$TMP/from_eval")" = "$T60_DOCX_PROMPT" ] \
    || fail "the eleventh eval prompt is not the T60 memo-docx literal: $(sed -n 11p "$TMP/from_eval")"
T60_PDF_PROMPT="'Summarise the three points below into a one-page document and save it as summary.pdf. Point 1: the Tinyq-Rollout finished on Tuesday, with the new build on every branch office machine. Point 2: support tickets fell by a third in the week after it. Point 3: the old build is switched off at the end of the month.'"
[ "$(sed -n 12p "$TMP/from_eval")" = "$T60_PDF_PROMPT" ] \
    || fail "the twelfth eval prompt is not the T60 summary-pdf literal: $(sed -n 12p "$TMP/from_eval")"
head -n 9 "$TMP/from_eval" > "$TMP/from_eval_nine"

if ! cmp -s "$TMP/from_eval_nine" "$TMP/from_tasks"; then
    echo "FAIL: task prompts have DRIFTED between $EVAL and $TASKS" >&2
    echo "  Any score table built from the drifted text is not comparable." >&2
    diff -u "$TMP/from_eval_nine" "$TMP/from_tasks" >&2 || true
    exit 1
fi

# The task-name list must match the eval's `name=` values, same order.
# The digit class takes 10 as well as 1-9, so a renumbering cannot slip a
# task past this by widening past the old single-digit pattern.
grep -o '^n=[0-9][0-9]*; name=[a-z-]*' "$EVAL" | sed 's/.*name=//' > "$TMP/names_all"
[ "$(wc -l < "$TMP/names_all")" -eq 12 ] \
    || fail "extracted $(wc -l < "$TMP/names_all") task names from $EVAL, expected 12"
[ "$(sed -n 10p "$TMP/names_all")" = "resume-feedback" ] \
    || fail "the tenth task is not resume-feedback: $(sed -n 10p "$TMP/names_all")"
[ "$(sed -n 11p "$TMP/names_all")" = "memo-docx" ] \
    || fail "the eleventh task is not memo-docx: $(sed -n 11p "$TMP/names_all")"
[ "$(sed -n 12p "$TMP/names_all")" = "summary-pdf" ] \
    || fail "the twelfth task is not summary-pdf: $(sed -n 12p "$TMP/names_all")"
head -n 9 "$TMP/names_all" > "$TMP/names_eval"
# shellcheck disable=SC1090
. "$TASKS"
printf '%s\n' $TASK_NAMES > "$TMP/names_tasks"
cmp -s "$TMP/names_eval" "$TMP/names_tasks" \
    || { echo "FAIL: task NAMES drifted" >&2; diff -u "$TMP/names_eval" "$TMP/names_tasks" >&2 || true; exit 1; }

echo "OK: 9 task prompts and 9 task names byte-identical between $EVAL and $TASKS"
echo "OK: tasks 10 to 12 (resume-feedback, memo-docx, summary-pdf) present in $EVAL and held out of $TASKS"
