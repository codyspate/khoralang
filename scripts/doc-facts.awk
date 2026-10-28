# Reads a documentation block's checker errors and prints the ones that are
# facts about the block. Used by `check-docs.sh`; the rules are listed there,
# above `typechecked`, and each one below says which it is.
#
# Inputs, in order: the names `std` supplies (one per line), the block's own
# text, the program it was checked as, and the checker's output. `statements`
# is the line of that program where the block's statements begin, or 0 when it
# has none; `declared` is the names the block makes its own, space-separated
# with a leading space.

FILENAME == ARGV[1] { known[$0] = 1; next }

# The builtin types, which `std` uses and never declares.
BEGIN {
    split("Int Float Bool String Char Unit Never U8 U16 U32 U64 I8 I16 I32 I64", builtin, " ")
    for (b in builtin) known[builtin[b]] = 1
}

FILENAME == ARGV[2] {
    line = $0
    while (match(line, /[A-Za-z_][A-Za-z0-9_]*/)) {
        written[substr(line, RSTART, RLENGTH)] = 1
        line = substr(line, RSTART + RLENGTH)
    }
    next
}

FILENAME == ARGV[3] { program[FNR] = $0; lines = FNR; next }

/^error: / { said[++count] = substr($0, 8); at[count] = 0 }
# The line of the program an error points at, which says which function it is
# about.
/^ *--> / && count > 0 && at[count] == 0 {
    spot = $0
    sub(/:[0-9]+$/, "", spot)
    sub(/^.*:/, "", spot)
    at[count] = spot + 0
}

# The name a backticked piece of a message is about: `Fiber::wait` is about
# `Fiber`, `Raised<A + B>` about `Raised`.
function head(piece) {
    sub(/^[^A-Za-z_(]*/, "", piece)
    sub(/[^A-Za-z0-9_].*$/, "", piece)
    return piece
}

# A name the block neither declares nor gets from `std`, or one it uses as a
# value of its own (`clock.sleep`) that nothing in the block binds. A
# capitalized name one typo away from a `std` type is neither: `Mapp` is
# `Map` misspelled, or a `std` type renamed under the example, and reading it
# as the prose's own type would excuse exactly the rot this looks for.
function outside(name) {
    if (name == "") return 0
    if (index(declared, " " name " ") > 0) return 1
    return !(name in known) && !near_std(name)
}

# A type in a mismatch is the prose's only if the block does not declare it
# either: `expected `Int`, found `Point`` about the block's own `Point` is a
# fact about the block.
function foreign(name) {
    return name ~ /^[A-Z]/ && !(name in known) && index(declared, " " name " ") == 0 \
        && !near_std(name)
}

# Within one edit of a capitalized `std` name, or two when both are seven
# letters or more. A name under five letters counts only by a letter added or
# dropped (`Mapp`, `Lst`): one letter changed in a short word is another
# word, and `Id`, `Pool` are the prose's names, not `Io` and `Poll` misspelled.
function near_std(name,    other, limit) {
    if (name !~ /^[A-Z]/) return 0
    for (other in known) {
        if (other !~ /^[A-Z]/) continue
        if (other == name) continue
        limit = (length(name) >= 7 && length(other) >= 7) ? 2 : 1
        if (length(name) - length(other) > limit || length(other) - length(name) > limit) continue
        if (length(name) < 5 && length(name) == length(other)) continue
        if (distance(name, other) <= limit) return 1
    }
    return 0
}

# Levenshtein distance between two words.
function distance(a, b,    i, j, n, m, row, prev, cost, best) {
    n = length(a); m = length(b)
    for (j = 0; j <= m; j++) prev[j] = j
    for (i = 1; i <= n; i++) {
        row[0] = i
        for (j = 1; j <= m; j++) {
            cost = substr(a, i, 1) == substr(b, j, 1) ? 0 : 1
            best = prev[j] + 1
            if (row[j - 1] + 1 < best) best = row[j - 1] + 1
            if (prev[j - 1] + cost < best) best = prev[j - 1] + cost
            row[j] = best
        }
        for (j = 0; j <= m; j++) prev[j] = row[j]
    }
    return prev[m]
}

# Rule 1: the message is about such a name. Where the message says a name is
# missing, the name is the first one it quotes. Where it says two types do not
# match, either side may be such a type -- `expected `String`, found `Id`` is
# about the `Id` the block never declared, not about the `String`. Nothing
# else counts: `has no case `Timeout`` quotes a case, which is a fact about a
# real type.
function about_outside(message,    rest, piece, name) {
    if (message ~ /^cannot find module `std::/) return 0
    if (message ~ /^cannot find module /) return 1
    if (message ~ /^`[^`]+` is not a trait in scope/) {
        # Had `std` such a trait, it would be in scope.
        return outside_or_std_type(head(substr(message, 2)))
    }
    if (message ~ /^cannot (find|resolve)( type| constructor)? `/ \
        || message ~ /^`[^`]+` is not imported here, so/ \
        || message ~ /^`[^`]+` is not in scope here, so nothing is known/ \
        || message ~ /^`[^`]+` is not a record( type|, so)/) {
        if (message ~ /has no case `/) return 0
        return outside(head(substr(message, index(message, "`") + 1)))
    }
    if (message ~ /expected `[^`]+`, found `[^`]+`/ || message ~ /^match arms disagree/ \
        || message ~ /^`derive\([A-Za-z]+\)` needs every field/ \
        || message ~ /cannot be handed to another fiber: `[^`]+` is declared without a body/ \
        || message ~ /needs `[^`]+`, which this function does not raise$/) {
        rest = message
        while (match(rest, /`[^`]+`/)) {
            piece = substr(rest, RSTART + 1, RLENGTH - 2)
            rest = substr(rest, RSTART + RLENGTH)
            name = head(piece)
            if (foreign(name)) return 1
        }
    }
    return 0
}

function outside_or_std_type(name) {
    return index(declared, " " name " ") == 0
}

# Rule 2: a record literal given the one `std` type in scope with its fields.
function inferred_std_record(message,    name) {
    if (message !~ /^this `[^`]+` is missing `/ && message !~ /^cannot build `[^`]+` here/) return 0
    name = head(substr(message, index(message, "`") + 1))
    return (name in known) && !(name in written)
}

# Rule 3: what the function a statement was cut from would have said. Only for
# an error inside the statements -- from line `statements` of the program on.
function the_surroundings(message, line) {
    if (statements == 0 || line < statements) return 0
    # A capability the statements use is one the function they were cut from
    # had in its `with` clause. The checker names it as a capability rather
    # than as a missing name, and this matches that message, not the old one.
    return message ~ /needs `[^`]+`, which this function does not (raise|require)$/ \
        || message ~ /^`[a-z_]+` is a capability, and nothing here provides it/ \
        || message ~ /^`(break|continue)` outside a loop/ \
        || message ~ /^this `return`: expected / \
        || message ~ /^this expression cannot be assigned to$/ \
        || message ~ /^`_` is only meaningful in a pipeline stage/ \
        || message ~ /^`\(.*\)` is not a function, so it cannot be called/
}

# Rule 4: a body written as `// ...`. Only the function the error points at,
# and only when that function's own body is the elision: one elided body in a
# block says nothing about its neighbors.
function elided_body(message, line,    i, depth, opened, text) {
    if (message !~ /^this function returns `[^`]+`, but its body has type `\(\)`$/) return 0
    depth = 0; opened = 0
    for (i = line; i >= 1 && i <= lines; i++) {
        text = program[i]
        if (text ~ /\/\/ \.\.\./) return 1
        depth += gsub(/\{/, "{", text) - gsub(/\}/, "}", text)
        if (depth > 0) opened = 1
        if (opened && depth <= 0) return 0
    }
    return 0
}

# Rule 5: what follows from rule 1.
function follows(message) {
    return message ~ /binds the failure, but the operand raises nothing/ \
        || message ~ /^nothing in this expression raises / \
        || message ~ /^the type of this closure was never pinned down/ \
        || message ~ /^this has to be a closure written here or a named function/ \
        || message ~ /^no record type has exactly the fields / \
        || (raises_nothing && message ~ /^this `match` is not exhaustive/)
}

# Rule 6: a block that restates `std`'s own declarations, which is how the
# Reference shows a type's surface. It parses; checking it would be checking
# that `std` does not declare its own type twice.
function restates_std(message,    name) {
    if (message !~ /^`[^`]+` already has a method named `/) return 0
    name = head(substr(message, 2))
    return name in known
}

END {
    for (i = 1; i <= count; i++)
        if (said[i] ~ /^nothing in this expression raises /) raises_nothing = 1
    for (i = 1; i <= count; i++) {
        message = said[i]
        if (message ~ /^[0-9]+ error\(s\)/) continue
        # A list of types parsed as a tuple of values: the block is a type,
        # and says so by naming a builtin type where a value goes.
        if (statements > 0 && at[i] >= statements && message ~ /^cannot find `(Int|Float|Bool|String|Char|U8|U16|U32|U64|I8|I16|I32|I64)` in this scope$/) {
            print "#type"
            continue
        }
        if (about_outside(message) || inferred_std_record(message) || the_surroundings(message, at[i]) \
            || elided_body(message, at[i]) || follows(message) || restates_std(message)) continue
        print "    " message
    }
}
