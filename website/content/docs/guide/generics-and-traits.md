---
title: Generics and traits
sidebar:
  order: 5
---

Generics let one definition work across many types while preserving static checking. Traits describe behavior a type can provide.

Khora supports higher-kinded types and trait-constrained generics. The compiler monomorphizes generic code for the concrete types a program uses, so generic abstraction does not require dictionary-passing in generated machine code.

Use a generic parameter when an implementation truly does not care about the concrete type. Add a trait bound when the implementation needs a specific operation from that type.

Common traits include `Eq`, `Ord`, `Show`, `Hash`, `ToJson`, and `FromJson`. Many data types can derive these rather than implementing them manually.

Trait visibility matters: operator or method resolution can only use an implementation whose trait is in scope where the operation is written. If an operator appears valid but the compiler says no implementation is reachable, check the relevant imports as well as the type's implementations.

Prefer the smallest useful bound. A function that only needs equality should ask for `Eq`, not for a larger interface merely because the current caller has one.
