---
title: Language guide
sidebar:
  order: 0
---

The Khora Guide teaches the language from the programmer's point of view. It is meant to be read in order the first time and used as a set of practical explanations afterward.

If you have not built a Khora program yet, start with [Getting Started](/docs/getting-started/) and come back here once the basic toolchain workflow works.

## Start with the core language

1. [Values and functions](/docs/guide/values-and-functions/) — expressions, immutable bindings, functions, and return values.
2. [Data types](/docs/guide/data-types/) — records, algebraic data types, constructors, and modeling data explicitly.
3. [Pattern matching](/docs/guide/pattern-matching/) — destructure values and make branching exhaustive.
4. [Collections and strings](/docs/guide/collections-and-strings/) — work with the everyday data structures used by applications.
5. [Generics and traits](/docs/guide/generics-and-traits/) — write reusable code while keeping behavior statically constrained.
6. [Pipelines](/docs/guide/pipelines/) — compose transformations with Khora's `|>` call-insertion syntax.

After these pages you should be comfortable reading ordinary pure Khora code.

## Learn typed failure and capabilities

7. [Errors and raises](/docs/guide/errors-and-raises/) — model recoverable failure in function types and propagate it with `!`.
8. [Effects and capabilities](/docs/guide/effects-and-capabilities/) — make external authority visible with `with` rows and provide it through handlers.

These are separate dimensions of a function type: `raises` says what recoverable failures may occur; `with` says what external capabilities the computation requires.

## Add resources and concurrency

9. [Resources and regions](/docs/guide/resources-and-regions/) — scope resource lifetimes and cleanup.
10. [Fibers and nurseries](/docs/guide/fibers-and-nurseries/) — run concurrent work without detached lifetime management.
11. [Shared state](/docs/guide/shared-state/) — make intentional shared mutable state explicit.

Khora's concurrency model is structured: child work belongs to a scope, cancellation participates in cleanup, and resource lifetimes remain visible in the program model.

## Build packages and tests

12. [Testing](/docs/guide/testing/) — write `test` blocks and test effectful code with controlled capabilities.
13. [Modules and packages](/docs/guide/modules-and-packages/) — organize code, imports, dependencies, lockfiles, and public package APIs.

## Guide or reference?

Use the **Guide** when you want to understand a feature, see ordinary patterns, or decide how to structure code.

Use the [Language Reference](/docs/reference/) when you already know what you are looking for and need the exact syntax or semantic rule. Use the [Standard Library](/docs/stdlib/) when you need a concrete API.

If you are coming from TypeScript + Effect, Go, or Rust, read the core Guide first, then use the migration pages to map familiar concepts onto Khora rather than treating the other language as Khora's mental model.
