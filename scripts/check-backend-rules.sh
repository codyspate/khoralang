#!/bin/sh
# Every way the code generator can refuse a program is a decision somebody made.
#
# **A rule the type checker does not know is a rule the editor cannot show.**
# `khora-lsp` publishes what the parser, the checker and the lints say, and
# nothing else — so a rule enforced while lowering is one a developer meets at
# `khora build`, after the loop they were working in called the program fine.
# That is the furthest place from the line that broke, and the hardest to act
# on.
#
# Two such rules were found this way and moved into `khora-types`: raising a
# type no declaration names, and an integer literal wider than an `Int`. Both
# had type-checked cleanly and been refused at build time, in silence as far as
# any editor was concerned.
#
# # What this gate does, and what it deliberately does not
#
# It cannot decide whether a message is a language rule or an assertion about
# the lowering — that reading is the whole question. What it can do is make the
# set of them a thing somebody chose rather than a thing that grew: the
# messages are recorded in `backend-rules.txt`, and a message that is not on
# the list fails the build.
#
# Adding one is a line in that file, and the point of the line is the moment
# before it, when the question gets asked: **can a program that passes
# `khora check` reach this?** If it can, the rule belongs in `khora-types`
# where a person can see it, and the backend keeps its copy as an assertion.
# If it cannot — "is not an operation the backend knows", "which is a compiler
# bug" — it is a statement about the compiler and belongs exactly where it is.
#
# The same shape as `no-bare-unsafe.sh`: a script cannot tell whether an
# argument is *sound*, only whether somebody wrote one.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

manifest=scripts/backend-rules.txt
lowering='crates/khora-codegen-llvm/src/lower'

if [ ! -f "$manifest" ]; then
    echo "  no $manifest; nothing records which backend refusals are known" >&2
    exit 1
fi

found="${TMPDIR:-/tmp}/khora-backend-rules-found"
grep -rhoE '\.fail\(format!\("[^"]+"|\.fail\("[^"]+"' "$lowering"/*.rs \
    | sed 's/\.fail(format!(//; s/\.fail(//' \
    | sed 's/^"//; s/"$//' \
    | sort -u > "$found"

# Comments and blank lines are the manifest's own prose.
known="${TMPDIR:-/tmp}/khora-backend-rules-known"
grep -vE '^\s*(#|$)' "$manifest" | sort -u > "$known"

added=$(comm -23 "$found" "$known")
gone=$(comm -13 "$found" "$known")

status=0

if [ -n "$added" ]; then
    echo "  a way to refuse a program that nothing has classified:" >&2
    echo "$added" | sed 's/^/    /' >&2
    echo "" >&2
    echo "  Before adding it to $manifest, answer the question it is there for:" >&2
    echo "  can a program that passes \`khora check\` reach this message?" >&2
    echo "" >&2
    echo "  If it can, the rule belongs in \`khora-types\`, where the editor" >&2
    echo "  can show it — the backend then keeps this as an assertion about" >&2
    echo "  what reaches it. If it cannot, it is a statement about the" >&2
    echo "  compiler and belongs where it is." >&2
    status=1
fi

if [ -n "$gone" ]; then
    echo "  $manifest lists a message the lowering no longer has:" >&2
    echo "$gone" | sed 's/^/    /' >&2
    echo "" >&2
    echo "  A list that outlives what it describes stops being read. Delete" >&2
    echo "  the line." >&2
    status=1
fi

if [ "$status" -eq 0 ]; then
    echo "  $(wc -l < "$found" | tr -d ' ') backend refusal(s), every one classified"
fi

exit "$status"
