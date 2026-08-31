# Schema

The decision for #141: one description of a value's shape, used to decode
untrusted input, validate it, and say precisely where it went wrong.

> **A schema describes a value. It does not know where the bytes came from.**
> That separation is the whole of the design, and it is the one thing
> `std::config` does not have.

## What is wrong with what exists

`std::config` reads settings and reads them well. Its four readers are:

```khora
pub fn string(name: String) -> Validated<String, ConfigError> with { env: Env }
pub fn integer(name: String) -> Validated<Int, ConfigError> with { env: Env }
pub fn boolean(name: String) -> Validated<Bool, ConfigError> with { env: Env }
pub fn secret(name: String) -> Validated<Redacted<String>, ConfigError> with { env: Env }
```

Read the signature rather than the name. `string(name)` is not "this field is a
string" — it is *go to the environment, fetch this variable, and give me the
text or a reason*. The description of the shape and the act of reading are one
function, and that is why none of it is reusable. A JSON request body wants the
same four questions asked of different bytes; so does a CLI argument, a
database row, a TOML file. Today each of those either invents its own
vocabulary or has none.

Three consequences follow, and all three were reported from outside:

- `secret` can only describe a secret **string**. It is a leaf, so there is no
  `secret(integer())` and no way to redact a whole record.
- The set of shapes is closed. Adding "a port, which is an integer between 1
  and 65535" means adding a function to `std::config`.
- Nothing can be asked of a configuration before it is read — not "which
  variables does this program need", which is the question a deployment wants
  answered without starting the program.

## The shape of the answer

Two layers, and the boundary between them is the point.

**A `Schema<A>` describes an `A`.** It is a value, it composes, and it knows
nothing about the environment, a socket or a file.

**A source produces a `Value`.** `Value` is a universal tree — text, numbers,
booleans, lists, records, nothing. Reading the environment produces one;
parsing JSON produces one; so does walking a query result.

**Its numbers carry text, not a `Float`, and this is the one place the design
cannot reuse what exists.** `std::json`'s `Json` is the same shape and its
`Number` case holds a `Float`, which means `9007199254740993` parses back as
`9007199254740992` and `10.10` can never be recovered exactly — measured, and
filed as #142. A schema library whose `Decimal` decoder routed every amount
through a double would rebuild, inside itself, the exact class of bug it exists
to prevent. So `Value::Number` holds the token's text and each numeric schema
parses from it with `Int::of_string` or `Decimal::of_string`, which is what
`std::config` already does and why `std::config` is exact.

If #142 lands first, `Json` and `Value` converge and one of them can go. If it
does not, `Value` stands alone and the JSON source parses text itself. Either
way this library does not inherit the lossy one.

Decoding is then one function over the two:

```khora
pub fn decode<A>(schema: Schema<A>, from: Value) -> Validated<A, DecodeError>
```

Configuration becomes a source plus a schema rather than a family of readers,
and the same schema decodes a request body without knowing it has been reused.

### Why `Validated` and not `Result`

Because `std::config` is right about this and it is the property worth keeping:
a configuration with three bad keys should report three bad keys, not the first
one. `Validated` accumulates; `Result` stops. `decode` therefore answers a
`Validated`, and `decode_or_stop` is the fail-fast form for a caller who has
one field and no use for a list.

This is also why a schema is not simply a function `(Value) -> A raises
DecodeError`. A raise stops at the first problem by construction, and stopping
is the behaviour we do not want by default.

### Why not raise

Everything else in `std` raises, so the exception wants a reason. It is that
the accumulating answer *is* the value here: a caller almost always wants to
print all of it. A raise would force `attempt` at every call site to get back
the thing the caller wanted in the first place. `Validated::to_result` is one
call for anyone who disagrees.

## The representation

**A tree, not a closure.** The obvious implementation is a record of two
closures, `decode` and `encode`, and it is wrong for one reason: a closure
cannot be asked anything. A schema has to answer questions other than "decode
this" —

- which keys does this need (a deployment listing its variables);
- what does this look like as a JSON Schema or an OpenAPI fragment;
- what does the documentation generator print for this request body.

So the core is a variant, and the combinators are its constructors:

```khora
pub type Schema<A> =
  | Text
  | Whole
  | Exact                                     // Decimal
  | Truth
  | Nothing                                   // ()
  | List_(of: Schema<A>)
  | Struct_(fields: List<FieldSchema>)
  | Optional(inner: Schema<A>)
  | Choice(cases: List<CaseSchema>)
  | Refined(inner: Schema<A>, ..)
  | Transformed(inner: Schema<A>, ..)
  | Secret(inner: Schema<A>);
```

Closures appear only where they must — in `Refined` and `Transformed`, which
carry a user predicate and a user conversion. Everything structural stays
inspectable. That is the same split Effect.Schema arrives at, and for the same
reason.

The exact spelling of the payloads is left to the implementation; what this
document fixes is that structure is data and only the two escape hatches are
functions.

## Secret is a combinator

The point you raised, and the clearest single improvement over what exists:

```khora
pub fn secret<A>(inner: Schema<A>) -> Schema<Redacted<A>>
```

`secret(integer())`, `secret(text())` and `secret(struct_(..))` all work, and
redaction composes with everything else instead of sitting beside it.

**No bound is needed on `A`, and that is worth saying because it looks as
though one should be.** A bound would be required if a schema were a type-level
thing and `Schema<A>` a constraint on `A`. It is not: `Schema<A>` is a *value*,
and holding one is already the evidence that `A` can be decoded. There is
nothing for `A: Schema` to add. If the implementation ever finds it needs a
bound, that is a signal the representation drifted toward the type level and
should be pulled back.

### Redaction has to survive into the error

A decode failure quotes what it found — that is most of a decode error's value:

```text
listen.port should be a whole number, and is `eighty`
```

Inside a `Secret`, it must not. This is the easiest imaginable way to put a
password in a log, and it is invisible until it happens once in production.

So `DecodeError` carries the found text as a `Redacted<String>` under a secret
and a plain `String` outside one, and `Secret`'s decoder is responsible for the
wrapping. A failure inside a secret reads:

```text
DATABASE_PASSWORD should be a whole number, and is <redacted>
```

The existing `Redacted` design is kept exactly: its `Show` prints a placeholder
rather than being absent, on the argument that an unprintable containing record
makes people stop wrapping the secret. Nothing here weakens that.

## Where it went wrong: the path

`ConfigError` names a variable, which is a path in a flat namespace. A nested
value needs a real one:

```khora
pub type Step = | Field(name: String) | Index(at: Int);
pub type DecodeError = { path: List<Step>, problem: Problem };
```

`Problem` is the closed set — missing, wrong shape, refused by a refinement, no
matching case in a union — and a refinement carries its own message, so "must
be between 1 and 65535" comes from the schema that imposed it rather than from
a generic complaint.

Rendering a path is `listen.port` and `items[3].id`, which is what a person
reading a log needs and what `ConfigError::describe` already does for one
level.

## Deriving a schema from a type

Effect.Schema derives the *type* from the schema, because TypeScript's
conditional types can do that and its users would otherwise write everything
twice. Khora should go the other way and derive the **schema from the type**:

```khora
derive(Show, Schema)
pub type Listen = { host: String, port: Int };
```

The language already has `derive` for `Show`, `Eq` and `Ord`, so this is an
existing mechanism rather than a new one, and it is the direction that suits a
language where the type is written first. A hand-written schema stays available
for everything derivation cannot know — a refinement, a renamed key, a
transform.

Whether `derive(Schema)` lands with the first version or after it is a
sequencing question, not a design one. The library has to work when written by
hand either way.

## What this replaces

`std::config` keeps its shape and loses its vocabulary:

```khora
pub fn read<A>(schema: Schema<A>) -> Validated<A, ConfigError> with { env: Env }
```

`string`, `integer`, `boolean`, `decimal` and `secret` become the corresponding
schema constructors, and the four env-reading functions become one. The
`ConfigError` variants stay reachable — `Denied` in particular is not a decode
problem but a permissions one, and it has to stay distinguishable from
`Missing` for the reason its own doc comment gives.

Migration is mechanical and the cookbook recipe changes shape rather than
length.

## Why this can be built now and could not be before

Two things were in the way and both are gone.

**A refinement may fail.** Until #136 the collection combinators refused a
closure that raises, so a schema whose refinement called a fallible user
function could not be written without hand-rolling every walk. The combinators
carry the caller's rows now.

**A caller can read the failure.** Until #137 a `catch` arm could not bind the
failure value, so folding a many-variant decode error into one answer meant one
arm per variant. A binding arm handles it in one.

A schema library that could not compose with the failure system would have
rebuilt the exact gap those two closed, which is why this was sequenced behind
them.

## What is deliberately not here

**Bidirectionality is in the design and not in the first version.** Effect.Schema
describes both directions with one value, and that is right — an encoder and a
decoder that disagree is a bug the type system should not permit. But the
demand is decode-shaped: untrusted input arriving at a boundary. The variant
above has room for `encode` and the first version should not ship it half-done.

**No `unknown`.** Khora has no dynamic type and does not want one. `Value` is
the universal tree and it is closed, which is a narrower and more honest thing.

**No async decoding.** A refinement that hits a database is a capability row on
the refinement's closure, which the combinators now carry, and needs nothing
from this design.
