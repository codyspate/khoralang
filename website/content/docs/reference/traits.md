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

Declare an associated type. `Iterator` in `std::core` declares two, and they
answer different questions — what the iteration yields, and what pulling from
it requires:

```khora
pub trait Iterator {
  type Item;
  type Effects;
  fn next(self) -> Step<Self, Self::Item> with Self::Effects;
}
```

With a bound:

```khora
pub trait Indexed {
  type Key: Eq + Show;
  fn key(self) -> Self::Key;
}
```

Supply them in an implementation. `Effects = {}` is an iterator over something
already in memory, so `next` asks its caller for nothing:

```khora
impl Iterator for Users {
  type Item = User;
  type Effects = {};

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

### Coherence, and the orphan rule that is not there yet

One impl of a trait for a type is the rule, and the compiler enforces it across
the whole compilation — the package's own modules, the sources of the packages
it depends on, and `std`. Two impls in one file are refused where they are
written:

```
error: `Codec` is already implemented for `A`; there can be only one impl of a
       trait for a type
```

and a second `impl Codec for A` in another module names the module that already
has one:

```
error: `Codec` is already implemented for `A` in `store`; there can be only one
       impl of a trait for a type in a program
```

The file whose path sorts first keeps the impl and the other is what gets
reported, so which of two modules is refused does not depend on the order the
files were read in. A type's identity is its name *and* the module that
declared it, so two types both named `Entry` are two types and each may have
its own `impl Codec`.

**There is still no orphan rule**, with one exception: `Share`, which only the
module declaring a type may implement for it. Nothing else stops you writing
someone else's trait for someone else's type. What has changed is that a second
impl is now an error you are shown rather than a silent choice of whichever one
was merged first. The only impl the compiler still cannot see is one in a
package this build does not include, so keep "the declaring package owns the
impl" as a convention: implement a trait for a type you declare, or a type you
declare for someone else's trait.

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

**A trait's methods resolve on every type that implements it, imported or not.**
An implementation is a property of the type, and `value.method(..)` asks the
type — so this compiles in a module that imports neither `Show` nor `Eq`:

```khora
module app::main;
import std::core::{print};

pub fn main() -> Int {
  let same = 1 == 1;
  let text = 42.show();
  if same { print(text); }
  0
}
```

Operators follow the same rule: `==` is `Eq::eq`, and it works on any type that
implements `Eq` without the name being brought in.

What an import buys is the **name**. `Show` has to be imported wherever the word
`Show` is written — as a bound in `<A: Show>`, in a `derive(Show)` clause, or as
the prefix of a trait-qualified call. `for` needs `Iterator` and `Step` in scope
for exactly that reason: it desugars to `Iterator::next`, and a path is a name
being looked up. One consequence worth knowing before it surprises you: a file
that imports `Eq` and then only ever writes `==` never writes `Eq`, so
`unused-import` reports the import, correctly.

## Two traits declaring the same method

A type may implement more than one trait that declares a given method name.
`List` implements both `Functor` and `Iterator`, and both declare `map`, so
method syntax has nothing left to choose with:

```khora
let ys = xs.map(fn (n) => n + 1);
```

```text
error: `map` is declared by `Functor` and `Iterator`, and `List<Int>` implements more than one
```

The answer is to name the trait. A trait-qualified call passes the receiver as
the first argument and settles which `map` is meant:

```khora
module app::main;
import std::core::{Functor, List, print};

pub fn main() -> Int {
  let xs = List::Cons(1, List::Cons(2, List::Nil));
  let ys = Functor::map(xs, fn (n) => n + 1);
  print(ys.show());
  0
}
```

`Iterator::map(xs, fn (n) => n + 1)` picks the other one, and gives a lazy
`Mapped` rather than a `List`. The trait has to be imported for this form, since
this is the case where the name is written. Without the import the call is
refused with ``cannot resolve `Functor::map` in this scope``.

`Functor::map` is a *trait* path, not a module path. [Modules and
packages](./modules-and-packages/#paths-versus-fields) says a module path cannot
prefix an expression, and that still holds — `std::core::map(..)` does not
resolve. `::` after a trait or type name reaches an associated item, which is
what this is.

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