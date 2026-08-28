---
title: Effects and capabilities
sidebar:
  order: 8
---

Khora uses effects and capabilities to make external authority visible in function types without turning application code into explicit plumbing. A function says what authority it needs with `with`; a handler supplies that authority for a call or lexical scope.

## Declare an effect

An effect is a named set of operations:

```khora
pub effect Clock {
  now: () -> Instant,
}

pub effect Store {
  load: UserId -> User raises StoreError,
  save: User -> () raises StoreError,
}
```

An operation can be pure from the caller's point of view or declare its own typed failures with `raises`.

`Clock` here is a stand-in cut down to one operation. The real one is [`std::clock::Clock`](/docs/stdlib/api/clock/) — it lives in its own module rather than in `std::env`, and it has four operations, including `sleep`. Waiting is an operation on the capability on purpose: a fake clock is `handler for Clock { sleep: fn _ms => (), .. }` and nothing else, so a test that exercises a retry loop finishes instantly.

## Require a capability with `with`

A function's capability row names the capabilities available in its body:

```khora
pub fn load_user(id: UserId) -> User
  with { store: Store }
  raises StoreError
{
  store.load(id)!
}
```

Read `with { store: Store }` as: this function requires a capability named `store` that implements the `Store` effect.

Multiple capabilities live in the same row:

```khora
fn create_session(id: UserId) -> Session
  with { store: Store, clock: Clock }
  raises StoreError
{
  let user = store.load(id)!;
  Session::new(user, clock.now())
}
```

Capability labels such as `store` and `clock` are ordinary names in the function body. The effect types (`Store`, `Clock`) describe the operations behind those names.

## Build a handler

`handler for Effect { ... }` creates a value that implements an effect:

```khora
let fixed_clock = handler for Clock {
  now: fn () => Instant::from_unix_seconds(0),
};
```

A handler may raise the failures declared by the operation it implements:

```khora
let memory_store = handler for Store {
  load: fn id => find_user(id)! catch {
    LookupError::Missing(_) => raise StoreError::NotFound(id),
  },
  save: fn user => persist(user)!,
};
```

Handlers are ordinary values, so they can be stored in constants, returned from functions, or assembled into named contexts.

## Supply capabilities to one expression

Use postfix `with` when one expression needs a set of handlers:

```khora
let user = load_user(id)! with {
  store: memory_store,
};
```

The call mirrors the declaration: the function says `with { store: Store }` to state what it needs, and the caller supplies `with { store: memory_store }`.

## Supply capabilities to a block

Use a `with` block when several expressions share the same capabilities:

```khora
with {
  store: memory_store,
  clock: fixed_clock,
} {
  let user = load_user(id)!;
  let session = create_session(id)!;
  print(session.id);
}
```

The handlers lexically enclose the code they serve. This matters in direct style: the effectful operation runs when the call is evaluated, not later through a deferred effect value.

## Handlers can depend on earlier capabilities

Bindings in a `with` row are sequential. A handler may use bindings declared above it:

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

If `postgres_db()` requires `config` and `scope`, and `sql_store()` requires `db`, the build order is simply the order shown.

## Named contexts

Use `context` to name a reusable bundle of handlers:

```khora
pub context Production {
  config: env_config(),
  scope: Scope::root(),
  db: postgres_db()!,
  store: sql_store(),
}
```

Install it by name:

```khora
let user = load_user(id)! with Production;
```

Or use it around a block:

```khora
with Production {
  run_server()!
}
```

## Override a context binding

A named context can be extended or overridden at the use site, which is especially useful in tests:

```khora
let user = load_user(id)! with Production {
  store: stub_store,
};
```

`Production` supplies the other bindings while `store` is replaced for this expression.

## Open capability rows

Reusable higher-order APIs can be polymorphic over additional capabilities with a row variable:

```khora
fn run<A, 'ef>(body: () -> A with 'ef) -> A
  with 'ef
{
  body()
}
```

A record row can also keep named entries while accepting an open tail:

```khora
{ clock: Clock | 'ef }
```

See [Generics and traits](./generics-and-traits/#failure-and-capability-row-variables) for row variables in higher-order function types.

## Capabilities and failures are separate

`with` answers what authority a computation needs. `raises` answers what recoverable failures it may produce:

```khora
fn determine_random() -> Bool
  with { random: Random }
  raises RandomFailure
```

The function needs access to randomness and may fail with `RandomFailure`; neither fact implies the other.

For explicit `raise`, propagation with `!`, pattern-based `catch`, error translation, and `attempt`, continue with [Typed failure with raises](./errors-and-raises/).