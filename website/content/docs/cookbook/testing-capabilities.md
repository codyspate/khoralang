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
import std::clock::{Clock};

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
  sleep: fn _millis => (),
};

test "session uses the supplied clock" {
  let session = create_session(42) with {
    clock: fixed_clock,
  };

  assert(session.user_id == 42);
  assert(session.created_at == 1700000000000);
}
```

**Keep the double in the same file as the test, or make it a function.** A
`const` has no declared type — its type comes from inferring over its
initializer — and the type map that carries a module's exports is built from
syntax, before anything is inferred. So nothing records what an exported `const`
is, and a file that imports one gets a name with no type behind it.

That is a real limitation rather than a rule with a reason, and it is worth
knowing before you factor a set of doubles into a `fakes` module. A function has
a signature, and a signature is what travels:

```khora
// In another module, and usable from anywhere.
pub fn fixed_clock() -> Clock {
  handler for Clock {
    unix_seconds: fn () => 1700000000,
    unix_millis: fn () => 1700000000000,
    monotonic_millis: fn () => 5000,
    sleep: fn _millis => (),
  }
}
```

Called at the use site — `with { clock: fixed_clock() }` — which also means each
test gets its own, rather than sharing one value.

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

## Waiting costs nothing in a test

`sleep` is an operation on `Clock`, not an ambient function. That one decision is why the handler above makes a retry loop or a poller run instantly:

```khora
sleep: fn _millis => (),
```

Nothing else is involved — no test runtime, no special mode, no rule about which fiber may advance time. Every language with an ambient `sleep` ends up building a parallel clock the runtime knows about, and documentation that opens by warning you to fork the sleeping code or deadlock. Here the capability *is* the seam, which is the same argument `Random::seeded` makes about the other unrepeatable input.

The real clock is `Clock::real()`, and it gives a sleeping fiber's worker back for the whole wait — ten thousand sleeping fibers cost ten thousand entries in a heap rather than ten thousand stacks.

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
