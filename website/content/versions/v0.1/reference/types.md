---
title: Types
sidebar:
  order: 7
---

Khora is statically typed with inference for ordinary local expressions and explicit annotations where a boundary or ambiguity needs one.

## Type annotations

Bindings:

```khora
let count: Int = 42;
let names: List<String> = [];
```

Parameters and returns:

```khora
fn add(left: Int, right: Int) -> Int {
  left + right
}
```

## Path types

A named type is a path, optionally with generic arguments:

```khora
Int
String
app::model::User
List<User>
Result<User, DbError>
```

Compile-time path segments use `::`.

## Unit

```khora
()
```

`()` is both the unit type and unit value.

## Tuple types

```khora
(Int, String)
(Int, String, Bool)
(Int,)
```

Parentheses without a comma group a type:

```khora
(Int)
```

## Record types

```khora
{
  id: Int,
  name: String,
}
```

Record fields may be explicitly mutable:

```khora
{
  mut count: Int,
  name: String,
}
```

A `mut` field can be assigned through a value of that record type; an ordinary field cannot.

```khora
let seen: Tally = { name: "hits", count: 0 };
seen.count = seen.count + 1;
```

This is what in-place aggregation is written out of, and the fast shape for
grouping by a small key — counters updated where they sit, rather than a new
value per event:

```khora
let buckets: Array<Tally> = Array::from_fn(3, fn i => { name: "b${i}", count: 0 });
Array::get(buckets, 1).count = Array::get(buckets, 1).count + 5;
```

`Array::from_fn` and not `Array::new`: `new` puts the *same* value in every
cell, which is invisible for a record nobody can change and is one counter with
three names the moment a field is `mut`. `from_fn` calls its function per cell.

A record with a `mut` field cannot cross into a fiber. Two fibers writing one
record is a data race, and the type says so; [Sharing](./sharing/) is what does
cross.

## Variant types

```khora
| None
| Some(value: A)
```

A normal named declaration uses a variant type on the right-hand side:

```khora
pub type Option<A> =
  | Some(value: A)
  | None;
```

Payloads may be named:

```khora
| Point(x: Int, y: Int)
```

or positional:

```khora
| Pair(Int, String)
```

## Function types

Unary function:

```khora
A -> B
```

Several parameters are represented by the parameter tuple on the left:

```khora
(A, B) -> C
```

Function arrows are right-associative:

```khora
A -> B -> C
```

means a function from `A` to a function from `B` to `C`.

## Capability clauses on function types

```khora
Request -> Response with { db: Db }
```

Open capability row:

```khora
A -> B with 'ef
```

A function type can carry both requirements and failures:

```khora
Request -> Response
  with { db: Db, clock: Clock }
  raises DbError + ValidationError
```

The clauses belong to the arrow they follow.

## `Char`

One Unicode scalar value, thirty-two bits wide.

```khora
fn initial(name: String) -> Option<Char> { String::char_at(name, 0) }
```

**Not a `String` of length one.** A `String` can be empty or hold a thousand characters, so a function taking one has to decide what those mean; a `Char` cannot be either. It lives in a register, so scanning a string a character at a time allocates nothing.

**A scalar value is not every 32-bit number.** The range stops at `0x10FFFF`, and the surrogates `0xD800` to `0xDFFF` are a hole in the middle of it — they exist only to encode a pair in UTF-16 and are not characters. `Char::from_code` stops the program on either, the way `U8::of` does on a number that does not fit.

`Char::code` goes the other way and cannot fail.

## Failure rows

`+` joins the failure types of a `raises` row:

```khora
DbError + ValidationError + HttpError
```

For a declaration:

```khora
fn serve() -> Response
  raises DbError + HttpError
{
  // ...
}
```

**Only in a `raises` or `with` clause.** `+` builds a row, and a row is what those two clauses take. Written anywhere else it is refused:

```khora
fn hold(r: Result<Int, A + B>) -> Int   // error
```

A `Result` holds one error type. Handle a wider row with [`catch`](/docs/reference/failures/#catch), which matches per type and never has to name a combined type.

There is no union type — no way to write "an `Int` or a `String`" as the type of a value. `+` in a bound (`T: Eq + Show`) is the *other* meaning of the symbol and means the parameter implements both.

## Generic type arguments

```khora
List<String>
Map<String, User>
Matrix<Float, 4, 4>
```

Type arguments can include integer literal types for const-generic parameters.

## Row variables

A row variable starts with `'`:

```khora
'ef
'er
```

They are used where an API is polymorphic over capabilities or failures:

```khora
A -> B with 'ef raises 'er
```

The name after `'` has no built-in meaning. `'ef` for capabilities and `'er` for errors are conventions, spelled with two letters so they cannot be read backwards: `'e` alone looks like "errors" to anybody arriving from a library that calls them that.

## Record rows and open tails

A closed capability-shaped row:

```khora
{ db: Db, clock: Clock }
```

A row with an open tail:

```khora
{ db: Db | 'ef }
```

Rows may merge additional row values in the tail position:

```khora
{ 'left | 'right | clock: Clock }
```

An **error row** names failure types rather than capabilities, and its entries
are bare:

```khora
{ Oops }
{ Timeout | Refused }
```

That spelling matters in type-argument position, which is the one place a row
has to be written down rather than inferred. A `Fiber`'s second parameter is a
row, so:

```khora
let f: Fiber<(), { Oops }> = Fiber::spawn(work);   // correct
let g: Fiber<(), Oops> = Fiber::spawn(work);       // refused
```

`Fiber<(), Oops>` is a type where a row belongs. It is a declaration nothing
can inhabit, and the compiler says so at the assignment:

```
error: expected `Fiber<(), Oops>`, found `Fiber<(), { Oops: Oops }>`; `Oops` is
       a type and a row belongs here — write it `{ Oops }`
```

Most code never writes one, because a signature's `raises` clause takes the
types directly (`raises Oops`) and everything else infers. `Fiber<A, 'er>` in
the standard library and `Fiber<(), 'er>` in the concurrency reference are row
*variables*, which need no braces — which is why the concrete form is easy to
miss.

Rows are structural; ordinary declared ADTs remain nominal.

## Explicit polymorphic types with `forall`

```khora
forall<A>. A -> A
```

Several parameters, including const parameters, use the normal generic parameter list:

```khora
forall<A, const N: Int>. Vector<A>
```

Most named generic functions do not need explicit `forall`; the declaration's own type parameter list introduces their parameters.

## Opaque types

A type declaration may omit its definition:

```khora
pub type Handle;
```

The representation is not available to ordinary source using the type.

## Wrappers and named data

A type declaration over an existing type gives a domain name to a value, and
gives it a type of its own:

```khora
pub type UserId = Int;
pub type OrderId = Int;
```

**These are distinct types, not other spellings of `Int`.** A `UserId` is not
accepted where an `Int` is wanted, an `Int` is not accepted where a `UserId`
is, and a `UserId` is not an `OrderId` — which is the reason to write one:

```text
error: this argument: expected `UserId`, found `OrderId`
```

Build one by calling the type's name, and open it by matching on it, the shape
a Rust tuple struct has:

```khora
let id = UserId(1);

fn number(id: UserId) -> Int {
  match id { UserId(value) => value }
}
```

`derive` applies as it does to anything else, and is usually wanted: without
`Eq` and `Ord` a `UserId` cannot be a `Dict` key, and without `Show` it cannot
go in a `${..}` hole.

```khora
derive(Eq, Ord, Show)
pub type UserId = Int;
```

`Show` prints `UserId(1)`, not `UserId::UserId(1)` — the one case is the type.

The underlying type may be anything, a generic one included, which is how a
long type gets a short name:

```khora
pub type Books = Dict<Currency, Bucket>;
```

Khora has **no transparent alias**: no form meaning "another spelling of the
same type". Where that is what is wanted, write the type out, or take the
wrapper and the one `match` it costs.

The other two ways a declaration names data, for comparison — a record:

```khora
pub type User = {
  id: UserId,
  name: String,
};
```

and a variant:

```khora
pub type Lookup =
  | Found(user: User)
  | Missing;
```

See [Declarations](./declarations/) for visibility, `derive`, and complete declaration forms; see [Generics](./generics/) for bounds, const parameters, variance, and polymorphism.