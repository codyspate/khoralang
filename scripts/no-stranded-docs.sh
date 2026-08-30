#!/bin/sh
# A doc comment that describes the item under it, and not some other one.
#
# **Twenty-seven doc comments in this tree were on the wrong item**, eleven of
# them describing something else entirely. `std/core.kh` alone had seven, and
# one of those had been wrong for four commits. The shape is always the same:
# something new is inserted *between* an existing doc comment and the item it
# was written for, so the old comment silently becomes the top of the
# newcomer's documentation and the original item is left bare.
#
# Nothing caught it. `khora-doc`'s `std_surface` test checks that every public
# item *has* a `///`, and after an edit like that every one of them still does
# -- the comment was not deleted, only re-parented, so the count never moves.
# `cargo doc` renders the result without complaint, because what it is handed
# is a perfectly well-formed comment. It reaches a reader as one paragraph that
# changes subject halfway through, which reads as bad writing rather than as a
# bug, and the item that lost its documentation is silently undocumented.
#
# **What it looks for.** Doc prose here wraps at column 79, so a line that ends
# a sentence *short* of that column ended it deliberately: the next word would
# have fitted and the writer broke anyway, which means a paragraph ended. A
# paragraph boundary inside one doc comment is written with a bare `///`
# between the halves. Find a deliberate break with no bare `///` after it and
# you have found either a missing blank line or two doc comments that have been
# run together -- and the second is this bug.
#
# The column is measured rather than chosen. Every one of the nine hits that
# sat at *exactly* 79 turned out to be ordinary wrapping, and every real
# stranding had room to spare, so the test is "would the next word have fitted
# with the line still under 79".
#
#     sh scripts/no-stranded-docs.sh [file...]
#
# Tracked `.rs` and `.kh` files by default.
#
# **A line that means it.** A break that is deliberate and is not a paragraph
# is exempted by putting the marker below on the line before the doc comment.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

marker='stranded-docs-ok'
myself='scripts/no-stranded-docs.sh'

if [ "$#" -gt 0 ]; then
    files=$*
else
    files=$(git ls-files '*.rs' '*.kh' | grep -v "^$myself\$")
fi

# The list becomes the argument list rather than going through `xargs`, which
# reads a backslash as an escape -- and an absolute path on Windows is mostly
# backslashes, so `xargs` handed `awk` a filename that did not exist and this
# passed by finding nothing. Paths in this tree have no spaces in them.
set --
for file in $files; do
    set -- "$@" "$file"
done
[ "$#" -gt 0 ] || exit 0

found=$(awk -v marker="$marker" '
    # The wrap column. A break is deliberate when the next word would have left
    # the line shorter than this.
    BEGIN { width = 79 }

    function bare(s) { sub(/^[ \t]+/, "", s); sub(/[ \t]+$/, "", s); return s }

    # Anything that is not a doc line ends the block, and with it any fence.
    !/^[ \t]*\/\/\// { prev = ""; fence = 0; skip = ($0 ~ marker); next }

    {
        body = $0
        sub(/^[ \t]*\/\/\//, "", body)
        text = bare(body)

        # Inside a fenced example neither the wrap column nor the shape of the
        # prose means anything.
        if (text ~ /^```/) { fence = 1 - fence; prev = ""; next }
        if (fence) { prev = ""; next }

        # A bare `///` is the separator this looks for the absence of.
        if (text == "") { prev = ""; next }

        # Indented content is a nested list or an example, not wrapped prose.
        if (body ~ /^   /) { prev = ""; next }

        if (prev != "" && !skip) {
            # A sentence that ends, then one that starts. A line ending in `**`
            # closes a bolded lead-in and the paragraph carries on past it; a
            # line opening with a bullet is a list rather than a paragraph, so
            # neither is a break worth reporting.
            if (prev ~ /[.!?][)\]*_`"]*$/ && prev !~ /\*\*$/ && text ~ /^(\*\*|[A-Z`[])/) {
                word = text
                sub(/ .*$/, "", word)
                if (prevlen + 1 + length(word) < width) {
                    printf "%s:%d: a paragraph ends here with no `///` after it\n", FILENAME, NR - 1
                    printf "    ...%s\n", substr(prev, length(prev) - 55)
                    printf "    %s...\n", substr(text, 1, 56)
                }
            }
        }
        prev = text
        prevlen = length($0)
    }
' "$@" || true)

if [ -n "$found" ]; then
    printf '  FAILED  a doc comment may be on the wrong item:\n' >&2
    printf '%s\n' "$found" | sed 's|^|    |' >&2
    printf '\n' >&2
    printf '  Either the blank `///` between two paragraphs is missing, or two\n' >&2
    printf '  doc comments have been run together and the first one belongs to\n' >&2
    printf '  an item that no longer follows it.\n' >&2
    exit 1
fi

printf '  ok    every doc comment reaches its item\n'
