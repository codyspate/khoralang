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
import std::clock::{Clock};

const fixed_clock = handler for Clock {
  unix_seconds: fn () => 1700000000,
  unix_millis: fn () => 1700000000000,
  monotonic_millis: fn () => 5000,
  sleep: fn _millis => (),
};

test "session uses the supplied clock" {
  let session = create_session(user) with {
    clock: fixed_clock,
  };

  assert(session.created_at == 1700000000000);
}
```

The function's `with` row tells the test exactly which outside authority must be supplied. Named contexts can provide a normal application environment with one binding overridden for the test.

`sleep` is one of the clock's operations, so that four-line handler is also how a test runs a retry loop or a poller without waiting for it. There is no test runtime and no special mode.

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

**Benchmarks build unoptimized unless you say otherwise**, like everything else the toolchain compiles — a language being brought up should give you a readable crash by default. There is no `--release` flag here; `khora test` and `khora bench` read `KHORA_PROFILE`, because a flag on every subcommand is three ways to say one thing:

```bash
KHORA_PROFILE=release khora bench .
```

`khora bench` prints which profile it used above the results, so a number pasted into an issue carries that with it. A small integer loop differs by about a factor of two between the two; code the optimizer can see through differs by much more.

**And some workloads differ by nothing measurable.** Code whose time goes on allocation, reference counting or I/O spends it in the runtime, which is compiled once and the same either way. If a release build buys you nothing, that is information about where the time is going rather than a sign the profile did not take.

## Test failure and cancellation paths

Khora's strongest guarantees matter when control flow does not return normally. Tests for resources, transactions, and concurrent code should include successful completion, typed failure, cancellation where applicable, and the cleanup or rollback behavior required on those paths.

## Use the same commands in CI

A useful baseline is:

```bash
khora fmt . --check
khora check .
khora test .
```

Projects that rely on performance budgets can run selected `khora bench` workloads separately from correctness CI — with `KHORA_PROFILE=release`, or the budget is measured against a build nobody ships.

For concurrent tests, continue with [Fibers and nurseries](./fibers-and-nurseries/) and [Resources and regions](./resources-and-regions/).