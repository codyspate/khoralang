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