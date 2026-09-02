#!/bin/sh
# The readiness tally is a count, so count it.
#
# `docs/release-readiness.md` opens with **N of M**, and that line was written
# by hand. It said 153 of 222 while the boxes said 150 -- for long enough that
# several commits shipped against it, and long enough that the number was
# quoted in a status report before anybody counted. A gate that overstates
# itself is worse than no gate: the whole point of the document is that it is
# scored against the tree rather than against an account of the tree.
#
# It understated too, which is the half nobody looks for. Five items were open
# whose `**Left:**` described conditions that had been fixed commits earlier --
# `khora --version` printing no build metadata, no `CHANGELOG.md`, no lint
# page. This script cannot catch those; only re-reading each item can, and
# #173 is that. What this catches is the arithmetic, for ever.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

doc=${1:-docs/release-readiness.md}

done_count=$(grep -c '^[[:space:]]*- \[x\]' "$doc" || true)
open_count=$(grep -c '^[[:space:]]*- \[ \]' "$doc" || true)
total=$((done_count + open_count))

claim=$(grep -o '\*\*[0-9]\{1,\} of [0-9]\{1,\}\*\*' "$doc" | head -1 | tr -d '*')
claimed_done=$(printf '%s' "$claim" | cut -d' ' -f1)
claimed_total=$(printf '%s' "$claim" | cut -d' ' -f3)

if [ -z "$claim" ]; then
    printf '  FAILED  %s has no "**N of M**" line to check\n' "$doc"
    exit 1
fi

if [ "$claimed_done" != "$done_count" ] || [ "$claimed_total" != "$total" ]; then
    printf '  FAILED  the readiness tally disagrees with the boxes\n'
    printf '          the header says  %s of %s\n' "$claimed_done" "$claimed_total"
    printf '          the boxes say    %s of %s  (%s still open)\n' "$done_count" "$total" "$open_count"
    exit 1
fi

printf '  ok    the readiness tally is %s of %s, and the boxes agree\n' "$done_count" "$total"
