---
title: From TypeScript + Effect
sidebar:
  order: 1
---

If you know Effect, Khora's motivation should feel familiar: failures and capabilities belong in types, resource lifetimes should be structured, and concurrent work should have ownership.

The major difference is where that model lives. Khora builds those ideas into a native language/runtime rather than expressing them as a TypeScript library over the JavaScript/Node execution model.

## Visibility is `pub`, not `export`

The one place the surface will read as *less* familiar than you expect.
`import` is spelled the way you already spell it, but what marks a declaration
public is `pub`:

```khora
pub type Entry = { id: Int, memo: String };
pub fn total(entries: List<Entry>) -> Int { .. }
```

It also appears on methods, which is why it is not `export`: nobody imports
`Map::get`, they reach it by having a `Map`. A declaration without `pub` is
private to its file, which is closer to a module with no `export` than to
TypeScript's default of exporting whatever is written at the top level.

## Direct style

Khora code calls effectful operations directly inside the capability/failure context declared by the function. There is no separate `Effect<A, E, R>` value that application code must construct and compose.

`raises E` corresponds roughly to the typed failure dimension. A `with { ... }` capability row corresponds to environmental authority. Handlers provide capabilities for a scope.

## Pipelines

Khora's `|>` is call-oriented. `value |> f(a)` means `f(value, a)`, and a single `_` placeholder can select another argument position. It is not limited to piping into unary functions.

## Fibers and scopes

Khora fibers and nurseries provide structured concurrency in the language/runtime. Cancellation and finalization remain central concepts, but source code stays ordinary direct-style Khora.

## Memory/runtime

Khora compiles to native code and does not use a tracing GC or JavaScript VM. Automatic memory management is implemented through reference counting plus compiler ownership/reuse analysis rather than through a source-level borrow checker.
