# Schema

The decision for #141, revised under #170: one description of a value's shape,
used to decode untrusted input at every boundary the program has, validate it,
and say precisely where it went wrong. `docs/design/schema-derive.md` is the
record of the revision — what was wrong with the first version, the choices
made and the alternatives refused. This document is what the library is now.

> **A schema describes a value. It does not know where the bytes came from.**
> That separation is the whole of the design. The other half is newer:
> **the type is the schema.** A compiled language whose compiler holds every
> declaration does not need the schema written a second time.

## What it is for

Untrusted data arrives at more places than a request body. Environment
variables, the command line, a query string, a database row, a model's answer:
each is text somebody else produced, each has a shape the program expects, and
each used to be read by its own vocabulary — `std::config` had four readers,
`std::json` had `FromJson`, `std::ai` had `Extract`, and `khq` parsed its flags
by hand. Four ways to say "an integer, and here is what was wrong with it".

`std::schema` is the one way. A source hands over a `Raw`, a schema reads it,
and every problem is reported at once, with a path.

## The value and the two traits

```khora
pub type Schema<A> = {
  shape: Shape,
  read: (List<Segment>, Raw) -> Validated<A, Rejection>,
};

pub trait Decode { fn schema() -> Schema<Self>; }
pub trait Encode { fn encode(self) -> Raw; }

pub fn decode<A: Decode>(raw: Raw) -> Validated<A, Rejection>;
```

`read` is the typed half: a closure, so schemas compose, and `schema.decode(raw)`
is an ordinary method call. `shape` is the untyped half, untyped **on purpose**:
with no type parameter it goes in a `List`, which is what lets a record schema
hold fields of different types and still be walked — for the variables a
deployment needs, for a JSON Schema, for which flags are switches.

`Decode` is how a type says what its schema is, and `decode<A: Decode>` is
selected by the expected type, the way `serde_json::from_str` is: `let s:
Settings = decode(raw)`. `Encode` is a separate trait rather than a `write`
half inside `Schema<A>`, and the reason is that the two halves have different
customers. A schema is *asked* things — its shape is walked, its rules
rendered, its failures reported — and an encoder is only ever called.
Effect's bidirectional value is right for a language where the schema is the
only place the type exists; here the type exists, and a type that can be
written to the wire and not read from it, or read and not written, is a
decision the author makes by implementing one trait and not the other.
`Redacted<A>` decodes and refuses to encode, and the build stops at the site
that tried, which is the property that decision exists for.

### Why `Validated` and not `Result`

A configuration with three bad keys reports three bad keys. `Validated`
accumulates; `Result` stops; `Validated::to_result` is one call for a caller
who wants the other behaviour. This is also why a schema is not a function
`(Raw) -> A raises Rejection`: a raise stops at the first problem by
construction, and stopping is the behaviour not wanted by default. Everything
else in `std` raises; the exception is that here the accumulated answer *is*
the value a caller wants to print.

## The type is the schema

```khora
derive(Show, Decode, Encode)
pub type Mode = | Local | Remote(url: String);

derive(Show, Decode)
pub type Settings = {
  /// Where to accept connections.
  listen: Listen,
  password: Redacted<String>,
  debug: Option<Bool>,
  rate: Decimal,
  tags: List<String>,
  mode: Mode,
};
```

Nothing describes the record a second time. `derive(Decode)` is a
source-to-source expansion, like `derive(Show)`: for a record it writes one
`let s_i: Schema<T_i> = Decode::schema();` per field, zips them under their
names with `Fields::of`, and takes the tuple apart into an annotated record
literal. The annotation is how the impl for each field is chosen — the
expansion runs before anything knows what a type is, and the checker resolves
`Decode::schema()` from the `let`'s type exactly as it would for a hand-written
one. The whole thing sits inside `Schema::lazy("Settings", ..)`, so a type that
mentions itself terminates: the schema is built when it is first read, not
when it is constructed.

Every customization is a type. `Option<A>` is an optional field, `List<A>` a
list, `Redacted<A>` a secret whose failures never quote what they saw,
`Decimal` exact, a nested record whatever *its* schema is — derived, or
written by hand with a refined port, and `Settings` picks up whichever. A
variant reads a payload-free case as a bare string and a payload case as an
object tagged `type`; a newtype, `type UserId = Int`, is transparent. The
`///` above the type and above each field is carried into
`Schema::described`, so a JSON Schema rendered from the type says what the
comment says.

Two things a derive refuses, at the `derive` line: a field whose type has no
`Decode`, and a case whose payload has no field names. The wire needs a key,
and a name the type did not declare is not the compiler's to invent.

## The record form: `struct({ .. })`

```khora
let listen: Schema<Listen> = struct({
  port: between(int(), 1, 65535),
  host: default(string(), "127.0.0.1"),
});
```

The first version said this could not be typed, and shipped `struct2` to
`struct5` with assembler closures instead. It could not be typed *as a library
function*: `struct<A>(fields: R) -> Schema<A>` has no way to relate a record of
schemas to the record they decode without mapped types. It is not a library
function. `std::schema` declares it bodiless, `pub fn struct<A>(fields:
Fields<A>) -> Schema<A>;`, and a call to it with a record literal is rewritten
during lowering into `Schema::record` over `Fields::of` zipped in field order
and a closure that builds the literal — `{ port: a0, host: a1 }` — which the
checker types by the rules it already has. The result is a nominal
`Schema<Listen>` when the expected type says so and a structural one when it
does not, and the diagnostics are the record literal's own: ambiguous, missing
a field, or carrying one the type lacks. Only the literal's field values are
blamed on the author's text; every synthesized node is blamed on the call.

The rewrite is judged by where `struct` was imported from, so an alias works.
A pipe into it, or an argument that is not a record literal, is refused with a
sentence saying what the form is, because the call is a spelling for one
expansion and not a function that can be passed around. Errata 76 records how
the first version's argument went wrong.

## Customization without attributes

There are no attributes on fields. What Effect spells as annotations, Khora
spells as types and as ordinary values:

| want | write |
| --- | --- |
| optional field | `Option<A>` |
| a default | `default(string(), "127.0.0.1")` in a `struct`, or a hand impl |
| a renamed key | `key("listen-port", int())` in a `struct`, `renamed` on a schema |
| a rule | `refine(int(), "be even", fn n => n % 2 == 0)`, `between`, `min_length`, `non_empty`, `one_of` |
| a secret | `Redacted<A>` |
| a description | `///` on the type or field, or `Schema::described` |
| no unknown keys | `Schema::closed(schema)` |
| a transform | `Schema::map`, `Schema::try_map` |

A type that needs any of these on a derived field writes `impl Decode` for
that field's type, or for the record, and the derive of everything around it
is unchanged. `Fields<A>` — `of`, `none`, `zip`, `map` — is what both the
derive and `struct` expand to, and is there for the hand-written case that
neither covers.

## `Raw`, absence, and strictness

```khora
pub type Raw =
  | Absent | Null | Text(String) | Untyped(String) | Number(String) | Bool(Bool)
  | Sequence(List<Raw>) | Record(List<Pair<String, Raw>>) | Denied;
```

**`Absent` is not `Null`.** A missing key and an explicit `null` are different
statements, and `Option<A>` accepts the first while `nullable` accepts the
second. A JSON body with `"debug": null` where `debug: Option<Bool>` is
rejected, which is what a caller who meant to omit it wants to hear.

**Strictness is a fact about the source.** JSON labels its values, so a JSON
source hands over `Text` and `Number`, and `int()` refuses `"8080"` with
`port should be a whole number, and is "8080"`, the way serde and
`encoding/json` refuse it. The environment, the command line and a query
string cannot label anything, so they hand over `Untyped`, and every primitive
reads it. `decimal()` reads `Text` as well, because `"10.10"` in a JSON body
is the honest spelling of an amount. Nothing in the schema decides this; the
source records what it knew.

**Numbers carry their text.** `Raw::Number` holds the token, and `int()` and
`decimal()` parse from it, so `9007199254740993` and `10.10` survive. A schema
whose `Decimal` routed through a double would rebuild the bug it exists to
prevent.

**`Denied` is a source's refusal**, not a decoder's: an environment variable
the program's permissions do not grant reaches the schema as `Raw::Denied` and
is reported as `DATABASE_URL is not granted`, distinguishable from unset for
the reason `std::config`'s doc comment gives.

## `Shape`, rules, and the wire

`Shape`'s arms are named after the constructors, one for one — `String`,
`Int`, `Decimal`, `Bool`, `List`, `Dict`, `Struct`, `Cases`, `Optional`,
`Nullable`, `Default`, `Keyed`, `Refined`, `Secret`, `Closed`, `Described`,
`Lazy` — because a reader who knows the constructor knows the arm. A `Rule` is
a shape's account of a refinement: a sentence, and where the rule is one a
JSON Schema can say, the keyword too, so `between(int(), 1, 65535)` is
`minimum` and `maximum` on the way out and `port must be between 1 and 65535`
on the way in.

A variant's wire form is chosen by whether the case carries anything: `Local`
is `"Local"`, and `Remote(url: String)` is `{"type": "Remote", "url": ".."}`.
The tag key is `type`, which is what the JSON a model or a JavaScript client
produces already says; `cases_tagged` takes a different key for a wire that
already exists. In the environment the same variant is `MODE=Remote` and
`MODE_URL=..`.

## Errors, and every sentence they print

```khora
pub type Rejection = { path: List<Segment>, problem: Problem };
pub type Segment = | Field(name: String) | Index(at: Int);
```

`Problem` is the closed set, and each prints one sentence:

| | |
| --- | --- |
| missing | `listen.port is not set` |
| wrong shape | `listen.port should be a whole number, and is "eighty"` |
| refused by a rule | `listen.port must be between 1 and 65535` |
| unknown key, under `closed` | `listen.hots is not expected` |
| a source's refusal | `DATABASE_URL is not granted` |

Text is quoted, numbers are bare, `null` is `null`. Inside a secret the found
value is `<redacted>`: `DATABASE_PASSWORD should be a whole number, and is
<redacted>`. This is the easiest imaginable way to put a password in a log,
and `secret` is responsible for the wrapping so that no caller can forget.
`Rejection::report` joins the lines, and `Rejection` encodes as
`{path, message}`, so an API can answer with the same sentences a log gets.

A source may describe the path its own way: `std::config` prints
`LISTEN_PORT`, because that is the name the operator will search for.

## Every boundary, one path

| source | how a `Raw` is made | what reads it |
| --- | --- | --- |
| JSON body | `Raw::of_json(parse(text))` | `decode`, or `Request::json` |
| environment | `std::config::read(schema)` names each variable from the shape | the same schema |
| command line | `Raw::of_arguments_for(shape, args)`; a `Bool` field is a switch | `struct` or a derived record |
| query, headers, params | `Raw::of_map` | the same |
| database rows | `Row::to_raw`, `Row::sequence` | `list(Entry::schema())` |
| a model's answer | `ai::llm::extract<A: Decode>` renders the shape into the prompt | the derived schema |

`std::config` is a source and nothing else now: `read(schema)`, `variables(shape)`
for the names a deployment needs before the program starts, `describe` and
`report`. The shape decides the names — a nested record's field is
`LISTEN_PORT`, a list is split on commas, `key` renames a segment — and a
nested optional record is present when any of its variables is.

## JSON Schema

`Shape::to_json_schema` renders a shape as a draft 2020-12 document: a derived
type is a `$defs` entry and a `$ref`, which is what terminates a type that
mentions itself; a rule is its keyword; a secret is `writeOnly`; an optional
or defaulted field is left out of `required`; a variant is an `enum` or a
`oneOf` over the two forms the decoder reads; a description is `description`.
It is what a model is prompted with and what an API is documented by, and it
is rendered from the shape the decoder uses, so the two cannot disagree.

## What is deliberately not here

**No `unknown`.** `Raw` is the universal tree and it is closed, which is
narrower than a dynamic type and more honest.

**No attributes.** Everything an attribute would say is a type or a value, and
a type can be asked questions an attribute cannot.

**No positional payloads on the wire.** A case `Pair(Int, Int)` has no names,
and the wire needs keys; the derive refuses it rather than inventing `_0`.

**No bidirectional value.** Two traits, above. A type whose encoder and
decoder must agree is tested for it, the way a hand-written `Show` is.

**No async decoding.** A refinement that reaches a database is a capability
row on the refinement's closure, which the combinators carry, and needs
nothing from this design.

## What shipped first

#141 shipped a `Schema<A>` with the shape-and-closure representation kept
here, and three things this revision replaced. The constructors were named
`text`, `whole`, `exact` and `truth`, four invented words for four types the
language already named; errata 73 has that story, and the rule since is that
a constructor is named after the type it answers. The record forms were
`struct2` to `struct5` with assembler closures, under an argument that the
literal form could not be typed; errata 76 has that one. And `std::json`,
`std::config` and `std::ai` each kept a reader of their own beside the schema,
so that three vocabularies stood where one was meant to.

Two things were in the way of the first version and are still worth naming,
because a schema library that could not compose with the failure system would
have rebuilt the exact gap they closed: until #136 the collection combinators
refused a closure that raises, and until #137 a `catch` arm could not bind the
failure value.
