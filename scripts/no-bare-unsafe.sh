#!/bin/sh
# An `unsafe` block with nothing saying why it is sound.
#
# **`docs/design/soundness.md` divides this repository's `unsafe` into "enforced"
# and "believed", and this is what moves a line between the two.** The audit
# that wrote that document annotated the blocks by hand and recorded that 28 had
# no note. The number was 41 when it was next counted, because nothing was
# checking and every new block started life in the second column.
#
# A block is covered two ways, and the second is why this is not just `grep`:
#
#   **A note within eight lines above it.** The ordinary case. Eight reaches a
#   paragraph above the block and not the previous function, and it lets two
#   blocks share one argument — `region.rs` computes an address and then reads
#   it, which is one obligation and one note.
#
#   **A blanket note, spelled `SAFETY, for`.** It covers every block after it in
#   the file. `channel.rs`'s tests open a handle, use it and release it inside
#   one function, twenty-three times; the argument is identical every time and
#   writing it out twenty-three times is how the load-bearing note stops being
#   read. The distinct wording is deliberate: a reader typing `SAFETY, for` is
#   making a claim about a *run* of blocks and should know it.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

report="${TMPDIR:-/tmp}/khora-bare-unsafe"
: > "$report"

for file in $(find crates -name '*.rs' | sort); do
    awk -v file="$file" '
        { line[NR] = $0 }
        # A blanket note applies from here to the end of the file.
        /SAFETY, for/ { blanket = 1 }
        /(^|[^A-Za-z_])unsafe[ \t]*\{/ {
            total += 1
            if (blanket) next
            # Upward to the start of the enclosing item and no further. A
            # fixed window of N lines reaches the note on the *previous*
            # function when a block sits near the top of a short one, which
            # is a false pass and was one: a bare `unsafe { *p }` appended
            # to `heap.rs` came out covered by the function above it.
            for (i = NR - 1; i >= 1; i -= 1) {
                if (index(line[i], "SAFETY") > 0) next
                if (line[i] ~ /^[ \t]*(pub[^f]*)?(unsafe[ \t]+)?(extern[ \t]+[^ \t]+[ \t]+)?fn[ \t]/) break
                if (line[i] ~ /^\}/) break
            }
            printf "%s:%d: %s\n", file, NR, $0
        }
        END { printf "#%d\n", total }
    ' "$file" >> "$report"
done

blocks=$(grep -c '^#' "$report" > /dev/null 2>&1; awk -F'#' '/^#/ { n += $2 } END { print n + 0 }' "$report")
bare=$(grep -cv '^#' "$report" || true)

if [ "$bare" -gt 0 ]; then
    grep -v '^#' "$report" >&2
    printf '  FAILED  %d of %d unsafe block(s) say nothing about why they are sound\n' \
        "$bare" "$blocks" >&2
    printf '  Write a `// SAFETY:` note naming the invariant, or a blanket\n' >&2
    printf '  `// SAFETY, for ..` above a run of calls that share one.\n' >&2
    exit 1
fi

printf '  ok    %s unsafe block(s), every one with an argument\n' "$blocks"
