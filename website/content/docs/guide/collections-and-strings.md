---
title: Collections and strings
sidebar:
  order: 13
---

Khora's standard library provides persistent-style collection APIs and Unicode strings while the compiler/runtime optimize ownership and reuse where they can prove it is safe.

The programmer model stays functional: transforming a collection produces the next value. You do not write ownership annotations merely to let the compiler reuse storage.

## Strings

Strings are text values, not byte arrays with a text-shaped API. String interpolation is available for ordinary formatting, while structured encoders such as JSON should be used for machine-readable output.

Use `Show` for human-readable representations and dedicated encoding traits such as `ToJson`/`FromJson` when an external format has a contract of its own.

## Collection transforms

Prefer high-level transforms when they communicate intent clearly. Whole-program ownership analysis may turn some apparently persistent transformations into in-place reuse when the input is uniquely owned.

That optimization is deliberately invisible to source code: the meaning of the program does not depend on whether storage was reused.

## Shared collections

If several fibers need to coordinate around one evolving collection, place the collection behind an explicit `Shared` boundary or an external capability rather than smuggling mutation into otherwise pure code.
