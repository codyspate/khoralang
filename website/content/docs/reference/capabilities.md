---
title: Capabilities
sidebar:
  order: 11
---

Capabilities represent authority required by a computation. A function declares requirements with a `with` row; handlers satisfy those requirements for an expression or lexical block.

## Capability row on a declaration

```khora
fn load_user(id: Id) -> User
  with { store: Store }
  raises StoreError
{
  store.load(id)!
}
```

The row entry has a label and an effect type:

```text
store: Store
```

The label `store` is in scope in the function body.

The call mirrors the declaration. The function writes `with { store: Store }`
to say what it needs; the caller writes `with { store: memory_store }` to
supply it. Read the first as *this function requires a capability named `store`
implementing the `Store` effect*.

## Several capabilities

```khora
fn create_session(id: Id) -> Session
  with { store: Store, clock: Clock }
  raises StoreError
{
  let user = store.load(id)!;
  Session::new(user, clock.now())
}
```

## Open capability row

```khora
{ clock: Clock | 'ef }
```

The named capability is required and `'ef` represents any additional row entries.

A function can preserve the open row:

```khora
fn run<A, 'ef>(body: () -> A with 'ef) -> A
  with 'ef
{
  body()
}
```

## Handler values

```khora
let fixed_clock = handler for Clock {
  now: fn () => fixed_instant,
};
```

A handler's type is the effect it implements.

## Installing the real thing

The handlers above are invented, because a page about syntax should not depend
on which effects `std` happens to ship. Real programs install the ones it does,
and each capability's own module provides them:

```khora
import std::clock::{Clock};
import std::env::{Env};
import std::fs::{FsRead, FsWrite};
import std::log::{Level, Log};

pub fn main() -> Int {
  with {
    env: Env::real(),
    reads: FsRead::real(),
    writes: FsWrite::real(),
    clock: Clock::real(),
    log: Log::json(Level::Info),
  } {
    work()
  }
}
```

**The label is chosen by the function you are calling, not by you.** A row entry
named `fs:` will not satisfy a function that declared `reads: FsRead`; the error
says so — ``needs `reads: FsRead`, which this function does not require`` — but
it is worth knowing before you meet it, because the labels are effectively part
of `std`'s public API:

| Capability | Label | From |
| --- | --- | --- |
| `Env` | `env` | [`std::env`](/docs/stdlib/api/env/) |
| `FsRead` | `reads` | [`std::fs`](/docs/stdlib/api/fs/) |
| `FsWrite` | `writes` | [`std::fs`](/docs/stdlib/api/fs/) |
| `Clock` | `clock` | [`std::clock`](/docs/stdlib/api/clock/) |
| `Random` | `random` | [`std::random`](/docs/stdlib/api/random/) |
| `Log` | `log` | [`std::log`](/docs/stdlib/api/log/) |
| `Tracer` | `tracer` | [`std::trace`](/docs/stdlib/api/trace/) |
| `Db` | `db` | [`std::db`](/docs/stdlib/api/db/) |
| `HttpClient` | `client` | [`std::net::http`](/docs/stdlib/api/net/http/) |
| `Process` | `process` | [`std::process`](/docs/stdlib/api/process/) |
| `Nursery` | `nursery` | [`std::core`](/docs/stdlib/api/core/) |
| `Scope` | `scope` | [`std::core`](/docs/stdlib/api/core/) |

Every `real()` reaches the actual machine. A test installs a handler of its own
instead — see [Testing capabilities](/docs/cookbook/testing-capabilities/),
which is what the seam is for.

## Postfix `with`

Install handlers for one expression:

```khora
let user = load_user(id)! with {
  store: memory_store,
};
```

General form:

```text
Expr with { label: HandlerExpr, ... }
```

The installation is postfix, so it applies to the expression immediately before it.

## `with` block

Install handlers for a lexical region:

```khora
with {
  store: memory_store,
  clock: fixed_clock,
} {
  let user = load_user(id)!;
  create_session(user)
}
```

General form:

```text
with ContextRow Block
```

Handlers lexically enclose the operations they serve. That is a real
consequence of direct style rather than a syntactic detail: the operation runs
when the call is evaluated, inside the block, and not later through a deferred
effect value that outlived its handler.

## Sequential bindings

Bindings inside a context row are sequential. A later expression may use handlers introduced above it:

```khora
with {
  config: env_config(),
  scope: Scope::root(),
  db: postgres_db()!,
  store: sql_store(),
} {
  run_server()!
}
```

This allows service construction to remain flat rather than nesting one installation block per dependency.

## Named context declaration

```khora
pub context Production {
  config: env_config(),
  scope: Scope::root(),
  db: postgres_db()!,
  store: sql_store(),
}
```

General form:

```text
pub? context Name {
  label: Expr,
  ...
}
```

## Use a named context

Postfix:

```khora
load_user(id)! with Production
```

Block:

```khora
with Production {
  run_server()!
}
```

## Override named-context entries

```khora
load_user(id)! with Production {
  store: test_store,
}
```

or around a block:

```khora
with Production {
  store: test_store,
} {
  run_test_case()!
}
```

Entries written at the use site replace or extend the corresponding context row
for that installation. This is what makes a named production context usable
from a test: install `Production` and override the one capability the test
wants to control.

## Capability rows on function values

```khora
Request -> Response with { db: Db, clock: Clock }
```

Generic row:

```khora
A -> B with 'ef
```

Capability rows are part of function types, so higher-order functions can preserve requirements without a runtime service locator.

## Capabilities do not imply failure

```khora
fn choose_bucket() -> Int
  with { random: Random }
{
  random.in_range(0, 10)
}
```

A capability may be required by an operation that does not raise a recoverable failure. Conversely, a pure computation may use `raises` without any `with` requirement.

## Manifest permissions

A capability row says a function reaches outside the program. `[permissions]` in `khora.toml` says how far it may reach, and the standard library's real handlers enforce it:

```toml
[permissions]
network = ["api.example.com:443", "*.internal"]
env = ["PORT", "DATABASE_*"]

[permissions.fs]
read = ["./data", "./data/**"]
write = ["./data/out.txt"]
```

Both `read` entries, and the reason is the one surprise in the glob dialect
below: `./data/**` grants what is *inside* `data` and not `data` itself, so with
only that line a program can read every file in the directory and cannot list
it. `read_dir("data")` and `is_dir("data")` both raise `Denied`. The probes raise rather than answering `false` for the reason given further down: a `false` that could mean "not there", "unreadable" or "not granted" is the one somebody debugs for an hour.

The grants are compiled into the binary rather than read at run time. A file the program consults for its own permissions is a file an attacker edits.

**A missing table grants everything, and each category is independent.** Naming `network` says nothing about `env`. Tightening is opt-in.

A denial is its own error case, separate from the ordinary failure, because the two send a reader to different files:

| Reaching for | Denied as | Where the fix is |
| --- | --- | --- |
| an environment variable | `EnvError::Denied(name)` | `[permissions] env` |
| a host over HTTP | `CallError::Denied(host)` | `[permissions] network` |
| a path on disk | `IoError::Denied(path)` | `[permissions.fs]` |

**Including the two probes.** `FsRead::exists` and `FsRead::is_dir` answer a
`Bool` and raise `IoError` as well, so a path the manifest denies is `Denied`
rather than `false`. They answered a plain `Bool` until recently, which made
three situations one word — not there, there but unreadable, and there and
readable and simply not granted — and the third is the one somebody debugs for
an hour, because nothing in a `false` points at a manifest. The remaining
`false` means one thing: the operating system would not open it.

That combines with the glob rule above, which is where it matters most.
`./data/**` does not grant `data`, so `is_dir("data")` raises `Denied` for a
directory whose every file the program can read — the manifest is what has to
change, and the message says so.

`Unreachable` is DNS or a firewall; `Denied` is a line you can copy out of the message into the manifest. `Env::variable` and `std::env::variable_or` therefore `raise EnvError`, so both need a `!` at the call site:

```khora
let port = variable_or("PORT", "8080")!;
```

Globbing differs by category, and each one is the reading that costs the least surprise. For a path, `*` stops at a separator and `**` crosses one — and, as in `.gitignore`, **neither covers the directory being described**: `data/**` is a grant over the contents of `data`, so listing `data` needs `data` named as well. For a name — a variable, a command — there are no segments, so `*` spans everything. For a host, `*` spans dots, so `*.internal` covers `db.eu.internal`, and a grant with no port covers every port.

See [Effects and rows](./effects/) for effect declarations and [Failures](./failures/) for typed failure.