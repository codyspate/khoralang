---
title: Capabilities
sidebar:
  order: 10
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

Handlers lexically enclose the operations they serve.

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

Entries written at the use site replace or extend the corresponding context row for that installation.

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
it. `read_dir("data")` raises `Denied` and `is_dir("data")` answers `false`.

The grants are compiled into the binary rather than read at run time. A file the program consults for its own permissions is a file an attacker edits.

**A missing table grants everything, and each category is independent.** Naming `network` says nothing about `env`. Tightening is opt-in.

A denial is its own error case, separate from the ordinary failure, because the two send a reader to different files:

| Reaching for | Denied as | Where the fix is |
| --- | --- | --- |
| an environment variable | `EnvError::Denied(name)` | `[permissions] env` |
| a host over HTTP | `CallError::Denied(host)` | `[permissions] network` |
| a path on disk | `IoError::Denied(path)` | `[permissions.fs]` |

**Two of these cannot reach you.** `FsRead::exists` and `FsRead::is_dir` answer
a plain `Bool` and have no way to raise, so a path the manifest denies is
reported exactly as one that is not there. A `false` from either means "not
readable by this program" and nothing more precise; when the difference matters,
`read` and `read_dir` raise and say which it was.

`Unreachable` is DNS or a firewall; `Denied` is a line you can copy out of the message into the manifest. `Env::variable` and `std::env::variable_or` therefore `raise EnvError`, so both need a `!` at the call site:

```khora
let port = variable_or("PORT", "8080")!;
```

Globbing differs by category, and each one is the reading that costs the least surprise. For a path, `*` stops at a separator and `**` crosses one — and, as in `.gitignore`, **neither covers the directory being described**: `data/**` is a grant over the contents of `data`, so listing `data` needs `data` named as well. For a name — a variable, a command — there are no segments, so `*` spans everything. For a host, `*` spans dots, so `*.internal` covers `db.eu.internal`, and a grant with no port covers every port.

See [Effects and rows](./effects/) for effect declarations and [Failures](./failures/) for typed failure.