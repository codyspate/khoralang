---
title: Data types
sidebar:
  order: 2
---

Khora's core data model is built from primitive values, tuples, records, and algebraic data types.

## Primitive values

The standard numeric types include `Int` and IEEE `Float`. Exact base-10 arithmetic is provided by `Decimal` for values such as money where binary floating-point is the wrong representation.

Strings are Unicode text values. `Bool` represents `true` and `false`; `()` is the unit value used when a computation has no meaningful result value.

## Tuples and records

Tuples group values positionally. Records group named fields. Prefer records when names carry domain meaning or when a value will cross an API boundary.

## Algebraic data types

ADTs let a type enumerate the valid shapes a value can have. They are the normal way to model domain alternatives without nullable sentinel values or unchecked string tags.

An option-like value, for example, has two meaningful cases: a value exists or it does not. A result-like value has success and failure cases. Pattern matching makes callers account for the cases explicitly.

## Derivation

Khora can derive common traits such as equality, ordering, display, hashing, and JSON encoding/decoding where the data supports them. Derivation is preferable to handwritten boilerplate because the compiler can keep behavior aligned with the type definition.

## Exact decimals

`Decimal` is intentionally distinct from `Float`. Decimal values preserve base-10 meaning and scale, so values such as `1.50` can remain `1.50` when displayed while still comparing numerically with equivalent scales.

Use `Float` for approximate numerical computation and `Decimal` when exact decimal representation is part of correctness.
