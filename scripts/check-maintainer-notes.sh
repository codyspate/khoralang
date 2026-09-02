#!/bin/sh
# `no-maintainer-notes.sh` against known input.
#
# Both halves matter. A checker that misses the thing it was written for is
# useless, and a checker that fails a correct page is one somebody switches
# off -- so the passing cases here are the ones most likely to be caught by
# accident: "used to" as ordinary English, "used to" as a real migration note,
# a script a reader is told to run, and two links on one line.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fail=0

page() {
    printf -- '---\ntitle: probe\n---\n\n%s\n' "$1" > "$work/probe.md"
}

catches() {
    page "$2"
    printf '  %-42s ' "$1"
    if sh "$root/scripts/no-maintainer-notes.sh" "$work" > /dev/null 2>&1; then
        echo 'NOT CAUGHT'
        fail=1
    else
        echo 'caught'
    fi
}

allows() {
    page "$2"
    printf '  %-42s ' "$1"
    if sh "$root/scripts/no-maintainer-notes.sh" "$work" > /dev/null 2>&1; then
        echo 'allowed'
    else
        echo 'FALSE POSITIVE'
        fail=1
    fi
}

echo 'these must be caught'
catches 'a backticked design path'    'See `docs/design/fibers.md`.'
catches 'an errata number'            'Errata 35 says no struct crosses the C ABI.'
catches 'a roadmap number'            'Roadmap #142.'
catches 'a note about the note'       'The most negative Int prints, which took a second attempt.'
catches 'a path into crates/'         'The invariant is in `crates/khora-rt/src/fs.rs`.'
catches 'a nested link'               'See [the note](https://x/blob/main/[the note](https://x/docs/design/a.md)).'

echo 'and these must not be'
allows  'used to, as ordinary English' 'A schema, used to decode untrusted input.'
allows  'a real migration note'        '`Clock` used to live here and now does not.'
allows  'a script the reader runs'     'Run `sh scripts/setup-llvm.sh` after cloning.'
allows  'an ordinary link'             'See [the fibers note](https://x/blob/main/docs/design/fibers.md).'
allows  'two links on one line'        'See [a](https://x/a.md) and [b](https://y/b.md).'

echo
if [ "$fail" -eq 0 ]; then
    printf '  ok    the checker catches what it is for and nothing else\n'
else
    printf '  FAILED  see above\n'
fi
exit "$fail"
