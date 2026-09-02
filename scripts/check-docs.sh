#!/bin/sh
# Every hand-written Khora example in the documentation, against this compiler.
#
# **The generated pages were the only ones anything checked.** `khora doc std
# --check` fails when a page under `stdlib/api` drifts from the `///` comments
# it came from, so 993 of the 1,565 examples on the site are kept honest. The
# other 580 are the ones somebody wrote by hand in the Guide, the Reference and
# the Cookbook — the pages a person actually reads first — and nothing had ever
# compiled one.
#
# Two levels, because most blocks are fragments:
#
#   *every* block is parsed. This catches syntax that no longer exists, which
#   is the way documentation rots fastest.
#
#   a block that declares its own `module` is a whole program and is *checked*:
#   names resolved, types inferred, rows accounted for.
#
# A fragment is not type-checked. It refers to names it never declares by
# design — a signature, three lines of a `match` — and wrapping it in enough
# scaffolding to check would mean inventing the surroundings, which is a second
# document to keep true.
#
# # The wrappings, and why guessing is safe here
#
# A fragment is not a program and cannot be parsed as one, so it is tried in
# each of the shapes documentation actually uses — see `wrapped` below. The
# obvious worry is that a *stale* example passes by accidentally fitting one of
# them, and the answer is that every wrapping adds scaffolding a real program
# would already have:
#
#   - a bodyless signature gets `{}`, which a program with a body cannot take;
#   - a row or a parameter list is put where only a row or a parameter list can
#     go;
#   - match arms are put inside a `match`, which needs `=>` in every arm;
#   - a *list* is accepted only when every chunk of it is one of these, and is
#     tried last — a block whose lines are all statements has already parsed as
#     a whole under `inner` and never reaches it.
#
# The shape that would be unsafe is a general "split it anywhere and try both
# halves", which would excuse almost anything. Blocks that need it — a type
# declaration followed by a bare `let` — are written as whole modules in the
# documentation instead, which also means a reader can copy one and run it.
#
# # What it does not catch
#
# A fragment is parsed, not checked, so a line that is *syntactically* fine and
# means the wrong thing passes. `List<String` is the sharp case: with the
# closing bracket missing it is still a valid comparison expression, and no
# amount of wrapping makes a parser say otherwise. Only the blocks that declare
# their own `module` are checked, and only those catch a name that no longer
# exists or an operation that was added.
#
# The way to make an example carry its weight is therefore to write it as a
# whole module. 17 of the 580 are, and they are the ones that caught something.
#
# # What it caught on the way to zero
#
# 55 of 580 blocks failed when the wrappings above were written. Most were
# legitimate documentation the checker had to learn. Six were not:
#
#   - a whole `handler for Db` missing `broken`, stale since the operation was
#     added two commits earlier — which is exactly the rot this exists to find;
#   - two calls written `String::with_c_string<B, 'ef, 'er>(..)`, passing row
#     variables at a call site, which the parser does not accept and which
#     nobody would write;
#   - two bodies elided as `{ .. }`, which is not Khora;
#   - a signature shown with a body it did not need.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

khora="./target/debug/khora.exe"
[ -x "$khora" ] || khora="./target/debug/khora"

work="${TMPDIR:-/tmp}/khora-doc-blocks"
rm -rf "$work"
mkdir -p "$work"

# The prose. `stdlib/api` is generated and `khora doc --check` owns it.
#
# Named pages are accepted so that one can be checked while it is being
# written -- the whole tree is around two hundred `khora parse` calls and takes
# a couple of minutes, which is right for a gate step and wrong for a loop
# somebody is sitting in front of.
if [ "$#" -gt 0 ]; then
    pages="$*"
else
    pages=$(find website/content/docs -name '*.md' -not -path '*/stdlib/api/*' | sort)
fi

# One file per block, twice: `.raw` is the body exactly as written, and
# `.part`/`.whole` is what gets parsed first. `whole` in the name when the
# block declared a module, so the loop below knows which to check as well.
#
# **No `gensub`.** It is gawk's, and this runs under whatever `awk` the machine
# has — mawk on Debian, BSD awk on macOS. The block's number comes from the
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
            raw = sprintf("%s/%04d.%03d.raw", work, seq, n)
            printf "%s", body > raw
            close(raw)
            printf "%s\t%s:%d\n", file, page, line >> (work "/index")
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

candidate="$work/candidate.kh"

# `$2` around the body in `$1`, parsed. `$2` names one of the shapes below.
#
# **The body is a string, not a file.** It used to be read with `cat` inside
# each wrapping, which is a process; ten shapes over 580 blocks is five
# thousand of them, and on Windows a process is about fifteen milliseconds. The
# body is read once per block by the caller and `printf` is a builtin, so what
# is left is one `khora parse` per attempt — which is the cost that cannot be
# avoided and is what the running time should be made of.
wrapped() {
    body="$1"
    case "$2" in
    # A statement or an expression: `nursery.adopt(..)`, `let x = ..`. Most
    # blocks are one of these, and neither can sit at module level.
    inner) { printf 'module doc;\n\nfn example() -> () {\n'; printf '%s\n' "$body"; printf '}\n'; } > "$candidate" ;;
    # A type, which the Reference shows bare all the time.
    astype) { printf 'module doc;\n\ntype Example = '; printf '%s\n' "$body"; printf ';\n'; } > "$candidate" ;;
    # A signature with no body, which is how a page shows a shape without
    # repeating what it does. `{}` is a body a real program would already have.
    signature) { printf 'module doc;\n\n'; printf '%s\n' "$body"; printf '\n{}\n'; } > "$candidate" ;;
    # A generic parameter list: `<A: Eq + Show>`, `<const N: Int>`, `<+A>`.
    params) { printf 'module doc;\n\ntype Example'; printf '%s\n' "$body"; printf ' = Int;\n'; } > "$candidate" ;;
    # A row as it is written on a signature: `raises StoreError`, `with { .. }`.
    row) { printf 'module doc;\n\nfn example() -> () '; printf '%s\n' "$body"; printf ' {}\n'; } > "$candidate" ;;
    # The arms of a `match`, which need a `=>` each and so cannot be anything
    # else.
    arms) { printf 'module doc;\n\nfn example() -> () {\n  match subject {\n'; printf '%s\n' "$body"; printf '  }\n}\n'; } > "$candidate" ;;
    # One pattern, which is the left of an arm.
    pattern) { printf 'module doc;\n\nfn example() -> () {\n  match subject {\n'; printf '%s\n' "$body"; printf ' => (),\n  }\n}\n'; } > "$candidate" ;;
    # A fragment of an argument list, trailing comma and all.
    arguments) { printf 'module doc;\n\nfn example() -> () {\n  call(\n'; printf '%s\n' "$body"; printf '  )\n}\n'; } > "$candidate" ;;
    # A declaration, which is what a one-line `type` or `extern fn` is. The
    # base case for a whole block; a chunk of a list needs it too.
    declaration) { printf 'module doc;\n\n'; printf '%s\n' "$body"; printf '\n'; } > "$candidate" ;;
    # What only a trait can hold: an associated type, with or without a bound.
    associated) { printf 'module doc;\n\ntrait Example {\n'; printf '%s\n' "$body"; printf '}\n'; } > "$candidate" ;;
    # One entry of a record or a handler, trailing comma and all — which is how
    # a page shows a single operation without repeating the other four.
    field) { printf 'module doc;\n\nfn example() -> () {\n  let record = {\n'; printf '%s\n' "$body"; printf '  };\n}\n'; } > "$candidate" ;;
    esac
    "$khora" parse "$candidate" > "$work/out" 2>&1
}

# A block that is a *list*: one type per line, the constructs of the language
# one after another, two signatures with a blank line between them. The
# Reference is full of these and none is a program.
#
# Lines are accumulated until what has been accumulated parses in some shape,
# and then the next chunk starts — so an entry may span lines, which a
# multi-line array literal and a wrapped signature both do.
#
# **This cannot excuse a stale program**, and the reason is that it is reached
# last. A block whose lines are all statements parses as a whole under `inner`
# and never gets here; what gets here is a block that is not one thing, and
# asking whether it is several is then the honest question. The guard this
# replaced — refusing any block with a line that opened a block — was a proxy
# for the same argument, and it refused an `effect` declaration standing beside
# two signatures, which is exactly what a list of declarations looks like.
listed() {
    chunk=""
    first=""
    open=0
    while IFS= read -r line; do
        # A blank line between chunks is a separator; inside one it is part of
        # the chunk. The test is a pattern rather than `tr`, which would be a
        # process per line of every list in the documentation.
        case "$line" in
        *[![:space:]]*) ;;
        *) [ "$open" -eq 0 ] && continue ;;
        esac

        [ "$open" -eq 1 ] || first="$line"
        chunk="$chunk$line
"
        open=1

        shapes="declaration inner astype signature params row arms pattern arguments associated field"
        # **A chunk of several lines is only tried as a statement when its
        # first line looks continued**, which means it ended with an opener or
        # a comma. Without that,
        #
        #     List<String
        #     Map<String, User>
        #
        # — a type list with the first entry's bracket missing — was accepted,
        # because the two lines together parse as a chain of comparisons. The
        # case that needs multi-line statements is an array literal, and its
        # first line ends with `[`.
        #
        # Nothing legitimate is lost: a block whose lines are all statements
        # has already parsed as a whole under `inner` and never reaches here.
        if [ "$chunk" != "$first
" ]; then
            case "$first" in
            *"[" | *"(" | *"{" | *",") ;;
            *) shapes="declaration astype signature params row arms pattern arguments associated field" ;;
            esac
        fi

        for shape in $shapes; do
            if wrapped "$chunk" "$shape"; then
                chunk=""
                first=""
                open=0
                break
            fi
        done
    done < "$1"
    # Anything left over is a chunk that never became something.
    [ "$open" -eq 0 ]
}

# A fragment passes if it parses in *any* of the shapes documentation uses.
# Which one it is depends on what the block is illustrating, and the
# documentation should not have to say — that would be a second thing to keep
# true.
parses() {
    if "$khora" parse "$1" > "$work/out" 2>&1; then
        return 0
    fi
    case "$1" in
    *.part.kh) ;;
    *) return 1 ;;
    esac
    raw=${1%.part.kh}.raw
    [ -e "$raw" ] || return 1
    # Once, for every shape below. See `wrapped`.
    body=$(cat "$raw")
    for shape in inner astype signature params row arms pattern arguments associated field; do
        if wrapped "$body" "$shape"; then
            return 0
        fi
    done
    listed "$raw"
}

total=0
parsed=0
checked=0
bad=0
for block in "$work"/*.kh; do
    [ -e "$block" ] || continue
    case "$block" in *candidate.kh) continue ;; esac
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
