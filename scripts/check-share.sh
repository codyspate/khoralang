#!/bin/sh
# A runtime entry that takes a Khora value, with nothing saying whether it
# publishes it.
#
# **What this prevents: a value reaching another fiber without its shared
# bit.** An object is local to the fiber that made it until a runtime entry
# makes it reachable elsewhere, and that entry has to mark it with
# `khora_share` first. Forgetting is silent today: counting is atomic, and
# the debug owner check only fires on a path a test runs. So every
# `extern "C" fn` in `khora-rt` that takes a Khora value -- a `*mut u8` or a
# `u64` word -- has to do one of two things somewhere between its doc
# comment and its closing brace:
#
#   call `khora_share`, `born_shared`, or one of `shared.rs`'s and
#   `fiber.rs`'s wrappers around it (`share_word`, `share_error`, `.share`),
#   outside a comment, or
#
#   carry `// SHARE: <why this does not publish>`: it only reads, it
#   releases, the value stays on the calling fiber, or the object is a
#   handle born shared.
#
# A new entry starts life failing this, which is the point: the question is
# asked when the entry is written, by whoever writes it.
#
# The test is lexical. A Khora value is a `*mut u8`, a `*mut c_void`, a `u64`
# or a `usize`. A `*const u8` is a byte buffer and does not count; a callback
# parameter is a function type and does not count; a `u64` or `usize` named
# exactly `len`, `length`, `size`, `capacity` or `previous`, or ending in
# `_len`, `_length`, `_size` or `_capacity`, is a number. A `*mut u8` that is
# a byte buffer or a Rust-side object does count, and says so in its note: the
# gate cannot tell, so it asks. Any `pub` visibility counts, at any
# indentation, so an entry in a nested module or a `pub(crate)` one with
# `#[no_mangle]` is seen. A mark named only inside a string literal is not a
# mark. Tests (`mod
# tests` and `#[cfg(test)]` items) are not entries. What it cannot tell is
# whether a note's reason is true; that is for review.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

report="${TMPDIR:-/tmp}/khora-share-gate"
: > "$report"

for file in $(find crates/khora-rt/src -name '*.rs' | sort); do
    awk -v file="$file" '
        # Stop at the tests: nothing below `mod tests` is an entry point.
        /^(pub(\(crate\))? )?mod tests/ { intests = 1 }
        intests { next }
        # A single test-only item is skipped too, up to its closing brace.
        prev ~ /^#\[cfg\(test\)\]/ && /^(pub[^ ]* )?(unsafe )?(extern "C" )?fn / { skipping = 1 }
        { prev = $0 }
        skipping { if ($0 ~ /^\}/) skipping = 0; next }
        # A doc comment or attribute starts an item; remember where.
        /^[ \t]*(\/\/\/|#\[)/ { if (!inhead) { headstart = NR; noted = 0 } ; inhead = 1 }
        /SHARE:/ { noted = 1 }
        /^[ \t]*pub(\([a-z]+\))? (unsafe )?extern "C" fn / {
            inhead = 0
            infn = 1; name = $0; at = NR; sig = ""; shared = noted
            ind = $0; sub(/[^ \t].*$/, "", ind)
        }
        infn {
            if (sig == "" || index(sig, ")") == 0) sig = sig $0
            code = $0; gsub(/"([^"\\]|\\.)*"/, "\"\"", code)
            if (code ~ /(khora_share|born_shared|share_word|share_error|\.share)\(/ && code !~ /^[ \t]*\/\//) shared = 1
            if ($0 ~ /SHARE:/) shared = 1
            if (substr($0, 1, length(ind) + 1) == ind "}") {
                infn = 0
                # Parameters only: from the "(" after `fn` to the first ")".
                # After `fn`, because `pub(crate)` has a "(" of its own.
                p = sig; sub(/^.*extern "C" fn [A-Za-z0-9_]*\(/, "", p); sub(/\).*$/, "", p)
                gsub(/Option<[^>]*>/, "", p)
                gsub(/extern "C" fn\([^)]*\)/, "", p)
                # A number, by its exact name or its suffix. A substring
                # would let `resize_target: u64` through.
                gsub(/(^|[ \t,])([a-z_]*_)?(len|length|size|capacity|previous): (u64|usize)/, " ", p)
                if ((p ~ /\*mut u8/ || p ~ /\*mut ([a-z_]+::)*c_void/ || p ~ /: (u64|usize)/) && !shared) {
                    fn = name; sub(/^.*fn /, "", fn); sub(/\(.*$/, "", fn)
                    printf "%s:%d: %s\n", file, at, fn
                }
                noted = 0
            }
            next
        }
        !/^[ \t]*(\/\/\/|#\[)/ { inhead = 0; if ($0 !~ /^[ \t]*$/ && $0 !~ /^[ \t]*\/\//) noted = 0 }
    ' "$file" >> "$report"
done

missing=$(wc -l < "$report" | tr -d ' ')
if [ "$missing" -gt 0 ]; then
    cat "$report" >&2
    printf '  FAILED  %d runtime entr(y/ies) take a Khora value and neither call khora_share nor say SHARE: why not\n' "$missing" >&2
    exit 1
fi
echo "  ok  every runtime entry that takes a Khora value marks it or says why not"
