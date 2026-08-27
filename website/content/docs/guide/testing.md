---
title: Testing and benchmarks
sidebar:
  order: 12
---

Khora has first-class `test` and `bench` declarations. Tests run with `khora test`; benchmarks run with `khora bench`. Both live beside normal source rather than depending on a naming convention for functions.

## Write a test

A test has a string name and a block:

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

Run package tests from the package root:

```bash
khora test .
```

Filter by a substring of the test name:

```bash
khora test . --filter double
```

## Test pure functions directly

Pure functions need ordinary inputs and assertions over their returned values:

```khora
test "discount never goes below zero" {
  assert(discount(10, 20) == 0);
}
```

Keeping domain logic pure when it naturally can be keeps these tests small and deterministic.

## Test typed failures

Use the same `catch` or `attempt` syntax application code uses:

```khora
test "missing users are reported" {
  let result = attempt(fn () => load_user(999)!);

  match result {
    Result::Ok(_) => assert(false),
    Result::Err(UserError::NotFound(id)) => assert(id == 999),
  }
}
```

See [Typed failure with raises](./errors-and-raises/) for the failure model.

## Test capabilities with handlers

Effectful code can receive controlled handlers instead of reaching real external services:

```khora
const fixed_clock = handler for Clock {
  now: fn _ => fixed_instant,
};

test "session uses the supplied clock" {
  let session = create_session(user) with {
    clock: fixed_clock,
  };

  assert(session.created_at == fixed_instant);
}
```

The function's `with` row tells the test exactly which outside authority must be supplied. Named contexts can provide a normal application environment with one binding overridden for the test.

## Write a benchmark

A benchmark uses the same named-block shape with `bench`:

```khora
bench "parse representative payload" {
  parse_payload(fixture)!;
}
```

Run all benchmarks:

```bash
khora bench .
```

Or select matching names:

```bash
khora bench . --filter payload
```

The benchmark runner compiles and times `bench` blocks. Keep setup that is not part of the measurement outside the operation you intend to compare when the benchmark design allows it.

## Test failure and cancellation paths

Khora's strongest guarantees matter when control flow does not return normally. Tests for resources, transactions, and concurrent code should include successful completion, typed failure, cancellation where applicable, and the cleanup or rollback behavior required on those paths.

## Use the same commands in CI

A useful baseline is:

```bash
khora fmt . --check
khora check .
khora test .
```

Projects that rely on performance budgets can run selected `khora bench` workloads separately from correctness CI.

For concurrent tests, continue with [Fibers and nurseries](./fibers-and-nurseries/) and [Resources and regions](./resources-and-regions/).