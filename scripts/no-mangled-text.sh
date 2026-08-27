#!/bin/sh
# Source that a shell quoting mistake got into.
#
# Two failure modes, both of which have reached committed code in this
# repository and neither of which any compiler or test notices, because both
# produce a *valid* program with wrong text in it.
#
# **A control character.** `docs/errata.md` and `MEMORY.md` both record the
# heredoc version: an escape inside a shell-quoted string gets interpreted
# somewhere on the way, and a literal control byte ends up in a doc comment.
# `crates/khora-pkg/src/source.rs` had a backspace for about ten minutes, and
# `std/json.kh` had a unit separator sitting in published documentation --
# "Lowercase because `<0x1f>` is what every other encoder writes", where the
# text meant to name the escape sequence and instead contained the byte.
#
# **A tripled apostrophe.** The sibling mistake: putting an apostrophe inside a
# single-quoted shell string needs a four-character dance, and getting it one
# level wrong triples the quote instead. Three of those were committed before
# this script existed -- in `khora-manifest`, in `khora-rt` and in a design doc
# -- and all three survived review, clippy, 1,719 tests and a clean baseline,
# because there is nothing wrong with them except that they are not English.
#
#     sh scripts/no-mangled-text.sh
#
# Tracked files only, so a scratch file in the tree does not fail the gate.
#
# **A line that means it says so.** A lexer fuzz fixture holding three quotes
# is not a mistake, so a line carrying the marker below is skipped. Making the
# exemption visible at the site is the point: a blanket exclusion for `tests/`
# would have hidden two of the real ones, which were in a doc comment and a
# test helper.
#
# This script is excluded from its own scan, because a tool that searches for a
# string is a file containing that string.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

marker='mangled-text-ok'
myself='scripts/no-mangled-text.sh'
status=0

# A legal TOML multi-line literal string is three quotes, so `.toml` is out of
# this half and only this half. `.config/nextest.toml` uses one. Built rather
# than written so that this line does not match itself.
quotes=$(printf "'%s" "''")
tripled=$(git ls-files '*.rs' '*.md' '*.sh' '*.kh' \
    | grep -v "^$myself\$" \
    | xargs grep -n -- "$quotes" 2>/dev/null | grep -v "$marker" || true)
if [ -n "$tripled" ]; then
    printf '  FAILED  a tripled apostrophe, from a quoting slip:\n' >&2
    printf '%s\n' "$tripled" | sed 's|^|    |' >&2
    status=1
fi

# Everything a text file has no business containing. Tab, newline and carriage
# return are excluded; the rest of C0 is not.
control=$(git ls-files '*.rs' '*.md' '*.sh' '*.kh' '*.toml' \
    | grep -v "^$myself\$" \
    | xargs grep -nP '[\x00-\x08\x0b\x0c\x0e-\x1f]' 2>/dev/null \
    | grep -v "$marker" || true)
if [ -n "$control" ]; then
    printf '  FAILED  a control character in text:\n' >&2
    # The offending bytes are the whole problem, and printing them raw would do
    # to the terminal what they did to the file. Names and lines only.
    printf '%s\n' "$control" | cut -d: -f1,2 | sed 's|^|    |' >&2
    status=1
fi

if [ "$status" -eq 0 ]; then
    printf '  ok    no mangled text\n'
fi
exit "$status"
