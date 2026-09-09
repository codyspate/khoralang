---
title: Traits
sidebar:
  order: 9
---

A trait is what generic code is allowed to assume, and an impl is a type's
answer to it. Both are checked where they are written.

## Declare a trait

```khora
pub trait Named {
  fn name(self) -> String;
}
```

General form:

```text
pub? trait Name<TypeParams>? (: Trait + Trait ...)? {
  TraitItem*
}
```

A trait item is a function declaration or associated type declaration.

## Trait methods

Required method:

```khora
pub trait Named {
  fn name(self) -> String;
}
```

Default method body:

```khora
pub trait Named {
  fn name(self) -> String;

  fn greeting(self) -> String {
    "Hello, ${self.name()}"
  }
}
```

A semicolon declares a required signature. A block supplies a default implementation.

## Supertraits

```khora
pub trait Ord: Eq {
  fn cmp(self, other: Self) -> Ordering;
}
```

Several supertraits use `+`:

```khora
pub trait Persisted: Eq + Show {
  fn id(self) -> Int;
}
```

An implementation of the child trait must satisfy the supertrait requirements.

## Associated types

Declare an associated type:

```khora
pub trait Iterator {
  type Item;
  fn next(self) -> Step<Self, Self::Item>;
}
```

With a bound:

```khora
pub trait Iterator {
  type Item: Show;
  fn next(self) -> Step<Self, Self::Item>;
}
```

Supply it in an implementation:

```khora
impl Iterator for Users {
  type Item = User;

  fn next(self) -> Step<Users, User> {
    // ...
  }
}
```

## Trait implementations

```khora
impl Named for User {
  fn name(self) -> String {
    self.name
  }
}
```

General form:

```text
impl<TypeParams>? TraitType for TargetType {
  TraitItem*
}
```

A trait implementation provides the methods and associated types required by the trait.

### Coherence, as it stands today

One impl of a trait for a type is the rule, and within a single **module** the
compiler enforces it:

```
error: `Codec` is already implemented for `A`; there can be only one impl of a
       trait for a type
```

**Across modules and across packages it is not enforced yet.** A second `impl
Codec for A` in another module of the same package, or in a package that
depends on the one declaring both `Codec` and `A`, compiles with no error and
no warning — and the one that runs is the impl in the module that declares the
trait, so the other is accepted, checked, and never called.

There is no orphan rule to lean on either. Until that is closed, treat "the
declaring package owns the impl" as a convention you keep rather than one the
compiler keeps for you: implement a trait for a type you declare, or a type you
declare for someone else's trait, and do not implement someone else's trait for
someone else's type expecting the result to be used.

## Generic implementations

```khora
impl<A: Show> Show for Box<A> {
  fn show(self) -> String {
    self.value.show()
  }
}
```

The type parameters after `impl` are scoped to the implementation block.

## Inherent implementations

An `impl` without `for` defines methods belonging directly to the target type:

```khora
impl User {
  fn normalized_name(self) -> String {
    self.name |> String::trim
  }

  pub fn display_name(self) -> String {
    self.normalized_name()
  }
}
```

General form:

```text
impl<TypeParams>? TargetType {
  fn ...
  pub fn ...
}
```

A public inherent method uses `pub`. Trait methods are reached through the trait contract rather than separately exported implementation members.

## Trait bounds on generic parameters

```khora
fn equal<A: Eq>(left: A, right: A) -> Bool {
  left == right
}
```

Several bounds:

```khora
fn render<A: Eq + Show>(value: A) -> String {
  value.show()
}
```

Bounds are trait paths separated by `+`.

## `Self`

Inside a trait, `Self` names the implementing type:

```khora
pub trait Combine {
  fn combine(self, other: Self) -> Self;
}
```

## Trait scope and resolution

Traits participate in method and operator resolution only where the relevant trait is in scope:

```khora
import std::core::{Eq};

let same = left == right;
```

This keeps the meaning of trait-provided behavior tied to explicit module imports rather than a process-wide registry.

## Deriving structural traits

```khora
derive(Eq, Ord, Show, Hash, Decode, Encode)
pub type User = {
  id: Int,
  name: String,
};
```

The compiler can derive those six structural traits when every field supports the requested behavior. A `derive(...)` clause appears immediately before its `type` declaration. `Decode` and `Encode` are `std::schema`'s: the declaration is the schema, and `User::schema()` reads one from untrusted input.

## Generic traits

```khora
pub trait Convert<A> {
  fn convert(self) -> A;
}
```

Trait parameters use the normal generic parameter syntax, including bounds, const parameters, and variance where meaningful.

See [Generics](./generics/) for parameter forms and [Declarations](./declarations/) for the top-level grammar shared by traits and implementations.