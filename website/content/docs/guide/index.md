---
title: Language guide
sidebar:
  order: 0
---

The Khora Guide teaches the language from the programmer's point of view. Read it in order the first time, then return to individual pages for practical examples.

If you have not built a Khora program yet, start with [Getting Started](/docs/getting-started/) and come back here once the basic toolchain workflow works.

## Core syntax and data

1. [Values and functions](/docs/guide/values-and-functions/) — `let`, `mut`, `const`, `pub`, functions, lambdas, function types, and return values.
2. [Control flow](/docs/guide/control-flow/) — blocks, `if`, `match`, `while`, `for`, `loop`, `break`, `continue`, and `return`.
3. [Data types](/docs/guide/data-types/) — literals, tuples, records, variants, generic types, and `derive(...)`.
4. [Pattern matching](/docs/guide/pattern-matching/) — wildcard, literal, binding, constructor, tuple, record, guarded, `let`, `for`, and `catch` patterns.
5. [Pipelines](/docs/guide/pipelines/) — `|>`, `_` insertion, fallible stages, and unary flow lambdas with `||>`.
6. [Generics and traits](/docs/guide/generics-and-traits/) — type parameters, bounds, traits, `impl`, associated types, const generics, row variables, `forall`, and variance.

After these pages you should be able to read the syntax of ordinary Khora programs rather than encountering unexplained language forms later in the Guide.

## Typed failure and capabilities

7. [Typed failure with raises](/docs/guide/errors-and-raises/) — `raises`, explicit `raise`, propagation with `!`, pattern-based `catch`, error translation, and `attempt`.
8. [Effects and capabilities](/docs/guide/effects-and-capabilities/) — `effect`, capability rows, `handler`, postfix and block `with`, named `context`, overrides, and open rows.

These are separate dimensions of a function type: `raises` says what recoverable failures may leave the computation; `with` says what capabilities it requires.

## Resources and concurrency

9. [Resources and regions](/docs/guide/resources-and-regions/) — resource acquisition, lexical lifetime, finalization, and cleanup across failure and cancellation.
10. [Fibers and nurseries](/docs/guide/fibers-and-nurseries/) — run concurrent work without detached lifetime management.
11. [Shared state](/docs/guide/shared-state/) — make intentional shared mutable state explicit.

Khora's concurrency model is structured: child work belongs to a scope, cancellation participates in cleanup, and resource lifetimes remain visible in the program model.

## Tests, modules, and everyday library values

12. [Testing and benchmarks](/docs/guide/testing/) — `test`, `bench`, controlled capabilities, failure tests, and the CLI runners.
13. [Modules and packages](/docs/guide/modules-and-packages/) — `module`, grouped/glob/aliased `import`, `pub`, dependencies, lockfiles, and package boundaries.
14. [Collections and strings](/docs/guide/collections-and-strings/) — list literals, `for`, transforms, interpolation, escapes, and multiline backtick strings.

## Guide or reference?

Use the **Guide** when you want to learn a construct in context and see how it is normally used.

Use the [Language Reference](/docs/reference/) when you already know what you are looking for and need the exact accepted forms, precedence, type rules, or compact syntax examples. The Reference is intentionally redundant with the Guide at the syntax level: a language construct should never exist only in prose or only in the parser.

Use the [Standard Library](/docs/stdlib/) when you need the concrete declarations and behavior of a library API.

If you are coming from TypeScript + Effect, Go, or Rust, learn Khora's own syntax and model here first, then use the migration pages to map familiar concepts onto it.