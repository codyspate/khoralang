---
title: Testing capabilities
sidebar:
  order: 8
---

A capability requirement is an explicit seam for tests. Instead of mutating process-global state or adding test-only branches to application code, provide a small handler for the capability the function already requires.

## Complete example

This session function depends on the current clock. The production program can install `Clock::real()`, while the test installs a deterministic handler:

```khora
module session_test;

import std::core::{assert};
import std::env::{Clock};

pub type Session = {
  user_id: Int,
  created_at: Int,
};

fn create_session(user_id: Int) -> Session
  with { clock: Clock }
{
  {
    user_id: user_id,
    created_at: clock.unix_millis(),
  }
}

const fixed_clock = handler for Clock {
  unix_seconds: fn () => 1700000000,
  unix_millis: fn () => 1700000000000,
  monotonic_millis: fn () => 5000,
};

test "session uses the supplied clock" {
  let session = create_session(42) with {
    clock: fixed_clock,
  };

  assert(session.user_id == 42);
  assert(session.created_at == 1700000000000);
}
```

Run it with the normal test runner:

```bash
khora test .
```

The subject under test did not change for the test. Its contract already says exactly what the test must provide:

```khora
fn create_session(user_id: Int) -> Session
  with { clock: Clock }
```

## Production wiring uses the same boundary

Application startup can install the real clock around the same function:

```khora
with { clock: Clock::real() } {
  let session = create_session(user_id);
  // ...
}
```

There is no global clock singleton and no `if testing` branch inside `create_session`.

## Test only the operations you need

A handler must satisfy the capability's declared operations, but it does not need to reproduce the complexity of the real system. A deterministic clock can return constants. An in-memory repository can store a small test state. A tracer can record finished spans instead of exporting them.

This keeps test doubles focused on the contract the caller observes rather than building a second production implementation.

## Override one capability in a larger context

When application tests use a named context, replace only the dependency relevant to the test:

```khora
let session = create_session(42) with Production {
  clock: fixed_clock,
};
```

The rest of `Production` remains unchanged while the clock binding is overridden for that expression.

See [Effects and capabilities](/docs/guide/effects-and-capabilities/) for handler and context syntax, and [Testing and benchmarks](/docs/guide/testing/) for `test`, filtering, and CI commands.
