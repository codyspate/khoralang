#!/bin/sh
# An exported item you cannot call from its signature has an example.
#
# **Not every item, and the rule is the interesting part.** `std` documents
# around 1,100 items. Demanding an example on each would mean writing one for
# `unix_millis: () -> Int`, whose two sentences of prose already say everything
# there is, and a rule that produces a thousand pieces of ceremony is a rule
# people route around.
#
# So the line is drawn where a reader is most reliably stuck: **an item that
# requires a capability.** A signature saying `with { db: Db }` tells you a
# capability is needed and nothing about which handler to install, where to
# install it, or what the block around the call looks like. That is the one
# question `std`'s own shape makes hard to answer from a signature, and it is
# the question a reader has before any other.
#
# **Higher-order functions are deliberately not in this rule**, and the first
# draft had them. `(A) -> B` in a parameter position also needs something
# written before the call, and `List::map` deserves an example as much as
# anything here does -- but the rule caught 1,057 items, which is most of the
# library, and a rule that flags most of the library is one nobody can finish
# and everybody turns off. Those are worth doing and are not this.
#
# # The first fence is the signature
#
# `khora doc` emits each item's signature as the first ```khora block under its
# heading. An item has an *example* when it has a second one. That coupling is
# why this reads the generated pages rather than the sources: the generator has
# already done the work of deciding what an item is.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

pages=website/content/docs/stdlib/api
if [ ! -d "$pages" ]; then
    echo "  no generated API pages; run \`khora doc\` first" >&2
    exit 1
fi

report="${TMPDIR:-/tmp}/khora-api-examples"
: > "$report"

find "$pages" -name '*.md' | sort | while read -r page; do
    awk -v page="$page" -v report="$report" '
        # **A heading is an item only when its signature follows it.** A `///`
        # block may contain its own `# Sections`, and the generator renders
        # those at the same level as an item name -- so treating every heading
        # as an item both invents items ("Why the body carries a row it does
        # not seem to need") and splits real ones, which hides an example
        # written after the first prose heading. What tells them apart is that
        # the generator always emits an item signature as the first thing under
        # an item heading.
        /^#{3,4} / {
            pending = $0
            sub(/^#+ +/, "", pending)
            expecting = 1
            next
        }
        # The first non-empty line after a heading decides what it was.
        expecting && /[^ \t]/ {
            expecting = 0
            if ($0 == "```khora") {
                settle()
                heading = pending
                fences = 1
                hard = 0
                in_sig = 1
                next
            }
            # Prose: the heading was a section of the current item, and the
            # line falls through to the ordinary handling below.
        }
        /^```khora$/ { fences += 1; next }
        /^```$/ { in_sig = 0; next }
        in_sig {
            if ($0 ~ /with \{/) hard = 1
        }
        END { settle() }

        function settle() {
            if (heading != "" && hard && fences < 2) {
                printf "%s\t%s\n", page, heading >> report
            }
        }
    ' "$page"
done

missing=$(wc -l < "$report" | tr -d ' ')

if [ "$missing" != "0" ]; then
    echo "  $missing exported item(s) require a capability and have no example:" >&2
    sed 's|website/content/docs/stdlib/api/||; s|\t|  ::  |' "$report" | sed 's/^/    /' >&2
    echo "" >&2
    echo "  Write one in the item's \`///\` block in \`std\`, then re-run" >&2
    echo "  \`khora doc std --out website/content/docs/stdlib/api\`." >&2
    exit 1
fi

echo "  every item requiring a capability has an example"
