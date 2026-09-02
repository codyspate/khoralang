# Schema: the type is the schema

**Status: proposed, nothing here is built.** This note plans the second
version of `std::schema` and supersedes the "representation", "deriving a
schema from a type" and "what this replaces" sections of `schema.md` once it
lands; until then `schema.md` describes what exists. Written 2026-09-01 from a
read of the compiler, the standard library, the docs and Effect's Schema
module, with four independent designs judged against each other and the
result checked by three adversarial reviews. Every claim below about what the
compiler does today was reproduced by compiling a probe program on this tree.

> **The declaration is the schema.** Effect writes the schema and recovers the
> type because TypeScript erases types. Khora's compiler holds every type's
> structure, so it can write the checker that runs at the boundary. The
> direction reverses, and most of Effect's surface goes with it.

## What is wrong with what exists

`std::schema` is the right idea with the wrong record form, and it is one
vocabulary among four.

**The record form is an assembler family.** `struct2` through `struct5` take
names, schemas and a function that puts the pieces back together:

```khora
struct4(
  "listen", struct2("host", string(), "port", port(), listen),
  "password", secret(string()),
  "rate", decimal(),
  "debug", optional(bool()),
  settings,
)
```

Nobody reads that correctly the first time. The names are strings, the
assembler is a function declared elsewhere, the arity is in the function name,
and it stops at five because `Validated::map5` does. The spelling a reader
reaches for is `struct({ port: int(), host: string() })`, and the design note
argued it cannot be typed. That was true of a *library function*: its argument
is a record of schemas and its result a schema of the record they decode, and
Khora has no type-level map from one to the other. It is not true of the
language, which is allowed to rewrite a call before typing it (§4).

**`derive(Schema)` was called required and never built.** Without it every
record schema is written twice: once as the type, once as the assembler.

**Four vocabularies decode untrusted input.** `std::json` has `FromJson`,
`ToJson` and `DecodeError` (first problem only, path as strings, found value
unredacted). `std::config` has five environment readers and `ConfigError`.
`std::ai` has `Extract` with an associated `Spec` and a `parse`. `std::db`
hands over positional `Row`s that every caller walks by index. The two
example services use none of the schema library: they hand-walk `Json::field`
into nested `Option` matches, and `ledger_service` silently drops a row that
does not decode.

## The plan in one paragraph

`Schema<A>` stays what it is, a value holding an untyped `Shape` and a typed
`read`. Two traits join it: `Decode`, whose one function is
`fn schema() -> Schema<Self>`, and `Encode`, whose one function is
`fn encode(self) -> Raw`. Both are derivable, and `derive(Decode)` is the
primary way to get a record or variant schema: the compiler writes it from the
declaration. The hand-written form is `struct({ host: string(), port: .. })`,
a call the compiler rewrites into ordinary calls before typing, so the literal
inside resolves against the declared record the way any record literal does.
`struct2`..`struct5` are deleted. `Raw` gains `Null`, `Untyped` and `Denied`;
`Shape` gains the arms a variant, a float, a dictionary, a rule, a wire key, a
default and a recursive type need; every source in `std` produces a `Raw` and
every boundary decodes through one `Schema<A>`. `FromJson`, `ToJson`,
`DecodeError`, the config readers and `Extract` are deleted.

## Decisions for the owner

Everything below is decided in this note under the repository's
decide-and-flag rule except these five, which either change the language
surface or reverse a recorded decision. Each has a recommendation.

1. **`struct({ .. })` as a compiler-known call** (§4). It is the spelling that
   was asked for, and the only way to have it without mapped types. The change
   is one rewrite in HIR lowering, no new syntax and no new typing rule.
   *Recommended.* A naming sub-question: Khora's own word for `type X = { .. }`
   is *record*, and the rewrite's target is `Schema::record`; `record({ .. })`
   would follow the rule that a constructor is named after the type it
   answers, at the cost of a TypeScript reader briefly hearing `Record<K, V>`.
   `struct` is Effect's spelling and Rust's nominal word. *Keep `struct`
   unless the owner prefers the rule.*
2. **Encoding as a separate `Encode` trait rather than a `write` half inside
   `Schema<A>`** (§7). A bidirectional value cannot hold a secret: `secret()`
   has no write half, so the value would either omit the key at run time or
   make every encoder fallible. The separate trait keeps the compile-time
   refusal `std::core`'s `Redacted` documents. *Recommended.*
3. **Strict primitives** (§5). `int()` stops accepting `"8080"` from a JSON
   body; the environment keeps working because its values arrive `Untyped`.
   This is what serde, `encoding/json` and Effect do. *Recommended.*
4. **The variant wire format** (§6): a payload-free case is a bare string and
   a payload case is an object tagged with `type`, replacing
   `derive(ToJson)`'s positional `{ "case": .., "fields": [..] }`.
   *Recommended.*
5. **Deleting `Extract`** (§9) in favor of `extract<A: Decode>`; it removes the
   one associated-type example in `std`. *Recommended.*

## 1. The value and the two traits

```khora
pub type Schema<A> = {
  shape: Shape,
  read: (List<Segment>, Raw) -> Validated<A, Rejection>,
};

impl<A> Schema<A> {
  pub fn decode(self, from: Raw) -> Validated<A, Rejection>;
  pub fn decode_or_stop(self, from: Raw) -> Result<A, List<Rejection>>;
  pub fn map<B>(self, f: (A) -> B) -> Schema<B>;
  pub fn try_map<B>(self, wanted: String, f: (A) -> Option<B>) -> Schema<B>;
  pub fn record(fields: Fields<A>) -> Schema<A>;
  pub fn cases(cases: List<Case<A>>) -> Schema<A>;
  pub fn cases_tagged(tag: String, cases: List<Case<A>>) -> Schema<A>;
  pub fn case<F>(name: String, fields: Fields<F>, make: (F) -> A) -> Case<A>;
  pub fn lazy(name: String, build: () -> Schema<A>) -> Schema<A>;
}

/// A type that can be read from untrusted input.
pub trait Decode {
  /// The schema for `Self`, chosen by the type the call is asked for.
  fn schema() -> Schema<Self>;
}

/// A type with one representation on the wire.
pub trait Encode {
  fn encode(self) -> Raw;
}

/// Selected by the annotation: `let s: Validated<Settings, Rejection> = decode(raw);`
pub fn decode<A: Decode>(from: Raw) -> Validated<A, Rejection>;
```

**Why the value keeps its name and the trait is `Decode`.** A trait and a type
cannot share a name in one module (`khora-hir` keys duplicates on the name
across kinds), and `Schema<A>` is the name every page and sentence already
uses; `Settings::schema()` answering a `Schema<Settings>` is what the docs
promise. `derive(Decode, Encode)` is the pair a Rust reader predicts from
`Deserialize, Serialize`, and `A: Decode` in a bound says what it means.
`Codec<A>` for the value with `Schema` as the trait was considered (it is
Effect v4's own word) and rejected: the rename would touch every annotation
for no gain in meaning.

**Selection is by expected type.** `Decode::schema()` with `Self` left open is
solved by the surrounding expression, the way `Applicative::pure` and
`json::decode` already are; `Settings::schema()` finds the impl by head; and
`Int::schema()` resolves because `type_of_trait_item` now searches impls by
head for any owner (the roadmap note saying `Int::show(x)` cannot resolve is
stale). All three were run end to end on this tree, including through
`impl<A: Decode> Decode for Option<A>`, a generic `Box<A>`, and the partially
concrete `impl<V: Decode> Decode for Map<String, V>`.

**Three spellings, one rule.** The constructor functions (`int()`,
`string()`, `list(..)`) are the hand-written vocabulary and what goes inside a
`struct` literal; `T::schema()` is how a user type is reached; `Decode::schema()`
under a `let` annotation is what generated code writes, and a person never
needs it.

**`std` implements `Decode` for** `String`, `Int`, `Float`, `Bool`, `Decimal`,
`Date`, `Time`, `DateTime`, `Json` and `Raw` (passthrough, shape `Any`),
`Option<A>`, `List<A>`, `Vector<A>`, `Dict<String, V>`, `Map<String, V>`, and
`Redacted<A>` (whose body is `secret`). `Encode` for the same set except
`Redacted`, plus a hand-written `Encode for Rejection` (§8) so a list of
problems can be a response body. Every impl lives in `std::schema` beside the
trait, which is what the orphan rule will want when it is enforced.

## 2. What `derive(Decode)` writes

`Decode` and `Encode` join `DERIVABLE`, replacing `ToJson` and `FromJson`;
`method_of` answers `schema` and `encode`, so one function per trait still
holds. The expander runs before any type is known and sees a field's type only
as text, so it selects each field's impl the way `derive(FromJson)` does: with
an annotated `let`. For

```khora
derive(Show, Decode)
pub type Listen = { host: String, port: Port };
```

it writes

```khora
impl Decode for Listen {
  fn schema() -> Schema<Listen> {
    Schema::lazy("Listen", fn () => {
      let host: Schema<String> = Decode::schema();
      let port: Schema<Port> = Decode::schema();
      Schema::record(Fields::map(
        Fields::zip(Fields::of("host", host), Fields::of("port", port)),
        fn t => {
          let (a0, a1) = t;
          let built: Listen = { host: a0, port: a1 };
          built
        }))
    })
  }
}
```

Four things about that text are load-bearing.

**Every helper is reached as `Owner::name`.** Generated text resolves in the
deriving file's scope, and a bare name resolves to the file's own item before
an imported one, so a companion called `field` or `record` would be captured
silently by a user's function of that name (the schema test harness itself
declares `fn field`). `Schema::..`, `Fields::..`, `List::..` and
`Decode::schema()` cannot be captured that way. The companions
`bring_derive_companions` pulls from `std::schema` for `Decode` are therefore
three type names, `Schema`, `Fields` and `List`; for `Encode` they are `Raw`,
`List` and `Pair`, because an encoded record entry is the literal
`{ key: name, value: self.f.encode() }`.

**Accumulation is a `zip` chain, never `return` inside a lambda.**
`Fields::zip` nests tuples through a new `Validated::zip(self, other) ->
Validated<(A, B), E>`, so a record of any width is one chain and a tuple `let`
unpacks it; a thirty-field chain was compiled and run. `Validated::zip`
answers a tuple where `List::zip` answers a `Pair`, and its doc says so.
`Validated` keeps `map2`..`map5`; there is no `map6`. The alternative, an
early `return` from the read closure, does not type-check today: the checker
types `Expr::Return` against the *enclosing function's* declared return
(`khora-types/src/check/expr.rs`, the `Expr::Return` arm), while HIR
(`body/lambda.rs`, "`return` leaves the lambda") and codegen treat it as
leaving the lambda. That disagreement is a bug on its own and is fixed first
(§11), but the derive does not depend on it.

**The built literal is annotated.** Two declared records with the same labels
make an unannotated literal ambiguous even under a return type, because the
function's return type is not armed as a hint for the body root. The
annotation costs nothing and removes the dependence.

**Everything sits inside `Schema::lazy`.** `lazy(name, build)` has shape
`Lazy(name, thunk)` and a `read` that calls `build()` when it runs, so
constructing a derived schema never recurses. A self-recursive `Tree`, and two
types that mention each other, derive with nothing written (both were run),
and the type's name is the JSON Schema `$defs` identifier for free. The cost
is that a record's field schemas are rebuilt once per record decoded, bounded
by the size of the input; if a profile ever shows it, the derive can emit
eager construction for a type whose fields name no user type and keep `lazy`
for the rest.

**Variants** derive to `Schema::cases`:

```khora
derive(Show, Decode)
pub type Mode = | Local | Remote(url: String);

// writes
Schema::lazy("Mode", fn () => {
  let remote_url: Schema<String> = Decode::schema();
  Schema::cases(List::Cons(
    Schema::case("Local", Fields::none(), fn _u => Mode::Local),
    List::Cons(
      Schema::case("Remote", Fields::of("url", remote_url), fn a0 => Mode::Remote(a0)),
      List::Nil)))
})
```

A **newtype** `type UserId = Int` is a one-case variant named after itself to
`shape_of`, and derives transparently: `let inner: Schema<Int> =
Decode::schema(); Schema::map(inner, fn a0 => UserId(a0))`. A **generic** type
gets `impl<A: Decode> Decode for Box<A>` and reads `A`'s schema through the
same annotated `let`, which is the one form that works because impl-level
generics cannot be `::` path owners.

**Refusals**, all at the `derive` clause: a field whose type has no `Decode`
("`derive(Decode)` needs every field to implement `Decode`, and the field
`inbox` has type `Channel<Int>`, which does not", the existing per-field
check, no change needed); a case with a positional payload ("the payload of
`Remote` has no name, and the wire needs a key; declare it as
`Remote(url: String)`"); a const or row parameter, as today. The import hint
stops saying `std::core` for every trait and gains a per-trait home table.

`derive(Encode)` is the mirror: `Raw::Record` of `{ key: "host", value:
self.host.encode() }` entries for a record; for a payload case a `Raw::Record`
whose first entry is `{ key: "type", value: Raw::Text("Remote") }` followed by
the payload entries; `Raw::Text("Local")` for a payload-free case. A record
holding a `Redacted<A>` is **refused** by the per-field check with no new
code, exactly as `derive(ToJson)` is refused today, which is the property §7
is built to keep.

## 3. Customization without attributes

Khora has no attribute syntax and has refused to add one twice (`#[if(windows)]`
in the FFI note, attributes as a lint hatch in `lint-hatch.md`), so a derived
schema is all-or-nothing and customization lives in the two places a nominal
language has: types and values.

- **A refinement is a newtype.** Unlike Go, Rust and TypeScript, where
  `type Port = Int` declares an alias, in Khora it declares a distinct type:
  `Port(n)` wraps and `match p { Port(n) => n }` unwraps. Its schema is three
  lines:
  ```khora
  impl Decode for Port {
    fn schema() -> Schema<Port> { between(int(), 1, 65535).map(fn n => Port(n)) }
  }
  ```
  Every `Port` that came *through the schema* passed the rule. Honesty about
  the rest: `Port(n)` and `Port::Port(n)` build one unchecked from any module,
  because Khora has no private constructor; a module-only constructor is a
  language decision this note does not make. (The bare `Port(n)` spelling from
  another module today hits "the type of this expression was never worked
  out", a resolution gap worth its own fix; `Port::Port(n)` resolves.)
- **A record with one odd field is written by hand with `struct`** (§4), and
  every type that contains it picks the hand-written impl up through the
  trait. That is the whole override story: derive by default, write the impl
  when derive cannot know, and composition does the rest.
- **A renamed key** is `key("userId", int())` inside a `struct` literal: the
  schema's shape becomes `Keyed("userId", inner)`, `Fields::of` reads the wire
  name off it and looks the value up by that name, and the rejection's path
  says `userId`, because the client only knows the name it sent.
  `renamed(schema, "userId", "user_id")` does the same over a whole derived
  schema when only one key differs (wire name first, field name second).
  `config::read` honors `Keyed` too, which is the override for its variable
  naming convention (§9).
- **A default** is `default(int(), 8080)` inside a literal; it fires on
  `Absent` only. A field given `null` is still an error, which is what serde's
  `default` and Effect's `optionalWith` both do, and it keeps `std::config`'s
  rule that a present and wrong value is never quietly replaced.
- **A secret** is `Redacted<A>` in the type, nothing else.
- **Whole-value rules** compose on the derived value: `Settings::schema().closed()`,
  `refine(Settings::schema(), "..", fn s => ..)`, `.described("..")`.
- **A transform** is `Schema::map` or `Schema::try_map(wanted, f: (A) -> Option<B>)`;
  `impl Decode for Date` is `string().try_map("an ISO 8601 date", Date::of_string)`
  and is the model for every user parse. A failed `try_map` is
  `Wrong(wanted, found)`, so it reads `created_at should be an ISO 8601 date,
  and is "yesterday"`.

As a last, optional step the derive copies each field's `///` into
`Schema::described(schema, text)` before `Fields::of`, and the type's into the
lazy shape, using the trivia walk `khora doc` already has, so a JSON Schema
and an LLM prompt carry the documentation the author wrote anyway. That is the
kind of thing only a compiler that owns the type can offer.

## 4. The record form: `struct({ .. })`

```khora
impl Decode for Listen {
  fn schema() -> Schema<Listen> {
    struct({ host: string(), port: between(int(), 1, 65535) })
  }
}
```

A Go or TypeScript reader should be told one thing on first meeting it: the
literal names an existing declared record, the way every Khora record literal
does; it does not make an anonymous one. `std::schema` declares
`pub type Fields<A>;` and a bodiless `pub fn struct<A>(fields: Fields<A>) ->
Schema<A>;`, so the item exists for `import`, for `khora doc` and for the
unused-import lint (a bodiless `pub fn` and an opaque generic type both compile
outside `std` today, and `struct` is not a keyword). No call to it ever
reaches the checker: in `lower_expr`'s call arm the callee is lowered before
its arguments, and when the callee's origin is `std::schema::struct` (through
`scope.origins`, so an alias counts and a user's own `struct` does not) and
the sole argument is a record literal with no `..base`, the call is rewritten
into HIR nodes of the same shape as the derive's text:

```khora
Schema::record(Fields::map(
  Fields::zip(Fields::of("host", e1), Fields::of("port", e2)),
  fn t => { let (a0, a1) = t; { host: a0, port: a1 } }))
```

The pipe desugaring builds its own calls, so `fields |> struct` is refused
with the same message as a non-literal argument rather than slipping past
the rewrite.

**Static calls only.** `apply` unifies the expected type with the callee's
return before checking the arguments, so `Schema<Listen>` flows into
`Schema::record`, then into `Fields::map`'s result, then into the lambda's
result, and the literal is checked against `Listen`'s declared fields; with a
wrong schema in a field the report is `field port: expected String, found
Int` at that field. A method chain (`Fields::of(..).zip(..).map(..)`) infers
its receiver first and loses the hint; probed, it reports "these fields fit
`Listen` and `Other`" even under an annotation, while the static form resolves
by annotation, in argument position, and inside a `Schema::lazy` thunk. Each
field expression is bound to a hidden local before the lambda, in source
order, so evaluation order and error order are the declaration order.

**Where the expected type comes from.** A `let s: Schema<Listen> = struct(..)`
annotation on the *schema value*; an argument position whose parameter type
is known; or the function's declared return, once the root hint is armed
(§11). Not from a function-typed annotation on a closure that builds it:
`let build: () -> Schema<Listen> = fn () => struct(..)` gets no hint today
because a function type written in a `let` annotation is dropped to `Unknown`
(§11). With no expected type the literal is found by its labels exactly as a
written literal is, and two records sharing the labels is the same ambiguity a
written literal has, reported at the `struct` call.

**A recursive type written by hand wraps the literal:**
`Schema::lazy("Tree", fn () => struct({ children: list(Tree::schema()) }))`.
The rewrite does not add `lazy` itself, and without it a hand-written
recursive schema runs out of stack at construction (probed).

**Diagnostics**, worded as messages about a schema (the rewrite marks the
literal's origin so `infer_record` can tell):

| situation | message, at |
| --- | --- |
| argument is not a record literal, or `fields \|> struct` | "`struct` takes a record literal with one schema per field, such as `struct({ host: string(), port: int() })`", at the argument |
| `{ ..base }` or `{}` | "`struct` cannot take fields from another record; write every field" / "`struct({})` describes no fields" |
| a label twice | "`host` is given twice in this `struct`" |
| no expected type, no record has these labels | "no record type has exactly the fields `host`, `port`; `struct` describes a declared record", at the call |
| no expected type, two records fit | "these fields fit `Listen` and `Other`; say which with `let s: Schema<Listen> = struct({ .. })`", at the call |
| expected `Listen`, `port` missing | "this `struct` for `Listen` is missing `port`", at the call |
| expected `Listen`, extra `debug` | "`Listen` has no field `debug`", at `debug:` |
| `port: string()` against `port: Int` | "`port` of `Listen` is an `Int`, but this schema decodes a `String`", at `string()` |
| `struct` used as a value | "`struct` is not a value; call it with a record literal" |

Every synthesized node carries the range of the `struct(..)` call except the
mention of each `a_i` inside the generated literal, which carries the range of
the user's `e_i`, so a wrong schema is reported at the schema.

**Why this passes the tie-breaker and `union` does not.** `struct` is a call
that accepts only a literal, which is novel; but every misuse is a loud
compile error that names the fix, so a reader who mispredicts is corrected
once. A `union([Mode::Local, Mode::Remote(string())])` literal was designed
and refused because `Mode::Remote(string())` is a constructor call with a
wrong-typed argument that the compiler would *silently* mean something else
by, which is the failure `vision.md` names as the expensive one; it also needs
HIR's `Variant` to learn payload labels and arity across three tables.
Variants are derived, or hand-written with `Schema::cases` when the tags must
differ from the case names. `struct` cannot describe an anonymous record or a
tuple, by design: positional data reaches a schema as a named row.

`Fields<A>` is public because the rewrite needs a target and a package may want
to build a record schema from something that is not source (a database
catalog, say); its doc says it is what `struct` expands to.

## 5. `Raw`, absence, and strictness

```khora
pub type Raw =
  | Absent                    // the key was not there
  | Null                      // the source said "nothing", explicitly
  | Text(text: String)        // the source said "this is text"
  | Untyped(text: String)     // the source could not say (env, argv, query, header)
  | Number(text: String)      // the token, never a Float
  | Bool(value: Bool)
  | Sequence(items: List<Raw>)
  | Record(fields: List<Pair<String, Raw>>)
  | Denied;                   // the source was refused permission to look
```

**`Null` is distinct from `Absent`.** `optional` treats both as `None`, which
is what serde and `encoding/json` do for an optional field (Effect asks for
`nullable: true` first); `nullable(inner)` is the schema for a field that must
be present and may be `null`. A required field given `null` reads `rate
should be an exact decimal, and is null` rather than `rate is not set`, which
is the difference between a client bug and a deployment bug.

**Leniency is a fact about the source, so it lives in `Raw`.** `int()`,
`float()` and `bool()` accept `Number`/`Bool` or `Untyped`, never `Text`;
`string()` accepts `Text` or `Untyped`, never `Number`. A JSON body with
`"port": "8080"` is refused, as serde refuses it, and the message says why:
`port should be a whole number, and is "8080"`, with the quotes, where a
number is written bare. `PORT=8080` decodes, because the environment cannot
label anything and says so. Two exceptions, argued: `decimal()` also accepts
`Text`, because money travels as a string on most wires (Effect's
`BigDecimal`, Go's `shopspring/decimal` and Rust's `rust_decimal` all
serialize it so, for the sake of JavaScript clients); and `bool()` reads
`true`/`false`/`1`/`0` from `Untyped`, dropping `yes`/`no`, settling the drift
between `std::config` and `std::schema` in favor of config's argument that a
reader accepting every spelling accepts a typo. The alternative, a
shape-directed lexer that labels each environment value by the leaf it is
decoding into, was designed and rejected: it has to be right for every leaf
and every future combinator, where `Untyped` needs nothing from anyone.

**`Denied` is a `Raw` arm.** The environment source puts it where a variable
the manifest does not grant would go; `wrong()` turns it into
`Problem::Denied`; `optional` does not swallow it, because a denied optional is
still a line missing from `khora.toml`. Producing the rejection at the source
and dropping the `Missing` the absent value would otherwise raise was the
alternative, and its bookkeeping is easy to get subtly wrong.

**Sources and bridges.** `Raw::of_json(Json)` is total (`Null` stays `Null`,
`Number` keeps its literal, `Object` becomes a `Record` in the order
`Json::entries` gives). `Raw::to_json` is the one bridge out: `Untyped`
becomes `Text`, an `Absent` entry is omitted from an object and written as
`null` inside an array, and `Denied` is never produced by an encoder and
renders as `null`. `Raw::of_map(Map<String, String>)` makes a `Record` of
`Untyped` for queries, path parameters and headers.
`Raw::of_arguments(List<String>)` reads `--k v`, `--k=v` and bare `--flag`
into a `Record`, positionals under `arguments`. `Row::to_raw` and
`Row::sequence` are in §9.

**On the way out**, `Option::None` encodes as `Absent` (an omitted key, `null`
inside a list), and both forms are accepted on the way in. `Decimal` encodes
as `Raw::Text(Decimal::show(d))`: a JSON string, because that is what all
three of the audience's decimal libraries write and the reason `decimal()`
reads text. `Redacted` has no `Encode` at all.

## 6. `Shape`, rules, and the wire format of a variant

```khora
pub type Shape =
  | Any | String | Int | Float | Decimal | Bool
  | List(of: Shape) | Dict(of: Shape)
  | Struct(fields: List<Named>)
  | Cases(cases: List<Alternative>)
  | Optional(inner: Shape) | Nullable(inner: Shape)
  | Default(inner: Shape)
  | Keyed(wire: String, inner: Shape)
  | Refined(inner: Shape, rule: Rule)
  | Secret(inner: Shape)
  | Closed(inner: Shape)
  | Described(inner: Shape, text: String)
  | Lazy(name: String, inner: () -> Shape);

pub type Named = { name: String, shape: Shape };
pub type Alternative = { name: String, fields: List<Named> };
pub type Rule =
  | Custom(must: String)
  | Between(low: String, high: String) | AtLeast(bound: String) | AtMost(bound: String)
  | MinLength(n: Int) | MaxLength(n: Int) | MinItems(n: Int) | MaxItems(n: Int)
  | OneOf(allowed: List<String>);
```

**Every arm is named after the constructor that builds it, one for one**, the
rule the last rename fixed: `any`, `string`, `int`, `float`, `decimal`,
`bool`, `list`, `dict`, `struct`, `cases`, `optional`, `nullable`, `default`,
`key`, `refine` and its eight rule constructors, `secret`, `closed`,
`described`, `lazy`. `Many` becomes `List` and `Maybe` becomes `Optional` for
that reason. `Refined` carries a structured `Rule` rather than a sentence so
`Shape::to_json_schema` can emit `minimum` and `minLength` instead of prose;
`refine(inner, must, holds)` stays as `Custom(must)` and `between`,
`at_least`, `at_most`, `min_length`, `max_length` (text), `min_items`,
`max_items`, `non_empty` (lists) and `one_of` are one line each over it.
Cheap to add now and expensive to retrofit through a derive. A `Pattern` arm
waits on a regular-expression module, which `std` does not have. `Lazy` holds
a thunk, so `Shape` loses `derive(Show)` and gets a hand-written one that
prints the name; every walker (`keys`, `to_json_schema`, `closed`,
`config::variables`) forces it, and the JSON Schema renderer keeps a visited
set.

**A variant on the wire.** A payload-free case is a bare string; a payload
case is an object whose first key is `type`:

```text
"Local"                                      Mode::Local
{ "type": "Remote", "url": "https://x" }     Mode::Remote
"Info"                                       Level::Info
```

One rule per case, decided by the case and not by its siblings, so adding a
payload case to a shipped enum does not change how the existing cases are
written. That is what a TypeScript union of `Literal("Local")` and a tagged
`Struct` encodes, and what JSON Schema's `oneOf` over a `const` and an object
expresses directly; the rendered schema and the decoder accept exactly the same
language. The tag key is `type` because `type` is a hard keyword and a record
field cannot be named by it (probed: a parse error), so no payload can ever
collide with its own tag; `case`, the current key, is an ordinary identifier
and can. `Schema::cases_tagged("kind", ..)` is the hand-written form for a
foreign API. A positional payload is refused by the derive rather than keyed
by number: a name the type did not declare is not the compiler's to invent.
Untagged unions of records are not offered, because they are ambiguous by
construction; a leaf that arrives as either a string or a number is a
`try_map` over `any()`.

## 7. Encode is a separate trait

`Encode` is not folded into `Schema<A>`, and this is the one place the plan
departs from Effect on purpose.

Effect's `Schema.Redacted` encodes the secret back out as the plain string;
the redaction lives only in `toString`, and its docs warn that a decode error
can expose the value. A bidirectional Khora value would have to say what
`secret()` writes, and there are two answers, both worse than not asking: omit
the key (Go's `json:"-"`), which turns the compile-time refusal
`std::core::Redacted` argues for into a run-time hole a round trip reports as
`password is not set` at the far end; or make `write` fallible, which makes
every encoder in the language answer a `Result` to protect one combinator. A
third, giving `Schema` an error row that `secret` extends, is elegant and puts
a row variable on every annotation.

With the trait separate, `derive(Decode)` on a record holding a `Redacted` is
accepted and `derive(Encode)` on it is refused at the derive line by the
per-field check that exists today, and no encoder is fallible. The pair a
derive writes agrees by construction. The honest cost: a hand-written `Decode`
that renames a key needs a hand-written `Encode` to match, and nothing checks
it; a round-trip test helper is the mitigation, which is the discipline serde
asks for.

`Encode` writes `Raw`, not `Json`, so decode and encode share one tree and
`Raw::to_json` is the single bridge out. `Response::json` takes `A: Encode`
instead of `A: Show`; `impl Encode for Json` keeps `Response::json(200,
Json::Object(..))` compiling in both example services.

## 8. Errors, and every sentence they print

`Rejection { path, secret, problem }` is kept; it is already the shape of
Effect's `ArrayFormatter`. `Problem` gains `Unexpected`, produced only by
`closed()` (unknown keys are ignored by default, as every library this
audience uses does), and `Denied`. `found` stays a `Redacted<String>`
unconditionally and `secret` still marks every rejection under it, so no new
problem kind, transform or formatter can quote a password.

| problem | sentence |
| --- | --- |
| `Missing` | `listen.host is not set` |
| `Wrong(wanted, found)` | `listen.port should be a whole number, and is "8080"`; a `Text` or `Untyped` is quoted, a `Number` is bare, `true`/`false`, `null`, `a list`, `a record` as themselves |
| `Wrong` under a secret | `password should be text` |
| `Refused(rule)` | `listen.port must be between 1 and 65535`, `at least 1`, `at most 10`, `at least 3 characters`, `at most 80 characters`, `at least 1 item`, `at most 5 items`, `not empty`, `` one of `a`, `b` ``, or the `Custom` text |
| `Unexpected` | `verbose is not expected` |
| `Denied` | `password is not granted`; `std::config` says `PASSWORD is not granted -- add it to [permissions] env in khora.toml` |
| an unknown tag | `` payment.type should be one of `Card`, `Cash`, and is "Cheque" `` |
| a payload case given a bare string | `payment should be a record, and is "Card"` |
| a tag that is not text | `payment.type should be text, and is 7` |
| a failed `try_map` | `created_at should be an ISO 8601 date, and is "yesterday"` |

What each primitive wants: `text`, `a whole number`, `a number`, `an exact
decimal`, `true or false`, `a list`, `a record`. A renamed key reports the wire
name.

`Rejection::report(problems)` joins one `describe` per line; `config::report`
does the same with paths spelled as variable names, and is the one a service
prints before it stops. There is no tree formatter: a person fixing a
deployment wants lines, and a program wants the list.

**`impl Encode for Rejection`** is hand-written in `std::schema` and writes
`{ "path": "ship_to.city", "message": "ship_to.city is not set" }`, never
`found`, so a handler's 422 body is `Response::json(422, problems)` and a
TypeScript client gets the array of issues it expects.

## 9. Every boundary, one path

- **HTTP body.** `parse(request.body)` answers `Result<Json, JsonError>` (400:
  not JSON); `Input::schema().decode(Raw::of_json(document))` answers
  `Validated` (422: wrong shape, `Response::json(422, problems)`). `JsonError`
  stays a `Result` so that split survives. Query, path parameters and
  headers: `Raw::of_map(request.queries)`. `Response::json<A: Encode>`.
- **Environment.** `std::config` keeps its name and loses its vocabulary:
  `read<A>(schema: Schema<A>) -> Validated<A, Rejection> with { env: Env }`
  walks the shape, reading `LISTEN_PORT` for `listen.port`, `TAGS=a,b` for a
  `List`, `MODE=Remote` plus `MODE_URL=..` for a payload case and `MODE=Local`
  for a bare one, into a `Raw` of `Untyped`, `Absent` and `Denied`; a `Keyed`
  shape names its variable outright, which is the override. `variables(shape)`
  answers the deployment question without starting the program; `report`
  spells paths as variable names. `string`, `int`, `decimal`, `bool`,
  `secret`, `or_default` and `ConfigError` are deleted. The module doc's
  argument against a description type was about deferring a read; a schema
  defers nothing, and the `Env` handler is still the only provider.
- **Database rows.** `Row` gains `columns: List<String>` (postgres has them
  and drops them before building rows) and `Row::to_raw()`: `Number` and
  `Money` render to `Raw::Number` text, which both round-trip exactly, so
  `Raw` keeps one numeric representation; `Text` to `Text`, `Flag` to `Bool`,
  `Null` to `Null`. `Row::sequence(rows)` is the `Raw::Sequence` of them, so
  the spelling people copy is `list(Entry::schema()).decode(Row::sequence(rows))`,
  which reports `[3].amount is not set` with the row index and accumulates
  across rows for free. (`Schema::lazy` then rebuilds `Entry`'s field schemas
  once per row; §14.) `ledger_service` replaces forty lines of nested `Option`
  matches with that line, and a bad row is reported rather than dropped.
- **Model answers.** `Extract` is deleted. `extract<A: Decode>(prompt)` puts
  `A::schema().shape.to_json_schema()` in the prompt, parses the answer,
  decodes it, and raises `SchemaExtractionError(problems: List<Rejection>)`
  where today it carries a `String`. `risk_analyzer`'s stub becomes
  `derive(Decode)` on its report and level.
- **Command line.** `Raw::of_arguments(env.arguments())` then decode; flags
  are the record's field names, `-` for `_`. A `usage(shape)` renderer can
  follow as a `std::cli` module; `khq` is the first user.
- **Files.** `read_text` then `parse`, or a row at a time through `Raw::of_map`
  against a header.

## 10. JSON Schema

`Shape::to_json_schema(self) -> Json` renders draft 2020-12: `Struct` to
`object` with `properties` and a `required` list omitting `Optional` and
`Default` fields; `Closed` to `additionalProperties: false`; `Cases` to `enum`
when every case is payload-free and otherwise `oneOf` over a `const` per bare
case and an object per payload case; `Refined` to its rule's keyword
(`minimum`, `maximum`, `minLength`, `maxLength`, `minItems`, `maxItems`,
`enum`) plus the sentence in `description`; `Secret` to `writeOnly: true`;
`Described` to `description`; `Keyed` to the wire name; `Dict` to
`additionalProperties`; `Lazy` to a `$defs` entry and a `$ref`, with the
visited set terminating recursion; `Nullable` to a `type` array with `"null"`;
`Any` to `{}`. `Default` is rendered as not required only; the default value
itself is not rendered, because rendering it would need `A: Encode` on the
combinator. `Decimal` renders as `string` with a description saying the digits
are exact, matching what `Encode` writes.

## 11. Compiler changes, sized

| where | what | size |
| --- | --- | --- |
| `crates/khora-types/src/check.rs`, `check_function` | arm the root hint with the declared return type before inferring the body root; today `fn f() -> U8 { 200 }` fails and a tail record literal cannot use its return type | S |
| `crates/khora-types/src/check/expr.rs`, `Expr::Return` | a lambda-return stack so `return` inside a lambda is checked against the lambda's result; HIR and codegen already treat it so, and the checker is simply wrong today | S |
| `crates/khora-types`, `type_of_ref` | a function type written in a `let` annotation is dropped to `Unknown`, so `let f: (Int) -> Int = fn x => "s";` checks clean and the annotation neither hints nor checks the lambda; found while probing, fixed with the two above | S |
| `crates/khora-hir/src/derive.rs` | `Decode`/`Encode` in `DERIVABLE`, `method_of`; capture case payload *names* in `shape_of` (only types are kept today); record, variant, newtype writers for both traits; refuse positional payloads; delete the four JSON writers | M |
| `crates/khora-hir/src/lib.rs` | companion lists: `Decode` brings `Schema`, `Fields`, `List`; `Encode` brings `Raw`, `List`, `Pair`; delete the JSON lists | S |
| `crates/khora-types/src/derive.rs` | per-trait home table for the import hint | S |
| `crates/khora-hir/src/body/exprs.rs` (+ a `body/schema.rs`) | the `struct` rewrite at call lowering, the origin check, the hidden locals, the synthesized lambda, the pipe-form refusal, the nine diagnostics; `Expr::Record` gains an origin so `infer_record` can word its messages | M |
| `crates/khora-types/src/check/expr.rs`, `infer_record` | when the hint names a declared record, commit to it and report per field rather than fall back to the label search; schema wording under the `struct` origin | S |
| `crates/khora-syntax` (shared trivia walk) | lift `khora-doc`'s `doc_of` so the derive can copy `///` into `described`; last and optional | S–M |
| codegen, monomorphization, trait selection | none; every generated form is calls, records, tuples, lambdas and matches the backend already compiles | — |

## 12. Migration, in independently shippable steps

Each step leaves the gate green on its own, and each names what it needs.
Every step that deletes or renames something carries its own CHANGELOG
Breaking entry; the existing Unreleased bullet saying `many` and
`struct2`..`struct5` are unchanged is amended by the step that changes them.

1. **Checker fixes.** Root hint; lambda-return stack; function-typed `let`
   annotations. Tests: `fn f() -> U8 { 200 }`, a tail record literal resolved
   by return type, a `return` inside a lambda, a mismatched lambda under a
   function-typed annotation. `Validated::zip` in `std::core` with its doc.
   Needs nothing.
2. **`std::schema` representation, hand-written path complete.** `Raw` arms,
   `Problem` arms, `Rule`, the new `Shape` and its hand-written `Show`, the
   constructors and combinators of §6, `Schema::map`/`try_map`/`record`/`cases`/
   `cases_tagged`/`case`/`lazy`, `Fields` (`of`, `none`, `zip`, `map`) and
   `Case`, `Rejection::report`, `Raw::of_json`/`to_json`/`of_map`/`of_arguments`.
   Keep `struct2`..`struct5` for this step, reimplemented over `Schema::record`
   (a `Fields::zip` chain and the caller's assembler in `Fields::map`), so the
   eight schema tests and the cookbook pass on mechanical edits. Regenerate the
   API page. Breaking: the arms, the strictness, `many` to `list`. Needs 1.
3. **The traits.** `Decode` and `Encode` with every `std` impl and
   `Encode for Rejection`. The first test is the five-line one:
   `let s: Schema<Int> = Decode::schema();` selects the impl with `Self` nested
   in the constructor. Hand-written `impl Decode for Listen` tests through a
   generic impl and a newtype. Needs 2.
4. **`derive(Decode)` and `derive(Encode)`**, while the JSON derives still
   exist. Expansion tests in `khora-types/tests/derive.rs` pinning the
   annotated selection and each refusal; end-to-end tests for a record, a
   nested record, a generic record, a variant, an enum, a newtype, a
   self-recursive and a mutually recursive type, and a record with a secret
   that derives `Decode` and refuses `Encode`. Needs 3.
5. **`struct({ .. })`.** The rewrite, the origin mark, the diagnostics, checker
   tests for each row of the table, end-to-end tests. Delete `struct2`..`struct5`;
   rewrite `tests/schema.rs`, the prose page and the cookbook, and regenerate
   the cookbook's byte-for-byte output. Breaking: `struct2`..`struct5`. Needs
   1 and 2; independent of 4, and ordered after it only so the literal is
   documented as the override form.
6. **JSON.** Delete `FromJson`, `ToJson`, `DecodeError`, `decode`, `field_as`,
   `variant`, `unknown_variant` and the `variant_*` helpers; keep `Json`,
   `JsonError`, `parse`, `encode`, `Field`, `member`, `object`, the accessors
   and `Show for Json`, which `Raw::of_json` and `Raw::to_json` are built on.
   `Response::json<A: Encode>`. Rewrite `tests/json.rs`, `tests/redaction.rs`
   and the JSON API cookbook onto `parse`, `Raw::of_json` and
   `Input::schema().decode`; both example services. Reword the `Redacted` doc
   in `std/core.kh`, the `secret` doc in `std/config_native.kh`, the reference
   pages `declarations.md` and `traits.md`, and the migration page's
   `Redacted` row, all of which say `ToJson` today and would fail
   `khora doc --check`. Breaking: the traits, the variant wire format. Needs
   3 and 4.
7. **Environment.** `config::read`, `variables`, `report`, `Denied` through
   `Raw::Denied`, `Keyed` as the naming override; delete the readers and
   `ConfigError`; `tests/config.rs`, the configuration cookbook, the migration
   page's Config section; both example services read their settings through
   it. Breaking: the readers and `ConfigError`. Needs 2.
8. **Rows.** `Row.columns`, `Row::to_raw`, `Row::sequence`; postgres passes
   names; `tests/db.rs` and the postgres tests; `ledger_service` decodes rows
   through a derive. Breaking: `Row`'s shape, every `Db` handler. Needs 2.
9. **JSON Schema and the model.** `Shape::to_json_schema` with a test per arm
   including `$defs`/`$ref` for a recursive type; `extract<A: Decode>`; delete
   `Extract`; `risk_analyzer` derives its report. Breaking: `Extract`,
   `SchemaExtractionError`'s payload. Needs 4.
10. **Command line.** `Raw::of_arguments`; `khq`'s flags through a derived
    record; `usage(shape)` if a `std::cli` module is wanted. Needs 4.
11. **Doc comments into descriptions.** The shared trivia walk, the escaping of
    `"`, `\` and `${` in generated literals, and a test that a field's `///`
    appears in `to_json_schema`'s output. Needs 4.
12. **Records.** Rewrite `schema.md` in place from this note and the shipped
    code, and keep this note as the record of the decision, the way
    `typeclasses.md` outlived its implementation; an errata entry recording
    that "cannot be typed" was true of a library function and not of the
    language; a roadmap row for what #170 tracks; rescore the readiness list.

Every rename must be verified by running the schema, config, json, db and
derive test binaries, not by grep: Khora programs live inside Rust string
literals in `crates/khora-codegen-llvm/tests`, and the last rename missed a
third of its call sites that way.

## 13. What is mirrored from Effect, what is adapted, what is skipped

**Mirrored**, because it is intrinsic to a codec with a description: one value
interpreted several ways (decode, encode, JSON Schema, documentation);
accumulate-every-problem with a fail-fast door; optional, nullable and
default; list, dictionary and record combinators; filters that carry
structure; parse-don't-validate transforms; recursion by suspension; a
path-carrying error list with an array formatter on the wire; JSON Schema
generation; title and description metadata; reading configuration from a
schema, with `key` naming the variable; renaming a wire key.

**Adapted.** `Schema<A, I, R>` loses `I` (every source produces a `Raw`, so the
encoded side never varies and the `XFromY` family and `compose` vanish) and
`R` (a refinement that needs a capability puts an effect row on its closure).
`suspend` is what the derive emits for every type, so a user never writes it
for a derived one. `TaggedUnion`, `Literal` and `Enums` are variants;
`Literal` values that are not identifiers are `one_of`. `transformOrFail` is
`try_map`. `Class` is a nominal type with `derive(Show, Eq, Hash, Decode,
Encode)`; its validating constructor is a newtype's impl. `Schema.Config` is
`config::read`. `Redacted` decodes and refuses to encode, where Effect's
encodes the plain string.

**Skipped**, each with its reason: type extraction (`Schema.Type`,
`Schema.Encoded`, `asSchema`, the whole schema-to-type direction: the type is
the source); brands (a refinement is a newtype; see §3 for what that does and
does not guarantee); `pick`/`omit`/`partial`/`required`/`extend`/`keyof`
(structural-typing tools; in a nominal language each is "declare another
record", and `keyof` is `Shape::keys`); `UndefinedOr`/`NullishOr`/`exact`
(JavaScript's second absence); `Tuple` (positional data reaches a schema as a
named row; a tuple combinator can follow if a real source needs one);
`pattern` and `TemplateLiteral` (no regular-expression module yet; the `Rule`
arm is reserved); `parseJson` (a JSON document inside a string is
`try_map` over `parse` for now; a combinator that keeps the inner paths is
cheap and can follow); custom `message`/`missingMessage` (the sentences are
fixed by design so a report reads the same everywhere; `refine`'s `must` is
the one message an author writes); `decodingFallback`; untagged unions of
records (ambiguous by construction); `is`/`asserts`/`instanceOf` (no
`unknown` to narrow); `Pretty`, `Equivalence`, `Arbitrary` (`Show`, `Eq`,
`Hash` derive from the type; a generator can be written over `Shape` when a
property-testing library exists); the five decode flavors (`decode` and
`decode_or_stop` are the family, and `Validated::to_result` is the door to
`!`); one-way transforms with a forbidden encode (`Encode` is total, and a
value that must not go back out is a `Redacted`); `examples` annotations
(deferred; `described` covers the prompt today); Standard Schema interop,
`propertyOrder`, `concurrency`, `batching`.

## 14. Risks, stated

- **`Schema::lazy` on every derived type** rebuilds a record's field schemas
  per record decoded, including once per row in `list(Entry::schema())`. Fine
  for configuration and request bodies; measurable on a hot row decoder. The
  derive can emit eager construction for types whose fields name no user type
  if a profile asks for it.
- **Two record writers.** The derive writes text that is re-parsed; the
  `struct` rewrite synthesizes HIR nodes. They cannot share code and must be
  kept in step; a test that decodes the same record through both and compares
  the rejections pins it.
- **Companion capture is narrowed, not gone.** A deriving file with its own
  type named `Schema`, `Fields`, `List`, `Raw` or `Pair` captures the
  generated text; the message must say which companion was taken.
- **Strictness is a silent behavior change** for any program that read a JSON
  string into an `Int`; it is a Breaking entry, and nothing at compile time
  flags the call site.
- **Two traits can drift** where a `Decode` is hand-written and its `Encode`
  is not kept in step; a round-trip test helper is the mitigation.
- **`Shape` holds a thunk**, so it loses `derive(Show)` and `Eq` and every
  walker must force it; a renderer that forgets its visited set loops.
- **`Row` gaining columns** breaks every `Db` handler and test double at once;
  the compiler finds each site, but the change touches the postgres package
  and the reference application in one step.
- **The environment flattening** (`LISTEN_PORT`, `TAGS=a,b`, `MODE`/`MODE_URL`)
  is a convention; `key` overrides one variable at a time, and a deployment
  with entirely different names hand-writes the source walk.
- **A newtype is not a private type.** `Port::Port(99999)` compiles from any
  module; the refinement guards the boundary, not the program. A module-only
  constructor is a separate language decision.
