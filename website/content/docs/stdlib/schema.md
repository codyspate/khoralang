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
lets schemas be combined — `optional(list(int()))` is three closures wrapped
around each other.

The **shape** is untyped, deliberately: it has no type parameter, which is what
lets a record's fields — all of different types — sit in one `List` and be
walked. That is how a deployment can ask which keys a configuration needs
without starting the program:

```khora
print("it needs: ${Shape::keys(schema().shape)}");
```

A record of two closures would have lost the shape. A bare description tree
would have lost the decoder. [the schema design note](https://github.com/codyspate/khoralang/blob/main/docs/design/schema.md) has the argument in full.

## The vocabulary

Six primitives, the combinators that wrap another schema, and the assemblers.
Every constructor is named after the type it answers.

| | |
| --- | --- |
| [`string()`](/docs/stdlib/api/schema/#string) | a `String`; a number is not text, and is refused |
| [`int()`](/docs/stdlib/api/schema/#int), [`float()`](/docs/stdlib/api/schema/#float) | an `Int` or a `Float`, parsed from the token |
| [`decimal()`](/docs/stdlib/api/schema/#decimal) | a `Decimal`, parsed from the token, or from text, because money travels as a string on most wires |
| [`bool()`](/docs/stdlib/api/schema/#bool) | a `Bool` |
| [`any()`](/docs/stdlib/api/schema/#any) | the `Raw` as it arrived, for a field whose shape is somebody else's business |
| [`optional(s)`](/docs/stdlib/api/schema/#optional) | an `Option<A>`; absent or `null` is `None`, present and wrong is still an error |
| [`nullable(s)`](/docs/stdlib/api/schema/#nullable) | an `Option<A>` that must be present and may be `null` |
| [`default(s, value)`](/docs/stdlib/api/schema/#default) | an `A`, with `value` when the field is absent; `null` is still an error |
| [`list(s)`](/docs/stdlib/api/schema/#list) | a `List<A>`, indexing each element into the error path |
| [`dict(s)`](/docs/stdlib/api/schema/#dict) | a `Dict<String, A>`, for a record whose keys are data |
| [`refine(s, must, holds)`](/docs/stdlib/api/schema/#refine) | the same `A`, rejected unless `holds`; `must` is the sentence the message uses |
| [`between`](/docs/stdlib/api/schema/#between), [`at_least`](/docs/stdlib/api/schema/#at_least), [`at_most`](/docs/stdlib/api/schema/#at_most), [`min_length`](/docs/stdlib/api/schema/#min_length), [`max_length`](/docs/stdlib/api/schema/#max_length), [`min_items`](/docs/stdlib/api/schema/#min_items), [`max_items`](/docs/stdlib/api/schema/#max_items), [`non_empty`](/docs/stdlib/api/schema/#non_empty), [`one_of`](/docs/stdlib/api/schema/#one_of) | the same, with a rule that carries its bounds, so a rendered document can carry them too |
| [`secret(s)`](/docs/stdlib/api/schema/#secret) | a `Redacted<A>`, and a failure inside it does not quote what it saw |
| [`key(wire, s)`](/docs/stdlib/api/schema/#key) | the same `A`, read under a key that is not the field's name |
| [`renamed(s, wire, field)`](/docs/stdlib/api/schema/#renamed) | a record schema with one key spelled differently on the wire |
| [`Schema::map`](/docs/stdlib/api/schema/#map), [`Schema::try_map`](/docs/stdlib/api/schema/#try_map) | something made from the value, or something it may fail to become |
| [`Schema::closed`](/docs/stdlib/api/schema/#closed) | the same record, refusing a key it did not declare |
| [`Schema::described`](/docs/stdlib/api/schema/#described) | the same schema, with a sentence for a document to carry |
| [`Schema::cases`](/docs/stdlib/api/schema/#cases) | a variant: a bare string for a payload-free case, an object tagged with `type` for the rest |
| [`Schema::lazy`](/docs/stdlib/api/schema/#lazy) | a schema built when first read, so a type may mention itself |
| [`struct2`](/docs/stdlib/api/schema/#struct2) … [`struct5`](/docs/stdlib/api/schema/#struct5) | a record of that many named fields |

They compose in the obvious way, which is most of the point:

```khora
between(int(), 1, 65535)
```

## Where a `Raw` comes from

`Raw::of_json` turns a parsed document into one, and `Raw::to_json` turns one
back, which is the one bridge in each direction. `Raw::of_map` takes a query
string, the path parameters or the headers; `Raw::of_arguments` takes the
command line, so `--port 8080` is a field named `port`.

A source that cannot label its values, which is every one of those but JSON,
hands over `Raw::Untyped`, and every primitive reads it. A source that can
label them is taken at its word: `"port": "8080"` in a JSON body is text, and
`int()` refuses it with `port should be a whole number, and is "8080"`, the
way serde and `encoding/json` refuse it. Strictness is a fact about the
source, recorded by the source.

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
  rate should be an exact decimal, and is "about seven percent"
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
public should be a whole number, and is "not a number"
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
Generating it from the type is the answer, [the schema design note](https://github.com/codyspate/khoralang/blob/main/docs/design/schema.md) calls it a required part of
the first version, and it has not landed.

**The assemblers stop at five fields.** `Schema::record` over `Fields` has no
such limit — `Fields::zip` nests a tuple, however many fields there are — but
nesting a record is usually what the shape of the data was telling you anyway.

**`std::json` still has decoders of its own.** `Raw::of_json` is the bridge
from a parsed document to a schema; `FromJson` and `ToJson` remain beside it
for now.

## See also

- [`std::schema` API](/docs/stdlib/api/schema/) — every type and function, with
  its signature.
- [Decode untrusted input](/docs/cookbook/decoding-input/) — the whole thing as
  a program, with its output.
- [Load application configuration](/docs/cookbook/configuration/) — what
  `std::config` does today, and still the shortest path for reading settings
  out of the environment.
