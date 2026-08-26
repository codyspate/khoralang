# D17 — multiline string literals

**Proposed, not built.** Decided: **backticks**.

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
by not parsing. `examples/ledger_service` currently carries this:

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

## Three things to settle when building it

**Does `${...}` interpolate inside one?** `"..."` already does, so consistency
says yes and a TypeScript reader expects it. Against that: the whole point is
embedding *other languages*, and a shell script or a Makefile fragment full of
`${VAR}` would interpolate by surprise. Two ways out — interpolate and require
`\${` to escape, or make backticks raw and add a second marker for the
interpolating form. **Recommendation: interpolate**, matching `"..."`, because
one string with two escaping rules is worse than one rule with an escape, and
because `$1` — the shape that actually appears in SQL — is not `${`.

**Is the leading indentation stripped?** A literal written inside a function is
indented to match the code around it, and those spaces are not part of the
string. Java's text blocks, Swift's `"""` and Rust's `indoc` all strip the
common prefix. **Recommendation: strip**, measured from the least-indented
non-blank line, and drop a first line that is empty — so the example at the top
of this page is exactly the SQL and nothing else. A raw form that keeps every
byte can come later if something needs it.

**Do `\n` and friends still work?** They should, for the same reason
interpolation should: two kinds of string with two escaping rules is a thing to
look up. A literal backtick is then `` \` ``.

## What it touches

The lexer, which is the whole of it: one more token shape, and
`literal_of`/`has_interpolation` learn to accept it. Nothing in the parser, the
HIR, the type checker or the backend changes, because the result is a `String`
exactly as today's literal is — which is the same argument the flow operator
made and the reason both are cheap.

The formatter must leave the inside of one alone, which is the one place this
is not free: it currently re-indents by token, and a multiline literal is a
token whose interior is content.

## Why it is not built yet

Nothing blocks it and nothing is waiting on it — the one place it is wanted has
a one-line workaround with a comment saying so. It is written down now because
the decision is made and the reasoning is cheap to lose.
