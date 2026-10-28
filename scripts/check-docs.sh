#!/bin/sh
# Every hand-written Khora example in the documentation, and every example in
# `std`'s own `///` comments, against this compiler.
#
# **The generated pages were the only ones anything checked.** `khora doc std
# --check` fails when a page under `stdlib/api` drifts from the `///` comments
# it came from -- but a comment and a page can agree perfectly about an example
# that does not compile. So this reads those comments too, and the hand-written
# pages in the Guide, the Reference and the Cookbook: the pages a person
# actually reads first.
#
# Three levels, because most blocks are fragments:
#
#   *every* block is parsed. This catches syntax that no longer exists, which
#   is the way documentation rots fastest.
#
#   a block that declares its own `module` is a whole program and is *checked*:
#   names resolved, types inferred, rows accounted for.
#
#   a fragment that is statements or declarations is *type-checked* too, with
#   every type `std` exports imported. It still refers to names it never
#   declares -- `load_user(id)!` on a page about failures -- and inventing them
#   would be a second document to keep true, so an error that comes only from
#   such a name is let through; `typechecked` below lists exactly which.
#   Everything a fragment says about `std` is held to: a method that is gone, a
#   case that does not exist, a wrong argument, a missing `!`.
#
# A fragment that is a type, a pattern, a row or a parameter list is only
# parsed, because none of them is code there is anything to check in. And a
# block may opt out with `<!-- unchecked: the reason -->` on the line before
# its fence; the run counts those and prints the count.
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
# halves", which would excuse almost anything. The list is not that: each chunk
# must parse alone, and a list of declarations and statements is then
# type-checked as the program those chunks make, so a chunk that only parsed by
# being cut from its neighbors is caught there.
#
# # What it does not catch
#
# A block that only parses -- a type, a pattern, a row -- passes if it is
# *syntactically* fine and means the wrong thing. `List<String` is the sharp
# case: with the closing bracket missing it is still a valid comparison
# expression, and no amount of wrapping makes a parser say otherwise. A
# fragment's uses of a name it never declares are not checked either, since the
# checker has nothing to check them against. That includes a capability used
# through its conventional binding without a `with { .. }` that names it:
# `nursery.spawn(42)` reads `nursery` as the prose's own local, so the wrong
# argument is never seen. Naming the capability in a signature makes it checked.
#
# A list whose chunks are code -- declarations, then statements -- is checked
# as one program: the declarations at module level, the statements in order in
# one function. Its types, rows and signatures still only parse.
#
# The way to make an example carry its full weight is still to write it as a
# whole module, which a reader can also copy and run.
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
#
# Type-checking the fragments found twenty-odd more, all of them examples a
# reader would copy and watch fail: a `handler for Clock` with a `now` field
# `Clock` does not have, a `catch` on `DbError` cases that do not exist,
# `String::length` (it is `byte_length`), `extern fn strlen` with a signature
# `std` declares differently, and `Fiber::wait` and `nursery` calls with no `!`.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

# **`target/` is not always the target directory.** `CARGO_TARGET_DIR`
# moves it, which a machine building two platforms out of one checkout has
# to do -- and this then looked for a compiler that was never going to be
# there and reported every documentation block unparseable, including on
# pages nobody had touched. A gate that fails everywhere it is run somewhere
# new teaches people to ignore it.
target="${CARGO_TARGET_DIR:-target}"
khora="$target/debug/khora.exe"
[ -x "$khora" ] || khora="$target/debug/khora"

work="${TMPDIR:-/tmp}/khora-doc-blocks"
rm -rf "$work"
mkdir -p "$work"

# `KHORA_DOC_CENSUS=file` appends one line per block that only parses -- its
# shape and where it is -- so that "these only parse" can be audited instead
# of believed. A count that hides type-checkable code is how blocks mixing
# declarations and statements went unchecked.

# The prose, and the examples in `std`'s own `///` comments. `stdlib/api` is
# generated from those comments, so it is read at the source rather than
# twice.
#
# Named pages are accepted so that one can be checked while it is being
# written -- the whole tree is several hundred `khora` calls and takes
# minutes, which is right for a gate step and wrong for a loop somebody is
# sitting in front of.
if [ "$#" -gt 0 ]; then
    pages="$*"
else
    pages=$(find website/content/docs -name '*.md' -not -path '*/stdlib/api/*' | sort)
    pages="$pages $(find std -name '*.kh' | sort)"
fi

# **Every type `std` exports, and `std::core`'s functions, imported, so a
# fragment can be type-checked.** A fragment that calls `Map::insert` or names
# a `Clock` was written for a reader who has the import; giving it the import
# is what lets the checker see the example the reader sees. Built from the
# sources rather than listed here, so a name added to `std` is in scope without
# anybody editing this file.
#
# Not every free function: `server`, `text`, `parse` and `run` are std
# functions and also the names examples give their own values, and importing
# them turned a fragment's `server` into `std::net::tls::server` and failed it
# for a type it never had. A platform file (`_windows`, `_macos`) is left out:
# it declares the same module as the Linux one and would import every name
# twice.
imports="$work/imports.inc"
names="$work/std-names"
for source in $(find std -name '*.kh' -not -name '*_windows.kh' -not -name '*_macos.kh' | sort); do
    awk '
        /^module [A-Za-z_:]+;/ { home = $2; sub(/;$/, "", home) }
        /^pub (type|effect|trait|fn|const|extern fn) / {
            name = $0
            sub(/^pub (extern fn|type|effect|trait|fn|const) /, "", name)
            sub(/[^A-Za-z0-9_].*$/, "", name)
            if (name ~ /^[A-Z]/ || home == "std::core") print home "\t" name
        }
    ' "$source"
done | sort -u | awk -F'\t' '
    # One name, one import: `std::core` first, as `std` itself imports from
    # it, and a later module never shadows a name an earlier one gave.
    { order = ($1 == "std::core") ? 0 : 1; print order "\t" $1 "\t" $2 }
' | sort | awk -F'\t' '
    seen[$3]++ { next }
    $2 != home { if (home != "") printf "};\n"; home = $2; printf "import %s::{%s", home, $3; next }
    { printf ", %s", $3 }
    END { if (home != "") printf "};\n" }
' > "$imports"
sed 's/^import [^{]*{//; s/};$//; s/, /\n/g' "$imports" > "$names"

# One file per block, twice: `.raw` is the body exactly as written, and
# `.part`/`.whole` is what gets parsed first. `whole` in the name when the
# block declared a module, so the loop below knows which to check as well.
#
# **A block may say why it cannot be checked**, on the line right before its
# fence: `<!-- unchecked: the reason -->`, or the same after `///` in `std`.
# The reason is recorded beside the block and counted, so a page cannot opt
# out without saying so, and the total of opted-out blocks is printed every run.
#
# **No `gensub`.** It is gawk's, and this runs under whatever `awk` the machine
# has — mawk on Debian, BSD awk on macOS. The block's number comes from the
# shell instead, and the page it came from is recorded beside it, so a failure
# can name the page without the file name having to encode it.
page_no=0
for page in $pages; do
    page_no=$((page_no + 1))
    awk -v page="$page" -v work="$work" -v seq="$page_no" '
        # In a `.kh` file only `///` lines are documentation; anything else
        # ends a block, since a fence cannot span code.
        {
            text = $0
            if (page ~ /\.kh$/) {
                if (text ~ /^[ \t]*\/\/\/ ?/) sub(/^[ \t]*\/\/\/ ?/, "", text)
                else { text = "\001"; inside = 0 }
            }
        }
        text == "```khora" {
            inside = 1; n += 1; body = ""; first = ""; line = NR
            reason = ""
            if (previous ~ /^<!-- unchecked: .* -->$/) {
                reason = previous
                sub(/^<!-- unchecked: /, "", reason)
                sub(/ -->$/, "", reason)
            }
            next
        }
        text == "```" && inside {
            inside = 0
            whole = (first ~ /^module /) ? "whole" : "part"
            file = sprintf("%s/%04d.%03d.%s.kh", work, seq, n, whole)
            if (whole == "part") printf "module doc;\n\n" > file
            printf "%s", body > file
            close(file)
            raw = sprintf("%s/%04d.%03d.raw", work, seq, n)
            printf "%s", body > raw
            close(raw)
            printf "%s\t%s:%d\t%s\n", file, page, line, reason >> (work "/index")
            previous = text
            next
        }
        inside {
            if (first == "" && text ~ /[^ \t]/) first = text
            body = body text "\n"
        }
        { previous = text }
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
    chunks=0
    # Each chunk is kept beside the block as `chunk.N.SHAPE`, so that the
    # chunks that are code -- declarations and statements -- can be checked
    # together afterwards. See `typechecked`.
    rm -f "$work"/chunk.*
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

        # A `derive(..)` line belongs to the declaration under it. Alone it
        # parses as a call, and the type below would be checked without it.
        case "$chunk" in
        "derive("*")
") continue ;;
        esac

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
                # A chunk that is also a type -- `Point`, `List<Int>` -- is
                # one: as a statement it says nothing, and checking it as one
                # reports a value that was never meant.
                if [ "$shape" = inner ] && wrapped "$chunk" astype; then
                    shape=astype
                fi
                chunks=$((chunks + 1))
                printf '%s' "$chunk" > "$work/chunk.$(printf '%03d' "$chunks").$shape"
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
    shape=whole
    if "$khora" parse "$1" > "$work/out" 2>&1; then
        case "$1" in *.part.kh) shape=declaration ;; esac
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
    listed "$raw" || return 1
    # Set after, because `listed` walks the same shapes and leaves the last.
    shape=listed
}

# A fragment that parsed as statements or as declarations, type-checked with
# `std` imported. Leaves each error that is a fact about the example in
# `$work/facts`, and succeeds when there is none.
#
# **What it lets through is what the block cannot know.** A fragment calls
# `load_user(id)!` because the prose described it, and declaring it would be
# writing a second program to keep true; the checker cannot see it, so an error
# that comes from not seeing it is not about the example. Everything else -- a
# `std` type with no such method, field or case, a wrong argument, a missing
# `!`, a value of the wrong type -- is the rot this exists to find, and fails.
# The rules, in `scripts/doc-facts.awk` beside this script, are each one sentence:
#
#   1. an error about a name the block neither declares nor gets from `std`;
#   2. an error about a `std` type the block never writes, which is inference
#      giving a record literal the only type in scope with those fields;
#   3. for a statement, what the function it was cut from would have said:
#      an error or capability it needs, a `break` outside the loop, a
#      `return` of that function's type, an assignment to its local;
#   4. a body written as `// ...`;
#   5. what follows from rule 1: a `catch` over an operand that raises
#      nothing the checker can see, a closure whose type that left open;
#   6. a block restating `std`'s own declarations, as the Reference does to
#      show a type's surface.
#
# What the rules cannot tell from rule 1 is a free function `std` used to
# export: it reads as one the example never declared. A removed type, method,
# field or case does not.
typechecked() {
    # A `listed` block is checked as the program it is a piece of: its
    # declaration chunks at module level, its statement chunks in one
    # function, in the order they were written. Its types, patterns, rows and
    # signatures stay parsed only. A block with no code in it at all is a list
    # of those, and counts with them.
    decls=""
    stmts=""
    if [ "$1" = listed ]; then
        for piece in "$work"/chunk.*; do
            [ -e "$piece" ] || continue
            case "$piece" in
            *.declaration) decls="$decls$(cat "$piece")
" ;;
            *.inner)
                # Each chunk is a statement of its own, and its last line may
                # be a bare expression that needs a `;` before the next. When
                # that line ends in a comment, the `;` goes before it.
                piece_text=$(cat "$piece")
                last=${piece_text##*
}
                code=${last%%//*}
                case "$last" in
                *"//"*) ;;
                *) code=$last ;;
                esac
                code_trimmed=${code%"${code##*[![:space:]]}"}
                case "$code_trimmed" in
                *";" | "") stmts="$stmts$piece_text
" ;;
                *) stmts="$stmts${piece_text%"$last"}$code_trimmed;${last#"$code_trimmed"}
" ;;
                esac
                ;;
            esac
        done
        [ -n "$decls$stmts" ] || return 2
        set -- listed "$decls$stmts"
    fi
    printf '%s\n' "$2" > "$work/body"
    # Names the block makes its own: its declarations, its `let`s, and what it
    # uses as a receiver. A receiver is the block's even when `std::core` has a
    # function of that name -- `nursery.spawn` is a capability -- unless the
    # block also calls it, as `nursery(fn () => ..)` does.
    declared=$(awk '
        /^(pub )?(extern fn|type|effect|trait|fn|const|context) / {
            name = $0
            sub(/^pub /, "", name)
            sub(/^(extern fn|type|effect|trait|fn|const|context) /, "", name)
            sub(/[^A-Za-z0-9_].*$/, "", name)
            own[name] = 1
        }
        {
            line = $0
            while (match(line, /[a-z_][A-Za-z0-9_]*\./)) {
                receiver[substr(line, RSTART, RLENGTH - 1)] = 1
                line = substr(line, RSTART + RLENGTH)
            }
            line = $0
            while (match(line, /[a-z_][A-Za-z0-9_]*\(/)) {
                called[substr(line, RSTART, RLENGTH - 1)] = 1
                line = substr(line, RSTART + RLENGTH)
            }
            line = $0
            while (match(line, /let (mut )?[a-z_][A-Za-z0-9_]*/)) {
                word = substr(line, RSTART, RLENGTH); sub(/^let (mut )?/, "", word)
                own[word] = 1
                line = substr(line, RSTART + RLENGTH)
            }
        }
        END {
            for (name in receiver) if (!(name in called)) own[name] = 1
            for (name in own) printf "%s ", name
        }' "$work/body")
    {
        printf 'module doc;\n\n'
        # The block's own declarations win over `std`'s, as they would in the
        # program the block was cut from.
        awk -v skip=" $declared" '
            {
                head = $0; sub(/\{.*$/, "{", head)
                list = $0; sub(/^[^{]*\{/, "", list); sub(/\};$/, "", list)
                n = split(list, each, ", ")
                kept = ""
                for (i = 1; i <= n; i++)
                    if (index(skip, " " each[i] " ") == 0) kept = kept (kept == "" ? "" : ", ") each[i]
                if (kept != "") print head kept "};"
            }
        ' "$imports"
        printf '\n'
        # A statement's value is discarded, as it is wherever it was cut from:
        # the tail of `fn example() -> ()` would otherwise be asked to be `()`.
        # A `listed` block's declarations come first, at module level.
        case "$1" in
        inner) printf 'fn example() -> () {\n  let _ = {\n%s\n  };\n}\n' "$2" ;;
        listed)
            printf '%s\n' "$decls"
            if [ -n "$stmts" ]; then
                printf 'fn example() -> () {\n  let _ = {\n%s\n  };\n}\n' "$stmts"
            fi
            ;;
        *) printf '%s\n' "$2" ;;
        esac
    } > "$candidate"
    "$khora" check "$candidate" > "$work/out" 2>&1 && return 0
    # Where the statements start: an error from there on is about a statement,
    # and what the function it was cut from would have said is excused for it
    # and for nothing above it.
    statements=$(awk '/^fn example\(\) -> \(\) \{$/ { print NR; exit }' "$candidate")
    awk -v statements="${statements:-0}" -v declared=" $declared" \
        -f "$root/scripts/doc-facts.awk" "$names" "$work/body" "$candidate" "$work/out" > "$work/facts"
    # A block the rules recognized as a type rather than a statement only
    # parses, and is counted with the other types.
    if grep -q '^#type$' "$work/facts" && ! grep -qv '^#type$' "$work/facts"; then
        return 2
    fi
    [ ! -s "$work/facts" ]
}

total=0
parsed=0
checked=0
typed=0
parse_only=0
excused=0
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
    reason=$(awk -F'\t' -v want="$block" '$1 == want { print $3; exit }' "$work/index")
    if [ -n "$reason" ]; then
        excused=$((excused + 1))
        continue
    fi
    case "$shape" in
    whole)
        if ! "$khora" check "$block" > "$work/out" 2>&1; then
            printf '  FAILED  %s does not check\n' "$(where "$block")" >&2
            grep '^error' "$work/out" | head -3 >&2
            bad=$((bad + 1))
            continue
        fi
        checked=$((checked + 1))
        ;;
    inner | declaration | listed)
        verdict=0
        typechecked "$shape" "$(cat "${block%.part.kh}.raw")" || verdict=$?
        case $verdict in
        0) typed=$((typed + 1)) ;;
        2)
            parse_only=$((parse_only + 1))
            [ -z "${KHORA_DOC_CENSUS:-}" ] || printf 'PARSEONLY-%s\t%s\n' "$shape" "$(where "$block")" >> "$KHORA_DOC_CENSUS"
            ;;
        *)
            printf '  FAILED  %s does not type-check\n' "$(where "$block")" >&2
            grep -v '^#type$' "$work/facts" | head -3 >&2
            bad=$((bad + 1))
            ;;
        esac
        ;;
    *)
        parse_only=$((parse_only + 1))
        [ -z "${KHORA_DOC_CENSUS:-}" ] || printf 'PARSEONLY-%s\t%s\n' "$shape" "$(where "$block")" >> "$KHORA_DOC_CENSUS"
        ;;
    esac
done

if [ "$bad" -gt 0 ]; then
    printf '  FAILED  %d of %d documentation example(s)\n' "$bad" "$total" >&2
    printf '  The blocks are kept at %s\n' "$work" >&2
    exit 1
fi

printf '  ok    %d example(s) parse: %d check as whole programs, %d type-check as\n' "$parsed" "$checked" "$typed"
printf '        fragments, %d are a type, a pattern or a row and only parse, and %d\n' "$parse_only" "$excused"
printf '        say why they are not checked\n'
