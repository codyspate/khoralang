#!/bin/sh
# Blocks until the newest `ci.yml` run finishes, then prints every failure.
#
# **A poll I have to remember is a poll I will forget.** The agent working
# through a conversion is not watching a browser, and `gh run view` on a live
# run answers "logs will be available when it is complete" -- so a check made at
# the wrong moment reads as no news rather than as not-yet. This blocks instead,
# and the caller runs it in the background with a completion notification.
#
#     sh scripts/ci-watch.sh            # the newest run
#     sh scripts/ci-watch.sh 34564772164 # a particular one
#
# Exits 0 when the run is green, 1 when anything failed. The failure output is
# every job's `--log-failed`, filtered to the lines that say what broke: a
# panic, a failing test, a compiler error, a step's exit code.
set -eu

run=${1:-}
if [ -z "$run" ]; then
    run=$(gh run list --workflow=ci.yml --limit 1 --json databaseId -q '.[0].databaseId')
fi
[ -n "$run" ] || { echo "no run to watch" >&2; exit 2; }

echo "watching run $run"
# `gh run watch` polls and returns when the run ends. `--exit-status` makes a
# failed run a non-zero exit here, which the notification then carries.
gh run watch "$run" --exit-status > /dev/null 2>&1 && verdict=0 || verdict=1

printf '\n=== run %s: %s ===\n' "$run" "$([ "$verdict" -eq 0 ] && echo GREEN || echo RED)"
gh run view "$run" --json jobs \
    -q '.jobs[] | "\(.conclusion // "-")  \(.name)"' 2>/dev/null | sort || true

if [ "$verdict" -ne 0 ]; then
    printf '\n=== what failed ===\n'
    # One pass per failed job. `--log-failed` is only the failing steps, and the
    # filter is the vocabulary a failure actually uses -- a panic line, a
    # nextest FAIL, a Khora diagnostic, a step's exit code.
    gh run view "$run" --json jobs -q '.jobs[] | select(.conclusion == "failure") | .databaseId' \
    | while read -r job; do
        name=$(gh run view --job="$job" --json name -q '.name' 2>/dev/null || echo "job $job")
        printf '\n--- %s ---\n' "$name"
        gh run view --job="$job" --log-failed 2>/dev/null \
            | grep -aiE 'panicked at|FAIL \[|test result: FAILED|^error|error:|would reformat|member\(s\) failed|##\[error\]|assertion' \
            | head -25 \
            | cut -c1-200 || true
    done
fi

exit "$verdict"
