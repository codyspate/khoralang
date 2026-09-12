#!/bin/sh
# Type-checks the whole-program examples on the generated API pages.
#
# **The gap this closes.** `check-docs.sh` skips `stdlib/api/*` on the grounds
# that `khora doc --check` owns those pages, and it does -- but only in the
# sense that it verifies the page matches the `///` comment it came from. A
# comment and a page can agree perfectly about an example that does not
# compile, and one did: `std::fs`'s `read_text` example used `IoError` without
# importing it, was published on the website, and was copied verbatim by
# somebody following the documentation. It cost them a compile error on their
# first read of the page.
#
# **Only blocks declaring their own `module`.** An API page is mostly
# signatures -- `read: (String) -> Array<U8> raises IoError` is an effect
# operation as `khora doc` renders it, not a program, and 12 of `fs.md`'s 38
# blocks are that shape. Checking everything would report those as failures
# forever, and a gate that cries wolf gets switched off. A block that says
# `module` is making a claim to be a complete program, and this holds it to it.
#
# That also means the way to get an example covered is to write it as a whole
# module, which is the same rule `check-docs.sh` states and the same reason:
# an example a reader can copy and run is worth more than a fragment.
set -eu

khora=${KHORA:-khora}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

pages=$(find website/content/docs/stdlib/api -name '*.md' | sort)

total=0
bad=0

for page in $pages; do
    # Split the page into fenced `khora` blocks, one file each, keeping only
    # those whose first line declares a module.
    awk -v dir="$work" -v page="$page" '
        /^```khora$/ { inside = 1; n++; first = ""; body = ""; next }
        /^```$/ && inside {
            inside = 0
            if (first ~ /^module /) {
                name = dir "/" n ".kh"
                printf "%s", body > name
                close(name)
                printf "%s %s %d\n", name, page, start
            }
            next
        }
        inside {
            if (first == "") { first = $0; start = NR }
            body = body $0 "\n"
        }
    ' "$page" >> "$work/index"
done

[ -s "$work/index" ] || { printf '  no whole-program examples on the API pages\n'; exit 0; }

while read -r block page line; do
    total=$((total + 1))
    if ! "$khora" check "$block" > "$work/out" 2>&1; then
        printf '  FAILED  %s:%s does not check\n' "$page" "$line" >&2
        grep '^error' "$work/out" | head -3 >&2
        bad=$((bad + 1))
    fi
done < "$work/index"

if [ "$bad" -gt 0 ]; then
    printf '  FAILED  %d of %d API example(s)\n' "$bad" "$total" >&2
    exit 1
fi

printf '  ok    %d whole-program API example(s) check\n' "$total"
