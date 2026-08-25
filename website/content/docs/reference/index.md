---
title: Language reference
sidebar:
  order: 0
---

This section is the precise lookup-oriented reference for Khora. The Guide teaches the language in a useful order; the Reference records the rules a programmer can rely on.

## Core syntax

Khora is expression-oriented. Functions use `fn`, immutable bindings use `let`, compile-time paths use `::`, and runtime field projection uses `.`.

Operator precedence from loosest to tightest is:

1. assignment `=` (right-associative)
2. pipeline `|>`
3. `||`
4. `&&`
5. comparisons
6. `+ -`
7. `* / %`
8. prefix `- !`
9. calls and field access

`|>` passes its left value as the first argument of a call unless the stage contains one `_` placeholder selecting another argument position.

## Types

The language includes primitive values, tuples, records, algebraic data types, generic types, higher-kinded types, and traits. Type inference is Hindley-Milner-style with row polymorphism for effects and failures.

## Effects and failure

`with` rows state capability requirements. `raises` rows state typed recoverable failures. `!` propagates a declared failure to the caller. Handlers provide effects for a scope.

## Memory and concurrency

Memory management is automatic from the programmer's perspective. The compiler/runtime use reference counting and ownership/reuse analysis without exposing a Rust-style borrow checker in ordinary source.

Concurrency is structured around fibers and nurseries. Cancellation is distinct from ordinary safepoint preemption and must run structured cleanup.

## Authoritative grammar

The compiler repository's `docs/grammar.ebnf` is the current implemented grammar while these public reference pages are being expanded. Before public release, this section must be complete enough that users do not need internal design documents for ordinary language questions.
