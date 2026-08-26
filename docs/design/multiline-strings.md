# D17 — multiline string literals

**Built.** Backticks.

```khora
const SCHEMA = `
  create table if not exists entries (
    id serial primary key,
    account text not null,
    amount int4 not null,
    memo text not null
  )
`;
```

## Why

A string literal is one line, and the first program that embedded SQL found out
by not parsing. `examples/ledger_service` carried this until this landed:

```khora
const SCHEMA: String = "create table if not exists entries (id serial primary key, account text not null, amount int4 not null, memo text not null)";
```

which is the same statement with the shape taken out of it. Every service that
talks to a database has several, and a language that positions itself for
financial reconciliation will meet embedded SQL on the first day. The same
applies to a shell command, an HTML fragment, a test fixture, and a help text.

Backticks because JavaScript, TypeScript and Go readers already know them, and
because the two spellings Khora could otherwise use are both taken or worse:
`"""` is three tokens the lexer would have to disambiguate from an empty string
followed by a string, and a `\` continuation makes the source uglier than the
problem.

## Three things settled while building it

**`${...}` interpolates — decided yes.** `"..."` already does, so consistency
says yes and a TypeScript reader expects it. Against that: the whole point is
embedding *other languages*, and a shell script or a Makefile fragment full of
`${VAR}` would interpolate by surprise. Two ways out — interpolate and require
`\${` to escape, or make backticks raw and add a second marker for the
interpolating form. **Recommendation: interpolate**, matching `"..."`, because
one string with two escaping rules is worse than one rule with an escape, and
because `$1` — the shape that actually appears in SQL — is not `${`.

**The indentation is stripped — decided yes.** A literal written inside a function is
indented to match the code around it, and those spaces are not part of the
string. Java's text blocks, Swift's `"""` and Rust's `indoc` all strip the
common prefix. **Recommendation: strip**, measured from the least-indented
non-blank line, and drop a first line that is empty — so the example at the top
of this page is exactly the SQL and nothing else. A raw form that keeps every
byte can come later if something needs it.

**`\n` and friends still work**, for the same reason interpolation does: two
kinds of string with two escaping rules is a thing to look up. A literal
backtick is `` \` ``, and `\$` still escapes a hole.

## What it touched

The lexer, and one funnel in lowering. A backtick literal is the **same token**
as a quoted one — `STRING_LIT` — so the parser, the type checker, ownership and
the backend never learn there were two spellings. `strip_quotes` is where the
delimiter is recognised, and every consumer already went through it.

**The formatter needed nothing.** A string is one token, and the formatter
re-indents between tokens, so a literal's interior is untouched by
construction. Worth checking rather than assuming, because a formatter that
re-indented the inside of one would silently change what the program means; a
test pins it.

**Interpolation and the dedent were the one hard part.** The `${..}` splitter
computes offsets into the *file* so that a diagnostic inside a hole points at
the right column — so the body cannot be dedented before splitting, or every
hole moves. The indent is measured over the whole body and taken off each text
piece as it is lowered, and the opening and closing blank lines are trimmed
from whichever piece carries them. A literal that opens with a hole has no
first text piece and nothing to trim, which is the right answer rather than a
special case.

## What it does not do

**No raw form.** Escapes work inside a backtick literal, so a Windows path or a
regular expression still doubles its backslashes. A second marker for a raw
string is the obvious extension and nothing has wanted it.

**A literal that opens on the same line as its content strips nothing**, since
that line's indentation is zero and zero is the minimum. That is the documented
way to opt out, and it is why the recommended shape puts the delimiter on its
own line.
