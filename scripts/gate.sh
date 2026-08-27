#!/bin/sh
# Which gate has passed for the tree as it stands right now.
#
#     sh scripts/gate.sh            the full gate: `scripts/baseline.sh`
#     sh scripts/gate.sh fast       the fast gate: `scripts/check.sh native`
#
# Exits zero if that gate finished clean and nothing tracked has changed since.
# Exits non-zero, loudly, otherwise.
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
#
# # Two gates, because they answer different questions
#
# **fast** is the whole test suite, front end and back end: `check.sh native`.
# Two minutes, and it catches anything that is a bug in the compiler.
#
# **full** is that plus clippy, the corpus check, the formatter check, the
# generated reference, the package tests, four example builds, the HTTP
# conformance suite against a real `curl`, and the runtime on Linux through
# WSL. It is what a push has to clear.
#
# Passing the full gate satisfies the fast one, which is the only sensible
# reading: a superset that passed cannot leave its subset in doubt. The reverse
# is not true and is the entire point of having two names. Roadmap 14.32.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

want=${1:-full}
case "$want" in
    fast | full) ;;
    *)
        printf 'gate: `%s` is not a gate. There are two: `fast` and `full`.\n' "$want" >&2
        exit 2
        ;;
esac

now=$(sh "$root/scripts/tree-id.sh")

# The full receipt answers for both, so it is tried first either way.
passed=''
for gate in full fast; do
    receipt="$root/.khora-gate-$gate"
    [ -f "$receipt" ] || continue
    [ "$(cat "$receipt")" = "$now" ] || continue
    passed="$gate"
    break
done

if [ "$passed" = full ] || [ "$passed" = "$want" ]; then
    printf 'gate: the %s gate passed for this tree (%s)\n' "$passed" "$now"
    exit 0
fi

# Nothing current. Say which command writes the receipt that is missing, and
# say whether the problem is "never run" or "run, then something changed".
if [ "$want" = fast ]; then
    command='sh scripts/check.sh native'
else
    command='sh scripts/baseline.sh'
fi

# Three different things can be wrong, and saying which is most of the value.
# A receipt for *this* tree from a lesser gate is not a stale receipt, and
# telling somebody their tree changed when it did not sends them to look for an
# edit that is not there.
stale=''
for gate in full fast; do
    receipt="$root/.khora-gate-$gate"
    if [ -f "$receipt" ]; then
        stale="$stale  $gate passed for  $(cat "$receipt")
"
    fi
done

printf 'gate: no current %s receipt.\n' "$want" >&2
printf '\n' >&2
printf '  %s\n' "$command" >&2
printf '\n' >&2
if [ "$passed" = fast ]; then
    printf 'writes one when it finishes clean. The tree is fine -- the *fast* gate\n' >&2
    printf 'passed for it. The full gate adds clippy, the corpus and formatter\n' >&2
    printf 'checks, the generated reference, the reference applications, the HTTP\n' >&2
    printf 'conformance suite and the runtime on Linux, and none of those has run.\n' >&2
elif [ -n "$stale" ]; then
    printf 'writes one when it finishes clean. What is on disk is for another tree:\n' >&2
    printf '\n' >&2
    printf '%s' "$stale" >&2
    printf '  this tree        %s\n' "$now" >&2
    printf '\n' >&2
    printf 'Something tracked has changed since. Note that a `git add` of a *new*\n' >&2
    printf 'file does this too: the tracked list is part of what a receipt names.\n' >&2
    printf 'Stage first, then run the gate, then commit -- which is the right\n' >&2
    printf 'order anyway, since a file that is not staged is not in the commit.\n' >&2
else
    printf 'writes one when it finishes clean. There is no receipt at all, which\n' >&2
    printf 'means it has not been run for this tree, or it failed.\n' >&2
fi
exit 1
