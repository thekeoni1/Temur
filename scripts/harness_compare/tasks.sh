# T37 shared task definitions: the nine eval tasks' PROMPT LITERALS, kept
# byte-identical to scripts/weak_model_eval.sh so a cross-harness score is
# a comparison of harnesses and not of wording (the T32 lesson).
#
# This file is GENERATED-SHAPED but hand-maintained: the literals below are
# copied verbatim from weak_model_eval.sh's run_round, including their
# original quoting. tests/harness_compare_drift.sh re-extracts them from
# weak_model_eval.sh and compares BYTE FOR BYTE, so an edit to either file
# that changes a prompt fails the gate rather than silently invalidating a
# published table.
#
# Deliberately NOT extracted-and-sourced back into weak_model_eval.sh:
# that script is gate-covered and underpins the published OFFLINE.md
# matrix, so it is left byte-identical and the pin test carries the
# no-drift guarantee instead.

PROMPT_1='Use the write tool to create a file named hello.txt containing exactly this text: hello-eval'
PROMPT_2="Two steps. Step 1: use the read tool on data.txt. It holds a single line that begins with 'token: ' and ends with a code. Step 2: use the write tool to create token.txt whose content is that code, meaning the text that follows 'token: ' on the line you just read, and nothing else."
PROMPT_3="Edit the file config.ini: change the line 'mode = development' to 'mode = production'. Do not change anything else in the file."
PROMPT_4='Use the bash tool to create a directory named build containing a file marker.txt with the text: done  (so the file is build/marker.txt)'
PROMPT_5='Three files exist here: alpha.txt, beta.txt, gamma.txt. Exactly one of them contains the string NEEDLE-4242. Find which file contains it (grep or read), then use the write tool to create found.txt containing that file name.'
PROMPT_6='The file version.txt contains 1.2.3. First edit version.txt so it contains 1.2.4 instead. Then, after the edit, use the bash tool to run: cp version.txt version.bak'
PROMPT_7='Delete the file obsolete.tmp in the current directory.'
PROMPT_8='Create a gzip-compressed file named notes.txt.gz in the current directory. Its DECOMPRESSED content must be exactly: eval-gz-99   (gzip is available via the bash tool).'
PROMPT_9='Two steps. Step 1: use the bash tool to run exactly: cat data.log   (the output is long and will be truncated in the middle). Step 2: the LAST line of that output begins with "FINAL-LINE: " and ends with a code. Use the write tool to create tail.txt containing that code, meaning the text that follows "FINAL-LINE: " on that last line, and nothing else.'

# Task names, in the same order, matching weak_model_eval.sh's `name=` values.
TASK_NAMES='write-file read-extract edit-config bash-mkdir find-needle bump-and-copy indirect-delete binary-nudge large-tail'
