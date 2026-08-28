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
"count = ${Int::to_string(count)}"
```

Interpolation is expression syntax, not a separate formatting language.

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