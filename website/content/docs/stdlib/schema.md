---
title: Schemas
sidebar:
  order: 1
---

`std::schema` describes the shape of a value once, and decodes it from
anywhere. The description does not know where the bytes came from, which is the
whole of the library and the reason it is worth a page.

Everything else in this section is reached through the generated
[`std::schema` API](/docs/stdlib/api/schema/). This page is the model behind it.

## The separation it is built on

`std::config` reads settings well, and none of it is reusable. Its signature
says why:

```khora
pub fn string(name: String) -> Validated<String, ConfigError> with { env: Env }
```

`string(name)` is not *this field is text*. It is *go to the environment, fetch
this variable, and give me the text or a reason* — the shape and the reading
are one function. A JSON body then needs its own vocabulary, and so does a CLI
argument, and so does a database row, and none of them can share a description
with the others.

A `Schema<A>` splits that in half. It describes an `A`; a [`Raw`] is whatever
some source produced; `decode` is one function over the pair.

```khora
let settings = Schema::decode(schema(), incoming)!;
```

Where `incoming` came from is the caller's business. The same
`Schema<Settings>` reads the environment, a request body and a test fixture.

## A schema is two halves in one record

```khora
pub type Schema<A> = {
  shape: Shape,
  read: (List<Segment>, Raw) -> Validated<A, Rejection>,
};
```

Both halves are load-bearing, and neither would do on its own.

The **closure** is what makes `schema.decode(value)` an ordinary call, and what
lets schemas be combined — `optional(many(int()))` is three closures wrapped
around each other.

The **shape** is untyped, deliberately: it has no type parameter, which is what
lets a record's fields — all of different types — sit in one `List` and be
walked. That is how a deployment can ask which keys a configuration needs
without starting the program:

```khora
print("it needs: ${Shape::keys(schema().shape)}");
```

A record of two closures would have lost the shape. A bare description tree
would have lost the decoder. `docs/design/schema.md` in the repository has the
argument in full.

## The vocabulary

Four primitives, four combinators that wrap another schema, and four assemblers.

| | |
| --- | --- |
| [`string()`](/docs/stdlib/api/schema/#string) | a `String`, as it arrived; a number or a boolean is accepted as its token |
| [`int()`](/docs/stdlib/api/schema/#int) | an `Int` |
| [`decimal()`](/docs/stdlib/api/schema/#decimal) | a `Decimal`, parsed from the token |
| [`bool()`](/docs/stdlib/api/schema/#bool) | a `Bool` |
| [`optional(s)`](/docs/stdlib/api/schema/#optional) | an `Option<A>`; the only combinator that can tell a missing field from a present one |
| [`many(s)`](/docs/stdlib/api/schema/#many) | a `List<A>`, indexing each element into the error path |
| [`refine(s, must, holds)`](/docs/stdlib/api/schema/#refine) | the same `A`, rejected unless `holds`; `must` is the sentence the message uses |
| [`secret(s)`](/docs/stdlib/api/schema/#secret) | a `Redacted<A>`, and a failure inside it does not quote what it saw |
| [`struct2`](/docs/stdlib/api/schema/#struct2) … [`struct5`](/docs/stdlib/api/schema/#struct5) | a record of that many named fields |

They compose in the obvious way, which is most of the point:

```khora
refine(int(), "between 1 and 65535", fn p => p > 0 && p < 65536)
```

[Decode untrusted input](/docs/cookbook/decoding-input/) puts them together
into a program that runs.

## What a failure is

`decode` answers a [`Validated`](/docs/stdlib/api/core/#validated), not a
`Result`, so a record with four bad fields reports four. A person fixing a
deployment wants the list, not one line of it per restart:

```text
refused:
  listen.host is not set
  listen.port must be between 1 and 65535
  password is not set
  rate should be an exact decimal, and is `about seven percent`
```

Each line is one [`Rejection`](/docs/stdlib/api/schema/#rejection), and
`Rejection::describe` is what wrote it. A rejection carries a path, so it can
say `listen.port` or `items[3].id`; the path is held innermost-first and turned
round only when a message is built.

`Validated::to_result` is one call for a caller who would rather stop at the
first problem.

### A number keeps its text

`Raw::Number` holds the token, not a `Float`. A `Float` carries about fifteen
significant digits, so `9007199254740993` comes back one short of itself and
`10.10` is never exactly recoverable — and a schema that decoded a `Decimal`
through one would rebuild, inside the library meant to prevent that class of
thing, the exact bug it exists to prevent.

`std::json` keeps its numbers the same way, so the two trees no longer disagree
about what a number is. They remain separate types: a `Raw` is what *any*
source produced, and JSON is one source.

### A secret does not appear in its own error

Quoting the bad value is most of what makes a decode error worth reading, and
it is also the easiest imaginable way to put a password in a log. So
`Rejection`'s `found` is a `Redacted<String>` unconditionally — not only inside
a secret — and `describe` decides once whether to expose it. No later variant
can forget.

```text
public should be a whole number, and is `not a number`
token should be a whole number
```

## What it does not do yet

**`derive(Schema)` is not built.** The spelling a reader reaches for —

```khora
struct({ port: int(), host: string() })
```

— cannot be typed: its argument is a record of *schemas*, and the result would
have to be a schema of the record of what they decode, which needs a type-level
map Khora does not have. So something has to say how the pieces become the
record, and today that is the function passed to `struct2` … `struct5`.
Generating it from the type is the answer, `docs/design/schema.md` calls it a
required part of the first version, and it has not landed.

**Records stop at five fields.** Nest a record rather than reaching for a wider
combinator — which is usually what the shape of the data was telling you
anyway.

**Nothing produces a `Raw` yet but a program's own code.** `std::json` has its
own decoders and keeps them until the two share a `Raw`; reading a request body
through a schema means writing that bridge yourself.

## See also

- [`std::schema` API](/docs/stdlib/api/schema/) — every type and function, with
  its signature.
- [Decode untrusted input](/docs/cookbook/decoding-input/) — the whole thing as
  a program, with its output.
- [Load application configuration](/docs/cookbook/configuration/) — what
  `std::config` does today, and still the shortest path for reading settings
  out of the environment.
