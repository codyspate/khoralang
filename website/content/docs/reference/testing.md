---
title: Testing and benchmarks
sidebar:
  order: 19
---

`test` and `bench` are declarations, not a naming convention, and they live
beside ordinary source. Tests run with `khora test`; benchmarks run with
`khora bench`. The declaration syntax is in
[Declarations](./declarations/#tests); this page is how they are used.

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

Run a package's tests from its root:

```bash
khora test .
```

Filter by a substring of the test name:

```bash
khora test . --filter double
```

## Typed failures

Use the same `catch` or `attempt` that application code uses:

```khora
test "missing users are reported" {
  let result = attempt(fn () => load_user(999)!);

  match result {
    Result::Ok(_) => assert(false),
    Result::Err(UserError::NotFound(id)) => assert(id == 999),
  }
}
```

[Failures](./failures/) is the model behind it.

## Capabilities, supplied by the test

Effectful code takes controlled handlers instead of reaching real services:

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

The function's `with` row tells the test exactly which outside authority has to
be supplied — there is nothing to discover by reading the body. A named context
can supply an ordinary application environment with one binding overridden for
the test; see [Capabilities](./capabilities/#override-named-context-entries).

`sleep` is one of the clock's operations, so that four-line handler is also how
a test drives a retry loop or a poller without waiting for it. There is no test
runtime and no special mode.

## Failure and cancellation paths

Khora's strongest guarantees are about control flow that does not return
normally, so a test for a resource, a transaction or concurrent work should
cover successful completion, typed failure, cancellation where it applies, and
the cleanup or rollback each of those paths owes. [Cancellation-safe
resources](/docs/cookbook/cancellation-safe-resources/) is that written out.

## Write a benchmark

`bench` takes the same named-block shape:

```khora
bench "parse representative payload" {
  parse_payload(fixture)!;
}
```

```bash
khora bench .
khora bench . --filter payload
```

Keep setup that is not part of the measurement outside the operation being
compared, where the benchmark's design allows it.

### Benchmarks build unoptimized unless told otherwise

Like everything else the toolchain compiles — a language being brought up
should give a readable crash by default. There is no `--release` flag here;
`khora test` and `khora bench` read `KHORA_PROFILE`, because a flag on every
subcommand is three ways to say one thing:

```bash
KHORA_PROFILE=release khora bench .
```

`khora bench` prints the profile it used above the results, so a number pasted
into an issue carries that with it. A small integer loop differs by about a
factor of two between the two profiles; code the optimizer can see through
differs by much more.

**And some workloads differ by nothing measurable.** Code whose time goes on
allocation, reference counting or I/O spends it in the runtime, which is
compiled once and is the same either way. A release build buying nothing is
information about where the time is going, not a sign the profile did not take.
[Performance](/docs/performance/) is the methodology.

## In CI

A useful baseline:

```bash
khora fmt . --check
khora check .
khora test .
```

A project with performance budgets can run selected `khora bench` workloads
separately from correctness CI — with `KHORA_PROFILE=release`, or the budget is
measured against a build nobody ships.
