---
title: Generics
sidebar:
  order: 7
---

Khora supports type parameters, trait bounds, const parameters, row variables, higher-kinded use, explicit `forall`, and variance annotations.

## Type parameters on functions

```khora
fn identity<A>(value: A) -> A {
  value
}
```

Several parameters:

```khora
fn pair<A, B>(left: A, right: B) -> (A, B) {
  (left, right)
}
```

General parameter-list shape:

```text
<Name, Name, ...>
```

## Type parameters on data types

```khora
pub type Box<A> = {
  value: A,
};

pub type Result<A, E> =
  | Ok(value: A)
  | Err(error: E);
```

## Generic type arguments

```khora
Box<Int>
Result<User, DbError>
Map<String, User>
```

`<...>` in type position is always a type-argument list; no turbofish syntax is required.

## Trait bounds

Single bound:

```khora
fn same<A: Eq>(left: A, right: A) -> Bool {
  left == right
}
```

Several bounds:

```khora
fn render<A: Eq + Show>(value: A) -> String {
  value.show()
}
```

General form:

```text
A: Trait + OtherTrait
```

Each bound is a trait path.

## Const generic parameters

```khora
pub type Matrix<A, const Rows: Int, const Cols: Int>;
```

General form:

```text
const Name: Type
```

Integer literals can be supplied as type arguments:

```khora
Matrix<Float, 4, 4>
Embedding<1536>
```

A const generic is part of a type's compile-time identity; it is distinct from a module-level `const` declaration.

## Row-variable parameters

```khora
fn call<A, B, 'e, 'r>(
  value: A,
  f: A -> B with 'e raises 'r,
) -> B
  with 'e
  raises 'r
{
  f(value)!
}
```

A row variable is introduced directly in the generic parameter list:

```text
'e
'r
```

Its spelling is arbitrary; the position where it is used determines whether it represents capability requirements, failures, or another row-shaped type.

## Open record/capability rows

```khora
{ db: Db | 'e }
```

The named entries are required and the row variable represents any additional entries.

## Higher-kinded use

A generic parameter can be used as a type constructor when its use requires that kind:

```khora
fn construct<F, A>(value: A, make: A -> F<A>) -> F<A> {
  make(value)
}
```

Here `F` is used with one type argument, so it is a type constructor rather than an inhabited concrete type. Kinds are checked from how parameters are used; Khora does not require a separate source-level kind annotation on this declaration.

## Explicit `forall`

A type value can quantify its own parameters:

```khora
forall<A>. A -> A
```

Several parameters:

```khora
forall<A, B>. (A, B) -> (B, A)
```

Const parameter:

```khora
forall<A, const N: Int>. A -> Vector<A>
```

General form:

```text
forall<TypeParams>. Type
```

Named generic declarations introduce their parameters directly and usually do not need explicit `forall`.

## Variance annotations

Covariant parameter:

```khora
pub type Source<+A>;
```

Contravariant parameter:

```khora
pub type Sink<-A>;
```

Invariant parameter:

```khora
pub type Cell<A>;
```

General forms:

```text
+A
-A
A
```

Variance is written on the parameter declaration itself.

## Generic implementations

```khora
impl<A: Show> Show for Box<A> {
  fn show(self) -> String {
    self.value.show()
  }
}
```

Type parameters immediately after `impl` are in scope for the implemented trait, target type, associated types, and methods in that block.

## Generic effects and traits

Effects and traits may also take type parameters:

```khora
pub effect Cache<K, V> {
  get: K -> Option<V>,
}

pub trait Convert<A> {
  fn convert(self) -> A;
}
```

The parameter grammar is shared across functions, types, traits, effects, and implementation blocks.

See [Traits](./traits.md) for associated types and implementations and [Types](./types.md) for function types, rows, and `forall` in context.