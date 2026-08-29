---
title: Types
sidebar:
  order: 3
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

## Failure unions

`+` forms the union used by `raises` rows:

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

Wrapper — a type of its own over an existing one, built with `UserId(1)` and
opened with `match id { UserId(v) => v }`. Khora has no transparent alias; see
[Wrapper types](/docs/guide/data-types/#wrapper-types):

```khora
pub type UserId = Int;
```

Record:

```khora
pub type User = {
  id: UserId,
  name: String,
};
```

Variant:

```khora
pub type Lookup =
  | Found(user: User)
  | Missing;
```

See [Declarations](./declarations/) for visibility, `derive`, and complete declaration forms; see [Generics](./generics/) for bounds, const parameters, variance, and polymorphism.