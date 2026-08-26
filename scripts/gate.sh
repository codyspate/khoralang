#!/bin/sh
# Whether the baseline has passed for the tree as it stands right now.
#
#     sh scripts/gate.sh
#
# Exits zero if `scripts/baseline.sh` finished clean and nothing tracked has
# changed since. Exits non-zero, loudly, otherwise.
#
# # Why this exists
#
# `scripts/baseline.sh` already exits non-zero on the first failure. The
# failure this is here for is one level up, in whatever ran it: a `| grep`, a
# `| tail`, a `&&` chain, a status read one command too late. Three commits
# have gone out on a red baseline that way, and every one of them looked fine
# on the screen at the time.
#
# So the question stops being "what did that command return" and becomes "is
# there a receipt for this tree". A status is a thing you can forget to read.
# A file is a thing you can ask about afterwards — from a hook, from CI, from a
# terminal an hour later — and the answer does not depend on how it was run.
#
# Roadmap 13.20.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

receipt="$root/.khora-baseline-ok"

if [ ! -f "$receipt" ]; then
    printf 'gate: no baseline receipt.\n' >&2
    printf '\n' >&2
    printf '  sh scripts/baseline.sh\n' >&2
    printf '\n' >&2
    printf 'writes one when it finishes clean. There is no receipt now, which\n' >&2
    printf 'means the baseline has not been run for this tree, or it failed.\n' >&2
    exit 1
fi

was=$(cat "$receipt")
now=$(sh "$root/scripts/tree-id.sh")

if [ "$was" != "$now" ]; then
    printf 'gate: the baseline receipt is for a different tree.\n' >&2
    printf '\n' >&2
    printf '  passed for  %s\n' "$was" >&2
    printf '  this tree   %s\n' "$now" >&2
    printf '\n' >&2
    printf 'Something tracked has changed since the baseline passed. Run it\n' >&2
    printf 'again:  sh scripts/baseline.sh\n' >&2
    exit 1
fi

printf 'gate: the baseline passed for this tree (%s)\n' "$was"
