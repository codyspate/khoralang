#!/bin/sh
#
# The published grammar against the lexer it claims to describe.
#
# `docs/grammar.ebnf` is served to MCP clients by `khora-mcp` and mirrored into
# the public reference, and at 1.0 "language syntax" is a frozen surface -- so
# the grammar is the artefact people will be held to, and nothing checked it.
# It had drifted far: it named `export` as the visibility keyword, which the
# lexer does not have and `reference/declarations.md` says outright is not it;
# `trait`, `impl`, `for` and `pub` were missing from its keyword table;
# `TraitDecl` and `ImplDecl` were written out and unreachable from
# `Declaration`; and `derive`, `extern`, `||>`, record update and string
# interpolation were absent altogether. Roadmap 16.
#
# What this can check is the part that is mechanical: every hard and contextual
# keyword the lexer defines appears in the grammar's own tables, and no word
# appears there that the lexer does not have. It deliberately does not claim to
# check that the productions describe the parser -- no script can, and
# pretending otherwise would be worse than the gap.
set -eu

cd "$(dirname "$0")/.."

grammar='docs/grammar.ebnf'
kinds='crates/khora-syntax/src/kind.rs'

section () { sed -n "/^$1! {/,/^}/p" "$kinds" | grep -o '"[a-z_]*"' | tr -d '"' | sort -u; }

# Comments are stripped first: every entry in these tables carries one saying
# where the word is a keyword, and those sentences name other keywords.
# Without this the table appears to define `fn` because `extern` is documented
# as "only before `fn`".
table () {
    sed -n "/^$1/,/;$/p" "$grammar"         | sed 's/([*].*[*])//g'         | grep -o '"[a-z_]*"' | tr -d '"' | sort -u
}

# **Temporary files rather than `<(...)`.** `baseline.sh` runs every gate
# script with `sh`, which on Debian is dash, and process substitution is a bash
# extension -- so this passed under `bash scripts/check-grammar.sh` and failed
# the gate with `Syntax error: "(" unexpected`, which is the one way a new
# check can be worse than no check.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

status=0
for pair in "keywords:Keyword" "contextual_keywords:ContextualKeyword"; do
    lexer_section="${pair%%:*}"
    grammar_table="${pair##*:}"

    section "$lexer_section" > "$work/lexer"
    table "$grammar_table" > "$work/grammar"
    missing=$(comm -23 "$work/lexer" "$work/grammar")
    invented=$(comm -13 "$work/lexer" "$work/grammar")

    if [ -n "$missing" ]; then
        printf '  FAILED  %s in the lexer and not in `%s` of %s:\n' \
            "$lexer_section" "$grammar_table" "$grammar" >&2
        printf '            %s\n' $missing >&2
        status=1
    fi
    if [ -n "$invented" ]; then
        printf '  FAILED  `%s` of %s names words the lexer does not have:\n' \
            "$grammar_table" "$grammar" >&2
        printf '            %s\n' $invented >&2
        status=1
    fi
done

# A production written out and never referenced is the other way this drifted:
# `TraitDecl`, `ImplDecl` and `ForExpr` were all defined and unreachable from
# the start symbol, so a reader following the grammar could not get to a trait.
# `Program` is the start symbol, and the three lexical tables are descriptions
# of the token stream rather than productions anything derives.
for rule in $(grep -oE '^[A-Za-z]+' "$grammar" | sort -u); do
    case "$rule" in
        Program|Keyword|ContextualKeyword|Comment) continue ;;
    esac
    uses=$(grep -c "\b$rule\b" "$grammar" || true)
    if [ "$uses" -le 1 ]; then
        printf '  FAILED  `%s` is defined in %s and never used\n' "$rule" "$grammar" >&2
        status=1
    fi
done

[ "$status" -eq 0 ] && printf '  ok    the published grammar matches the lexer\n'
exit "$status"
