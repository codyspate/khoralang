---
title: Testing
sidebar:
  order: 11
---

Khora tests are declared with `test` blocks and run with `khora test`. Pure code can be tested directly; effectful code can be tested by supplying controlled implementations for the capabilities it requires.

## Write a test

A test can live alongside the code it exercises:

```khora
module pricing_test;

import std::core::{assert};

fn double(value: Int) -> Int {
  value * 2
}

test "double returns twice its input" {
  assert(double(21) == 42);
}
```

Run the package tests from the package root:

```bash
khora test .
```

To focus on matching tests while you work:

```bash
khora test . --filter double
```

## Test pure functions directly

Pure functions need ordinary inputs and assertions over their returned values. Keep business logic pure when it naturally can be; tests then stay small and deterministic.

## Test capabilities at the boundary

Khora's effect and capability model makes tests explicit about the outside world they depend on. Code that requires `Db`, `Clock`, tracing, or another capability should not need a real database, wall clock, or telemetry backend just to exercise domain decisions.

Provide a small deterministic handler or in-memory implementation for the same capability contract, then call the application code normally. The function's `with` row tells the test exactly which external authority must be supplied.

See [Effects and capabilities](/docs/guide/effects-and-capabilities/) for the capability model and [Errors and raises](/docs/guide/errors-and-raises/) for recoverable failures.

## Test behavior, not plumbing

Prefer assertions over returned values and externally visible effects rather than compiler/runtime implementation details. A database transaction test, for example, should verify the result, commit or rollback behavior, and the failure observed by the caller—not internal scheduler choices.

## Include failure and cancellation paths

Khora's strongest guarantees matter when control flow does not return normally. Tests for resources, transactions, and concurrent code should include:

- successful completion;
- typed failure;
- cancellation where the operation can be cancelled;
- cleanup or rollback behavior that must occur on those paths.

For concurrent code, continue with [Fibers and nurseries](/docs/guide/fibers-and-nurseries/) and [Resources and regions](/docs/guide/resources-and-regions/).

## Use the same commands in CI

A useful baseline is:

```bash
khora fmt . --check
khora check .
khora test .
```

That keeps formatting, compiler diagnostics, and tests on the same toolchain developers use locally.
