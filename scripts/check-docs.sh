#!/bin/sh
# Every hand-written Khora example in the documentation, against this compiler.
#
# **The generated pages were the only ones anything checked.** `khora doc std
# --check` fails when a page under `stdlib/api` drifts from the `///` comments
# it came from, so 993 of the 1,565 examples on the site are kept honest. The
# other 572 are the ones somebody wrote by hand in the Guide, the Reference and
# the Cookbook — the pages a person actually reads first — and nothing had ever
# compiled one.
#
# Two levels, because most blocks are fragments:
#
#   *every* block is parsed, wrapped in a module if it does not declare one.
#   This catches syntax that no longer exists, which is the way documentation
#   rots fastest.
#
#   a block that declares its own `module` is a whole program and is *checked*:
#   names resolved, types inferred, rows accounted for. Three of these were
#   stale when this script was written, all from a `raises` clause that had
#   grown a `ChildFailed` earlier the same day.
#
# A fragment is not type-checked. It refers to names it never declares by
# design — a signature, three lines of a `match` — and wrapping it in enough
# scaffolding to check would mean inventing the surroundings, which is a second
# document to keep true.
#
# **Not in `scripts/baseline.sh` yet, and that is deliberate.** 59 of the 572
# blocks parse in none of the four shapes below. Every one inspected so far is
# a bodyless signature -- `fn transfer(..) -> Result<(), DbError> with { db: Db }`
# with neither a body nor a `;`, which the Reference and the Cookbook both use
# to show a shape without repeating what it does -- or a fragment of an
# argument list. Those are legitimate documentation and the checker has to
# learn them, rather than the documentation being bent to the checker.
#
# A gate step that passes with 59 known failures is a gate step that is lying,
# so this runs by hand until the count is zero. Roadmap #159.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

khora="./target/debug/khora.exe"
[ -x "$khora" ] || khora="./target/debug/khora"

work="${TMPDIR:-/tmp}/khora-doc-blocks"
rm -rf "$work"
mkdir -p "$work"

# The prose. `stdlib/api` is generated and `khora doc --check` owns it.
pages=$(find website/content/docs -name '*.md' -not -path '*/stdlib/api/*' | sort)

# One file per block. `whole` in the name when the block declared a module, so
# the loop below knows which to check as well as parse.
# **No `gensub`.** It is gawk's, and this runs under whatever `awk` the machine
# has -- mawk on Debian, BSD awk on macOS. The block's number comes from the
# shell instead, and the page it came from is recorded beside it, so a failure
# can name the page without the file name having to encode it.
page_no=0
for page in $pages; do
    page_no=$((page_no + 1))
    awk -v page="$page" -v work="$work" -v seq="$page_no" '
        /^```khora$/ { inside = 1; n += 1; body = ""; first = ""; line = NR; next }
        /^```$/ && inside {
            inside = 0
            whole = (first ~ /^module /) ? "whole" : "part"
            file = sprintf("%s/%04d.%03d.%s.kh", work, seq, n, whole)
            if (whole == "part") printf "module doc;\n\n" > file
            printf "%s", body > file
            close(file)
            printf "%s\t%s:%d\n", file, page, line >> (work "/index")
            # The same fragment as the body of a function. Most blocks are an
            # expression or a statement -- `nursery.adopt(..)`, `let x = ..` --
            # which is not a declaration and cannot sit at module level.
            if (whole == "part") {
                inner = sprintf("%s/%04d.%03d.inner.kh", work, seq, n)
                printf "module doc;\n\nfn example() {\n%s}\n", body > inner
                close(inner)
                # And as a type. The Reference shows a bare one all the time --
                # `forall<A>. A -> A`, an error row, a function type -- and it
                # is neither a declaration nor a statement.
                astype = sprintf("%s/%04d.%03d.astype.kh", work, seq, n)
                printf "module doc;\n\ntype Example = %s;\n", body > astype
                close(astype)
            }
            next
        }
        inside {
            if (first == "" && $0 ~ /[^ \t]/) first = $0
            body = body $0 "\n"
        }
    ' "$page"
done

# Where a block came from, for a message somebody can act on.
where() {
    awk -F'\t' -v want="$1" '$1 == want { print $2; exit }' "$work/index"
}

# A block that is a *list*, one lexeme per line: the identifiers a name may
# have, the shapes a literal comes in, the patterns that match one case each.
# The Reference is full of them and none is a program.
#
# **Only for a block with no braces**, which is what keeps this from excusing a
# stale multi-line example whose lines happen to parse one at a time. A list of
# lexemes has no structure to get wrong; anything with a block in it does.
lexemes() {
    # **A brace that opens a block, not any brace.** A character literal such
    # as the unicode escape has braces in it and no structure at all, and
    # rejecting every brace turned the escape table into a failure. What this
    # guards against is a multi-line *program* whose lines happen to parse one
    # at a time, and one of those has a line ending in `{`.
    if grep -qE '\{[[:space:]]*$|^[[:space:]]*\}' "$1"; then
        return 1
    fi
    one="$work/one.kh"
    sed '1,2d' "$1" | while IFS= read -r line; do
        [ -n "$(printf '%s' "$line" | tr -d ' \t')" ] || continue
        printf 'module doc;\n\nfn example() {\n  %s\n}\n' "$line" > "$one"
        "$khora" parse "$one" > /dev/null 2>&1 || exit 1
    done
}

# A fragment passes if it parses *either* way. Which of the two it is
# depends on what the block is illustrating, and the documentation should not
# have to say — that would be a second thing to keep true.
parses() {
    if "$khora" parse "$1" > "$work/out" 2>&1; then
        return 0
    fi
    case "$1" in
    *.part.kh)
        inner=$(printf '%s' "$1" | sed 's/\.part\.kh$/.inner.kh/')
        if [ -e "$inner" ] && "$khora" parse "$inner" > "$work/out" 2>&1; then
            return 0
        fi
        astype=$(printf '%s' "$1" | sed 's/\.part\.kh$/.astype.kh/')
        if [ -e "$astype" ] && "$khora" parse "$astype" > "$work/out" 2>&1; then
            return 0
        fi
        lexemes "$1"
        ;;
    *) return 1 ;;
    esac
}

total=0
parsed=0
checked=0
bad=0
for block in "$work"/*.kh; do
    [ -e "$block" ] || continue
    case "$block" in *.inner.kh | *.astype.kh) continue ;; esac
    total=$((total + 1))
    if ! parses "$block"; then
        printf '  FAILED  %s does not parse\n' "$(where "$block")" >&2
        grep '^error' "$work/out" | head -2 >&2
        bad=$((bad + 1))
        continue
    fi
    parsed=$((parsed + 1))
    case "$block" in
    *.whole.kh)
        if ! "$khora" check "$block" > "$work/out" 2>&1; then
            printf '  FAILED  %s does not check\n' "$(where "$block")" >&2
            grep '^error' "$work/out" | head -3 >&2
            bad=$((bad + 1))
            continue
        fi
        checked=$((checked + 1))
        ;;
    esac
done

if [ "$bad" -gt 0 ]; then
    printf '  FAILED  %d of %d documentation example(s)\n' "$bad" "$total" >&2
    printf '  The blocks are kept at %s\n' "$work" >&2
    exit 1
fi

printf '  ok    %d example(s) parse, %d of them check as whole programs\n' "$parsed" "$checked"
