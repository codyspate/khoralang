---
title: Expressions
sidebar:
  order: 5
---

Khora is expression-oriented: blocks, calls, control flow, matches, pipelines, and operators participate in producing values according to their types.

Function calls use ordinary positional arguments. Field projection uses `.`, while compile-time paths use `::`.

The pipeline operator rewrites the flow of a value into a call without introducing a separate runtime abstraction. `x |> f(a)` passes `x` first; one `_` placeholder may choose another argument position.

`!` is postfix failure propagation at a fallible call site. Prefix `!` remains boolean negation; their positions disambiguate them.

Blocks evaluate to their final expression when a value is required. `let` introduces a binding; assignment is deliberately low precedence and right-associative.

`match` is checked for exhaustiveness and unreachable arms. Loops and imperative forms exist for algorithms where they are clearer, while the language's default value model remains functional.
