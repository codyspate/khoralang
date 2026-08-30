---
title: Lexical structure
sidebar:
  order: 2
---

Khora source is UTF-8 text. Whitespace and comments separate tokens where needed; the syntax tree retains trivia so formatting and documentation tools can preserve source faithfully.

## Identifiers

Ordinary identifiers begin with an ASCII letter or `_` and continue with ASCII letters, digits, or `_`:

```khora
user
user_id
HTTP2
_internal
```

Hard keywords cannot be used as identifiers.

## Row variables

A row variable begins with `'` followed by an identifier:

```khora
'ef
'raises
'capabilities
```

The spelling after `'` has no built-in semantic meaning.

## Integer literals

```khora
0
42
1_000_000
```

Underscores may be used as visual separators.

## Floating-point literals

```khora
0.5
3.14159
6.02e23
1.0E-6
```

A fractional literal without the decimal suffix is an IEEE `Float`.

## Decimal literals

Append `d` directly to the number for an exact `Decimal` literal:

```khora
0d
0.01d
19.99d
1.25e3d
```

`d` is Khora's literal suffix; no whitespace may appear between the number and suffix.

## Boolean literals

```khora
true
false
```

## Quoted strings

```khora
"Khora"
"first\nsecond"
"say \"hello\""
```

Strings contain Unicode text. Backslash introduces escapes.

## String interpolation

`${...}` evaluates an expression inside a string:

```khora
"user ${user.id}"
"count = ${count}"
"the point is ${point} and the list is ${items}"
```

Interpolation is expression syntax, not a separate formatting language.

A hole that already holds a `String` is used as it stands. Anything else is
shown through [`Show`](/docs/reference/traits/), so a value whose type has no
`Show` impl is a compile error naming the type:

```
error: `Colour` has no `Show`, so it cannot go in a `${..}` hole. Write
       `derive(Show)` on it, or `impl Show for Colour`
```

`Show` does not have to be imported to interpolate. The hole is the use, and
the trait is never named in the source.

## Backtick strings

Backticks delimit multiline strings:

```khora
let query = `
  select id, name
  from users
  where active = true
`;
```

`${...}` interpolation and backslash escapes work in backtick strings too. A literal backtick is escaped with `\``.

When a multiline backtick literal opens on its own line, common source indentation is stripped so the surrounding code can remain indented without changing the text's intended layout.

Three things are removed, and the last two are the ones that surprise people:

- the **common indentation**, measured over the whole body and taken off each line, so the deepest-indented line keeps whatever it has beyond the shallowest;
- the newline **immediately after the opening backtick**;
- the newline and indentation **before the closing backtick**.

So this literal is thirty bytes and three lines, with no leading or trailing newline:

```khora
fn block() -> String {
  `
  line one
    indented
  line three
  `
}
```

It is `"line one\n  indented\nline three"`. `indented` keeps two spaces because the common prefix was two, not four. A literal that should *end* with a newline needs a blank line before the closing backtick, and one that should start with a newline needs a blank line after the opening one.

## Line comments

```khora
// ordinary comment
```

API documentation uses three slashes:

```khora
/// Returns the user for `id`.
pub fn load_user(id: Id) -> User;
```

Module documentation uses `//!`:

```khora
//! User-domain types and operations.
module app::users;
```

`khora doc` reads `///` and `//!` comments as Markdown documentation.

## Block comments

```khora
/* a block comment */
```

Block comments may nest:

```khora
/* outer
   /* inner */
   outer again
*/
```

## Hard keywords

These words are reserved:

```text
module import type trait impl fn match let mut pub as
if else forall const effect with raises raise catch
while loop break continue for return true false
```

## Contextual keywords

These words are keywords only in their grammatical position and remain usable as ordinary identifiers elsewhere:

```text
handler in context test bench derive extern
```

Examples of their keyword positions:

```khora
handler for Clock { now: fn () => fixed_instant }
for item in items { process(item); }
context Production { clock: live_clock }
test "works" { assert(true); }
bench "parse" { parse(fixture); }
derive(Eq, Show)
type Point = { x: Int, y: Int };
extern fn strlen(ptr: Ptr) -> U64;
```

`in` is contextual in the `for pattern in expression` form.

## Punctuation with language meaning

Common multi-character tokens include:

```text
::   compile-time path separator
->   function/type arrow
=>   match arm or lambda separator
|>   pipeline
||>  flow lambda
== != <= >=
&& ||
```

Single-character punctuation includes `; , . : | = + - * / % ! < > ( ) { } [ ] _`.

See [Expressions](./expressions/) for operator precedence and [Declarations](./declarations/) for the positions in which the declaration-oriented keywords appear.