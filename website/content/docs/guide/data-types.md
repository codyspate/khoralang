---
title: Data types
sidebar:
  order: 3
---

Khora's data model is built from primitive values, tuples, records, algebraic data types, and generic types. Types are declared with `type`; add `pub` when the type belongs to the module's public API.

## Primitive values and literals

Common scalar values include integers, floating-point numbers, exact decimals, booleans, strings, and unit:

```khora
let count: Int = 42;
let ratio: Float = 0.75;
let price: Decimal = 19.99d;
let enabled: Bool = true;
let name: String = "Khora";
let nothing: () = ();
```

A fractional literal without a suffix is a `Float`. The `d` suffix makes an exact `Decimal` literal, and the module has to have `import std::decimal::{Decimal};` for the literal to have a type:

```khora
let approximate = 0.1;
let exact = 0.1d;
```

See [Collections and strings](/docs/guide/collections-and-strings/) for interpolation and multiline string syntax.

## Wrapper types

A type declaration over an existing type gives a domain name to a value, and
gives it a type of its own:

```khora
pub type UserId = Int;
pub type OrderId = Int;
```

**These are distinct types, not other names for `Int`.** A `UserId` is not
accepted where an `Int` is wanted, an `Int` is not accepted where a `UserId`
is, and a `UserId` is not an `OrderId` — which is the reason to write one:

```
error: this argument: expected `UserId`, found `OrderId`
```

Build one by calling the type's name, and take it apart by matching on it —
the shape a Rust tuple struct has:

```khora
let id = UserId(1);

fn number(id: UserId) -> Int {
  match id { UserId(value) => value }
}
```

`derive` works on a wrapper as it does on anything else, and is usually what
you want — without `Eq` and `Ord` a `UserId` cannot be a `Dict` key, and
without `Show` it cannot go in a `${..}` hole:

```khora
derive(Eq, Ord, Show)
pub type UserId = Int;
```

`Show` prints `UserId(1)`, not `UserId::UserId(1)`: the one case is the type.

The underlying type may be anything, including a generic one, which is how a
long type gets a short name:

```khora
pub type Books = Dict<Currency, Bucket>;
```

Khora has **no transparent alias** — no form that means "another spelling of
the same type". If that is what you want, write the type out, or accept the
wrapper and the one `match` it costs.

## Tuples and unit

Tuples group values positionally:

```khora
let point = (10, 20);
let tagged = ("ready", true, 3);
```

Tuple types use the same shape:

```khora
fn dimensions() -> (Int, Int) {
  (1920, 1080)
}
```

`()` is both the unit value and the unit type. It is the result of a computation with no meaningful value to return.

## Record types and record values

Records give fields names:

```khora
pub type User = {
  id: Int,
  name: String,
  active: Bool,
};
```

Construct a record with a record literal:

```khora
let user = {
  id: 42,
  name: "Ada",
  active: true,
};
```

Read fields with `.`:

```khora
print(user.name);
```

Use records when field names carry meaning or when a value will cross an API boundary.

### Mutable fields

A field marked `mut` can be assigned through a value of the record type. Every other field is fixed once the value exists.

```khora
pub type Tally = {
  name: String,
  mut count: Int,
};

let seen: Tally = { name: "hits", count: 0 };
seen.count = seen.count + 1;
```

This is what in-place aggregation is written out of, and it is the fast shape for grouping by a small key — an array of counters updated where they sit, rather than a new value per event:

```khora
let buckets: Array<Tally> = Array::from_fn(3, fn i => { name: "b${i}", count: 0 });
Array::get(buckets, 1).count = Array::get(buckets, 1).count + 5;
```

`Array::from_fn` rather than `Array::new`, because `new` puts the *same* value in every cell — which is invisible for a record nobody can change and is one counter with three names once a field is `mut`. `Array::from_fn` calls its function once per cell.

A record with a `mut` field cannot cross into a fiber: two fibers writing one record is a data race, and the type says so. See [Fibers and nurseries](./fibers-and-nurseries/) for what does cross.

### Updating a record

`{ ..base, field: value }` builds a new record from an existing one. Every field
not named comes from `base`:

```khora
let renamed = { ..user, name: "Grace" };
```

This is a **new record**. `user` is unchanged and still whatever it was; what
the syntax saves is writing out the fields that do not change, which matters as
soon as a record has more than a few:

```khora
fn applied(counts: Counts, event: Event) -> Counts {
  match event {
    Event::Created => { ..counts, created: counts.created + 1 },
    Event::Deleted => { ..counts, deleted: counts.deleted + 1 },
    Event::Expired => { ..counts, expired: counts.expired + 1 },
  }
}
```

The base comes first and appears once. A field named twice is an error rather
than a last-one-wins, a field the base's type does not have is an error, and a
base that is not a record is an error. `{ ..base }` with no fields after it is
`base`.

Records with [`mut` fields](/docs/reference/types/#record-types) are the other
way to do this, and the difference is that a record update produces a new value
while assigning a `mut` field changes the one you already have. Reach for the
update when the old value still matters, and for `mut` when it does not.

## Algebraic data types

A variant type enumerates the shapes a value may have:

```khora
pub type LoadState =
  | Idle
  | Loading
  | Loaded(value: String)
  | Failed(message: String);
```

Constructors are namespaced with `::`:

```khora
let state = LoadState::Loaded("done");
```

Variants may have no payload, named payloads, or positional payloads. Pattern matching opens the value safely:

```khora
let message = match state {
  LoadState::Idle => "idle",
  LoadState::Loading => "loading",
  LoadState::Loaded(value) => value,
  LoadState::Failed(reason) => "failed: ${reason}",
};
```

See [Pattern matching](/docs/guide/pattern-matching/) for destructuring and guards.

## Generic types

Types can take parameters:

```khora
pub type Box<A> = {
  value: A,
};

pub type Outcome<A, E> =
  | Ok(value: A)
  | Err(error: E);
```

Use generics when the structure is independent of the concrete element type. Trait bounds, const parameters, row variables, and higher-kinded forms are covered in [Generics and traits](/docs/guide/generics-and-traits/).

## Deriving structural behavior

For structural traits whose implementation follows directly from the fields, place `derive(...)` immediately before the type declaration:

```khora
derive(Eq, Show, ToJson, FromJson)
pub type User = {
  id: Int,
  name: String,
};
```

Khora can derive `Eq`, `Ord`, `Show`, `Hash`, `ToJson`, and `FromJson` when the fields support the requested trait. Use a handwritten `impl` when the behavior is a domain decision rather than a structural consequence of the data.

The trait has to be in scope, so `derive(Show)` needs `Show` imported from `std::core`.

A field's type decides whether the derive is available, and a missing impl is sometimes the point:

- `List<A>` has `Show` and `Eq` when `A` does, so a record holding a list derives both. It has `ToJson` and `FromJson` from `std::json` on the same terms.
- `Redacted<A>` has `Show` — it prints `<redacted>` — and deliberately no `ToJson`. A record holding a secret stays printable and refuses to serialize, and the build stops rather than the payload leaking. It has no `Eq` either, because comparing two secrets byte by byte is how a timing side channel gets written by somebody who was not writing one.

## Exact decimals

`Decimal` is intentionally distinct from `Float`. Use `Float` for approximate numerical computation and `Decimal` when base-10 representation is part of correctness, such as money or externally specified decimal values:

```khora
let tax_rate: Decimal = 0.0825d;
let subtotal: Decimal = 19.99d;
```

The distinction is visible in source so callers can tell whether an API is doing approximate binary floating-point or exact decimal arithmetic.

### Arithmetic is by method, comparison is by operator

`Decimal` compares with the ordinary operators, because `Eq` and `Ord` are
traits and `Decimal` implements them — and it does so **by value**, so `1.50d`
and `1.5d` are the same number:

```khora
if paid == owed { .. }
if amount < limit { .. }
```

Arithmetic is by name. `+`, `-` and `*` are the primitive numeric types', and
adding a trait per operator is a language change this has not taken:

```khora
let total = subtotal.add(shipping).sub(discount);
let tax   = 19.99d |> Decimal::mul(0.0825d) |> Decimal::rounded(2, Rounding::HalfEven);
```

Both forms read well and both are the same call. The pipeline is worth knowing
about: it is the shape a chain of money operations takes, and it is what
Effect's `BigDecimal` uses for the same reason.

### Scale is part of the number

`mul` **adds** the scales — two numbers with four decimal places give one with
eight — and `add` brings both operands to the larger scale. That is what makes
the arithmetic exact, and it is also what makes a long chain overflow sooner
than people expect. `rounded` is the way back:

```khora
let charge = total.rounded(2, Rounding::HalfEven);
```

`divide` is the one operation that cannot be exact, so it takes the scale and
the rounding mode and returns `Option<Decimal>` — `None` when the divisor is
zero. Everything else stops the program rather than returning a number that is
not the answer; see [Traps](/docs/reference/traps/).

`Show` prints every place the scale says, so `Decimal::scaled(150, 2)` is
`1.50` and not `1.5`. A price to two places stays a price to two places, which
is what makes a column of them line up.

### How much fits

The significand is 128 bits — 38 digits — which covers every fiat computation,
`NUMERIC` as real schemas declare it, and an eighteen-decimal token balance.

It matters because alignment happens before addition, so what has to fit is not
the two numbers but the *aligned* ones:

```khora
let notional = 100000000.00d;   // scale 2
let rate     = 0.000000000001d; // scale 12

let together = notional.add(rate);  // needs the notional at scale 12
```

At 64 bits that stopped the program, on two numbers a rates desk writes down
every day. Going past 38 digits still does — the answer would be a different
number, and a different total is the failure this type exists to prevent.

### A column of them

```khora
let total = Decimal::total(rows);              // every one added up
let shown = Decimal::total(rows).at_scale(2);  // and at the column's width
```

`total` of nothing is `Decimal::zero()`, which is scale nought and prints as
`0`; `at_scale` only ever raises, so it cannot round a total that was already
wider. `Decimal::zero_at(2)` is the empty total when the shape matters from the
start. `abs`, `min` and `max` are there too, and the two comparisons are by
value, so `1.5d` and `1.50d` are the same number to them.

Full API: [`std::decimal`](/docs/stdlib/api/decimal/).