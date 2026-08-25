---
title: Patterns
sidebar:
  order: 3
---

Patterns appear in `match` arms and in irrefutable destructuring positions such as `let` bindings.

A `match` pattern may select an algebraic-data-type variant and bind its payload, destructure tuples or records, and nest those forms where the value's type permits it.

The compiler checks match exhaustiveness and arm reachability. A match over a closed ADT should normally enumerate the meaningful cases rather than use a catch-all solely to avoid future compiler errors.

Irrefutable patterns are accepted where the type guarantees the shape always matches. Refutable patterns belong in control flow that has an explicit non-match path.

Adding a new ADT variant can intentionally make existing exhaustive matches fail to compile; those diagnostics identify sites that must decide what the new case means.
