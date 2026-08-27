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

A fractional literal without a suffix is a `Float`. The `d` suffix makes an exact `Decimal` literal:

```khora
let approximate = 0.1;
let exact = 0.1d;
```

See [Collections and strings](./collections-and-strings.md) for interpolation and multiline string syntax.

## Type aliases

Give an existing type a domain name with a type declaration:

```khora
pub type UserId = Int;
```

Aliases are useful when the name improves the API, but they do not create a new runtime representation by themselves.

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

See [Pattern matching](./pattern-matching.md) for destructuring and guards.

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

Use generics when the structure is independent of the concrete element type. Trait bounds, const parameters, row variables, and higher-kinded forms are covered in [Generics and traits](./generics-and-traits.md).

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

## Exact decimals

`Decimal` is intentionally distinct from `Float`. Use `Float` for approximate numerical computation and `Decimal` when base-10 representation is part of correctness, such as money or externally specified decimal values:

```khora
let tax_rate: Decimal = 0.0825d;
let subtotal: Decimal = 19.99d;
```

The distinction is visible in source so callers can tell whether an API is doing approximate binary floating-point or exact decimal arithmetic.