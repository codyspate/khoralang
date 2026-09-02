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

## Reaching a schema through the type

A type that can be read from untrusted input implements `Decode`, whose one
function is `schema() -> Schema<Self>`. `std` implements it for its own
types — `String`, `Int`, `Float`, `Bool`, `Decimal`, `Date`, `Time`,
`DateTime`, `Option<A>`, `List<A>`, `Vector<A>`, `Dict<String, V>`,
`Map<String, V>`, `Redacted<A>` (as `secret`), `Json` and `Raw` — and a
program implements it for a record or a variant with the constructors above:

```khora
impl Decode for Listen {
  fn schema() -> Schema<Listen> {
    struct2("host", string(), "port", between(int(), 1, 65535), listen)
  }
}
```

Every schema that contains a `Listen` then reaches this one through the trait,
which is the whole override story: write the impl for the type that needs
something derivation cannot know, and composition does the rest. The schema is
reached by name, `Listen::schema()`, or by the type the surrounding expression
asks for:

```khora
let settings: Validated<Settings, Rejection> = decode(raw);
```

`Encode` is the other direction, `encode(self) -> Raw`, and `Raw::to_json` is
the bridge out. It is a separate trait rather than a second half of the schema
because a secret has no representation on the wire: `Redacted` implements
`Decode` and not `Encode`, so a record holding one reads and does not write,
and the build says so. `Rejection` implements `Encode`, so a list of problems
is a response body a client can read: one object per problem, with its `path`
and its `message`.

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

## The declaration is the schema

Effect writes the schema and recovers the type, because TypeScript erases
types. Khora's compiler holds every declaration, so the direction reverses:
`derive(Decode)` writes the schema from the type.

```khora
derive(Show, Decode, Encode)
pub type Mode = | Local | Remote(url: String);

derive(Show, Decode)
pub type Settings = {
  listen: Listen,
  password: Redacted<String>,
  debug: Option<Bool>,
  rate: Decimal,
  tags: List<String>,
  mode: Mode,
};
```

Nothing describes the record a second time. Every customization is a type:
`Option<A>` is optional, `List<A>` is a list, `Redacted<A>` is a secret whose
failures never quote what they saw, `Decimal` is exact, a nested type is
found through the trait — so `Listen` above may derive its schema or write
one by hand with a refined port, and `Settings` picks up whichever it is. A
variant derives to a bare string for a payload-free case and an object tagged
with `type` for the rest; a newtype, `type UserId = Int`, is transparent; a
type that mentions itself needs nothing written, because a derived schema is
built when it is first read.

Two things a derive refuses, at the `derive` line: a field whose type has no
`Decode`, and a case whose payload has no field names, because the wire needs
a key and a name the type did not declare is not the compiler's to invent.

## The record literal

The hand-written form is the record's own literal with a schema where each
value would go:

```khora
impl Decode for Listen {
  fn schema() -> Schema<Listen> {
    struct({ host: string(), port: between(int(), 1, 65535) })
  }
}
```

Which record it is comes from the type the expression is asked for — the
declared return type here, an annotation or a parameter elsewhere — or from
the labels alone when only one record has them, the way any record literal
resolves. A field whose schema decodes the wrong type is reported at that
schema; a field the record does not have is reported at the call.

`struct` is not a function that runs. Its argument is a record of *schemas*
and its result a schema of the record they decode, and there is no type-level
map from one to the other; a call to it is rewritten before it is typed into
`Schema::record` over `Fields`, which is what a derived schema is too. So it
takes a record literal and nothing else, there is no arity, and a record with
a hand-written schema is picked up by every schema that contains it.

**The assemblers stop at five fields.** `Schema::record` over `Fields` has no
such limit — `Fields::zip` nests a tuple, however many fields there are — but
nesting a record is usually what the shape of the data was telling you anyway.

**`std::json` parses and prints, and does not decode.** `parse` turns text
into a `Json` and `Raw::of_json` turns that into what a schema reads;
`Raw::to_json` and `encode` are the way back. There is no second decoding
vocabulary beside this one.

## See also

- [`std::schema` API](/docs/stdlib/api/schema/) — every type and function, with
  its signature.
- [Decode untrusted input](/docs/cookbook/decoding-input/) — the whole thing as
  a program, with its output.
- [Load application configuration](/docs/cookbook/configuration/) — what
  `std::config` does today, and still the shortest path for reading settings
  out of the environment.
