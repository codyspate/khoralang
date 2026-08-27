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
{ clock: Clock | 'e }
```

The named capability is required and `'e` represents any additional row entries.

A function can preserve the open row:

```khora
fn run<A, 'e>(body: () -> A with 'e) -> A
  with 'e
{
  body()
}
```

## Handler values

```khora
let fixed_clock = handler for Clock {
  now: fn _ => fixed_instant,
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
  scope: Scope::root,
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
  scope: Scope::root,
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
A -> B with 'e
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

See [Effects and rows](./effects.md) for effect declarations and [Failures](./failures.md) for typed failure.