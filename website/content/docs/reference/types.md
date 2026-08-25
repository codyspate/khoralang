---
title: Types
sidebar:
  order: 2
---

Khora is statically typed with inference for ordinary local expressions and explicit annotations available at API boundaries.

The type system includes primitive types, tuples, records, algebraic data types, parametric generics, higher-kinded types, traits, const generics, and row-polymorphic effects/failures.

## Inference

Local types are inferred using Hindley-Milner-style unification extended with Khora's rows and trait constraints. When inference cannot determine a type because no use constrains it, add an annotation at the binding or call boundary that owns the ambiguity.

## Nominal data and traits

Declared algebraic data types are nominal. Traits state reusable behavioral constraints and can be derived for common operations such as equality, ordering, display, hashing, and JSON conversion where supported.

## Function types

A function type may include capability requirements and recoverable failures in addition to arguments and result:

```khora
fn load(id: Id) -> User with { db: Db } raises DbError
```

These rows participate in type checking and generic composition rather than being comments attached to the function.

## Memory is not a source-level type burden

Khora's automatic memory management and compiler ownership analysis do not add borrow/lifetime parameters to ordinary application types. FFI/thread-affine resources are the major boundary where extra lifetime rules may need explicit API design.
