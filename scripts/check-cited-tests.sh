#!/bin/sh
# Every test a design document names as protecting something actually exists.
#
# **`docs/design/soundness.md` names the test that protects each invariant it
# records**, which is the half of the release gate's "name the invariant and
# the test or argument that protects it" that decays. A test gets renamed, or
# rewritten, or deleted because something replaced it, and the document goes on
# citing it -- so an inventory entry that reads like a guarantee is a sentence
# pointing at nothing.
#
# There is a live example. `soundness.md` cited
# `a_fiber_keeps_its_identity_across_workers` as the protection for
# thread-affinity under fiber migration. The test existed, so a check for the
# name alone would have passed -- and it never asserted that a fiber changed
# worker, so on a run where nothing migrated it proved nothing. This script
# would not have caught that. What it does catch is the next stage of the same
# decay, which is the citation outliving the test.
#
# A stronger check -- that the test still asserts what the document says it
# asserts -- is not something a script can do, and pretending otherwise would
# be worse than this. The document says which test; this says the test is
# there; a reader does the rest.
#
# # What counts as a citation
#
# A backtick-quoted `snake_case` name of at least three words. Three because
# that is the shape of a test name in this repository and not the shape of an
# ordinary identifier: `khora_array_new` is a function, `counts_non_atomically`
# is a function, and neither is being claimed as a test. Names that resolve to
# any `fn` in `crates/` are accepted, which keeps the check about existence
# rather than about a naming convention nobody agreed to.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

papers="docs/design/soundness.md docs/design/scheduler.md"
missing=0
checked=0

for paper in $papers; do
    [ -f "$paper" ] || continue

    # Backticked snake_case names of three or more words.
    names=$(grep -oE '`[a-z][a-z0-9]*(_[a-z0-9]+){2,}`' "$paper" | tr -d '`' | sort -u)

    for name in $names; do
        checked=$((checked + 1))
        # Any mention, not just a definition. A cited *test* that was deleted
        # or renamed leaves no trace at all, which is the decay this is for; a
        # cited foreign API appears as a call. Demanding a definition reports
        # the second as loudly as the first, which is how `build_in_bounds_gep`
        # -- inkwell's, not ours -- became this check's first false positive.
        if ! grep -rqE "\b$name\b" crates/ 2>/dev/null; then
            echo "  $paper cites \`$name\`, and it appears nowhere in crates/" >&2
            missing=$((missing + 1))
        fi
    done
done

if [ "$missing" -gt 0 ]; then
    echo "" >&2
    echo "  A design document names a test that is not there. Either the test was" >&2
    echo "  renamed or removed and the document was not, or the name is a typo." >&2
    echo "  Both leave an invariant recorded as protected by nothing." >&2
    exit 1
fi

echo "  $checked cited name(s), all present"
