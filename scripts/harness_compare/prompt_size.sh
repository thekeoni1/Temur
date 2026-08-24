#!/bin/sh
# T37 prompt-size measurement (Decision C's MEASURED finding): how many
# prompt tokens each harness spends before the model has done any work.
#
# Method, stated because the number is only meaningful with it: run ONE
# identical task per harness against the same served model, then read the
# token count llama.cpp itself reports for that harness's first AGENT
# request (the request that carries the tool definitions). The count is
# server-side, not an estimate and not self-reported by the harness.
#
# OpenCode's first request is a session-TITLE generation call carrying no
# tools; the agent request is the one after it. Selecting "first request"
# blindly would understate OpenCode by roughly 7k tokens, so the largest
# prompt-eval count in the window is taken instead: the tool-carrying
# request is always the biggest, and it is the number that matters for the
# context arithmetic.
#
# Usage: scripts/harness_compare/prompt_size.sh <harness> [outfile]
set -eu
cd "$(dirname "$0")/../.."

HARNESS=${1:?usage: prompt_size.sh <temur|opencode|codex> [outfile]}
OUTFILE=${2:-}
CONTAINER="${CONTAINER_NAME:-temur-llama}"
ARCHIVE_DIR="${ARCHIVE_DIR:-$HOME/temur-eval-archive/t37-harness-compare}"
MODEL_LABEL="${MODEL_LABEL:-promptsize}"

# Mark the current end of the server log so only this run is measured.
MARK=$(podman logs "$CONTAINER" 2>&1 | wc -l)

TASK_TIMEOUT="${TASK_TIMEOUT:-600}" \
ARCHIVE_DIR="$ARCHIVE_DIR" MODEL_LABEL="$MODEL_LABEL" \
    scripts/harness_compare/run.sh "$HARNESS" 0 >/dev/null 2>&1 || true

TOK=$(podman logs "$CONTAINER" 2>&1 | tail -n +"$((MARK + 1))" \
    | grep -o 'prompt eval time =[^/]*/ *[0-9]* tokens' \
    | grep -o '[0-9]* tokens' | grep -o '[0-9]*' \
    | sort -n | tail -1)

REQS=$(podman logs "$CONTAINER" 2>&1 | tail -n +"$((MARK + 1))" \
    | grep -c 'prompt eval time =' || true)

printf '%s\tlargest_prompt_tokens=%s\trequests=%s\n' "$HARNESS" "${TOK:-unmeasured}" "${REQS:-0}"
[ -n "$OUTFILE" ] && printf '%s\t%s\t%s\n' "$HARNESS" "${TOK:-unmeasured}" "${REQS:-0}" >> "$OUTFILE"
exit 0
