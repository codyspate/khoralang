---
title: Generics and traits
sidebar:
  order: 6
---

Generics let one definition work across many types while preserving static checking. Traits describe behavior a type can provide and generic code can require.

## Generic functions

Declare type parameters between the function name and parameter list:

```khora
fn identity<A>(value: A) -> A {
  value
}
```

Several parameters are comma-separated:

```khora
fn pair<A, B>(left: A, right: B) -> (A, B) {
  (left, right)
}
```

Khora infers type arguments at ordinary call sites:

```khora
let value = identity("hello");
```

## Generic types

Types use the same parameter syntax:

```khora
pub type Box<A> = {
  value: A,
};

pub type Either<A, B> =
  | Left(value: A)
  | Right(value: B);
```

## Trait bounds

Add a bound after `:` when the implementation needs behavior from a type parameter:

```khora
fn same<A: Eq>(left: A, right: A) -> Bool {
  left == right
}
```

Combine bounds with `+`:

```khora
fn describe_sorted<A: Ord + Show>(value: A) -> String {
  value.show()
}
```

Prefer the smallest useful bound. A function that only needs equality should ask for `Eq`, not a broader set of behavior.

## Declare a trait

A trait declares behavior without choosing the concrete type that provides it:

```khora
pub trait Named {
  fn name(self) -> String;
}
```

Traits may inherit other traits:

```khora
pub trait Persisted: Named + Show {
  fn id(self) -> Int;
}
```

## Implement a trait

Use `impl Trait for Type`:

```khora
impl Named for User {
  fn name(self) -> String {
    self.name
  }
}
```

Trait methods are reached where the trait is in scope. Imports therefore participate in method and operator resolution.

## Inherent methods

An `impl` without `for` adds methods belonging to the type itself:

```khora
impl User {
  pub fn display_name(self) -> String {
    self.name
  }
}
```

Use `pub` on an inherent method when code outside the declaring module should call it.

## Associated types

A trait may name a type that each implementation chooses:

```khora
pub trait Iterator {
  type Item;
  fn next(self) -> Option<Item>;
}
```

An implementation supplies the associated type:

```khora
impl Iterator for UserIterator {
  type Item = User;

  fn next(self) -> Option<User> {
    // ...
  }
}
```

Associated types can also carry trait bounds when the trait contract requires them.

## Const generics

Use `const` inside a generic parameter list when a compile-time value is part of the type:

```khora
pub type Matrix<A, const Rows: Int, const Cols: Int> = {
  // representation omitted
};
```

Integer literals can appear as const type arguments:

```khora
let transform: Matrix<Float, 4, 4> = make_transform();
```

This `const` is a generic parameter declaration. Module-level constants use the related but separate syntax described in [Values and functions](./values-and-functions.md#module-constants-with-const).

## Failure and capability row variables

Generic higher-order functions can be polymorphic over the capabilities and failures of a function they receive. Row variables begin with `'`:

```khora
fn map<A, B, 'e, 'r>(
  values: List<A>,
  transform: A -> B with 'e raises 'r,
) -> List<B>
  with 'e
  raises 'r
{
  // ...
}
```

`'e` and `'r` are ordinary row-variable names; their meaning comes from where they are used. This is why an effectful function can be passed to ordinary higher-order operations without introducing a separate `traverse` API.

## Explicitly polymorphic function types

When a value itself must be polymorphic, `forall` quantifies parameters in the type:

```khora
forall<A>. A -> A
```

Most application code gets polymorphism from a generic declaration and does not need to write `forall` directly. It is primarily useful in APIs that store or accept a polymorphic function value.

## Variance annotations

Generic parameters may carry an explicit variance marker when an API's type relationship requires it:

```khora
pub type Source<+A>;
pub type Sink<-A>;
```

`+A` is covariant, `-A` is contravariant, and an unmarked parameter is invariant. Reach for explicit variance when designing a reusable abstraction whose subtype relationship depends on the parameter; ordinary data types normally leave it unmarked.

## Derive when behavior is structural

For the compiler-supported structural traits, `derive(...)` can generate the implementation:

```khora
derive(Eq, Ord, Show, Hash)
pub type Point = {
  x: Int,
  y: Int,
};
```

Use a handwritten `impl` when behavior is a semantic decision rather than a mechanical consequence of the fields. See [Data types](./data-types.md#deriving-structural-behavior) for the derivable set and the [Traits reference](/docs/reference/traits/) for exact forms.