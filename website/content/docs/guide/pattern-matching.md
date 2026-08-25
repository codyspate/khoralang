---
title: Pattern matching
sidebar:
  order: 3
---

Pattern matching is how Khora safely opens algebraic data types and destructures values.

A `match` describes what to do for each shape a value may have. The compiler checks exhaustiveness and reachability, so missing cases and impossible later arms are diagnostics rather than production surprises.

Use matching when behavior depends on which variant you have. Use irrefutable destructuring in `let` when the shape is guaranteed by the type.

The important rule is that the type defines the possible cases; a match should not need a default branch merely to silence the compiler. If a new variant is later added, exhaustive matches become useful migration points because the compiler identifies the code that needs a decision.

Patterns may bind payloads from variants, destructure tuples and records, and nest where the underlying value nests. Prefer clear, shallow patterns over encoding large amounts of business logic in one match arm header.
