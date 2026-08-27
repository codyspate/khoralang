---
title: Effects and rows
sidebar:
  order: 9
---

Khora functions can carry two orthogonal row-polymorphic dimensions in addition to parameters and return values:

```text
with    capabilities required by the computation
raises  recoverable failures that may leave the computation
```

They are part of the function type and are checked by the compiler.

## Effect declaration

```khora
pub effect Clock {
  now: () -> Instant,
}
```

An effect with fallible operations:

```khora
pub effect Store {
  load: Id -> User raises StoreError,
  save: User -> () raises StoreError,
}
```

General form:

```text
pub? effect Name<TypeParams>? {
  operation: FunctionType,
  ...
}
```

An effect name is the type of handlers implementing that effect.

## Capability requirement on a function

```khora
fn load_user(id: Id) -> User
  with { store: Store }
  raises StoreError
{
  store.load(id)!
}
```

`with { store: Store }` introduces the label `store` into the body with the operations of `Store`.

## Several capabilities

```khora
fn build_report(id: Id) -> Report
  with { store: Store, clock: Clock }
  raises StoreError
{
  // ...
}
```

## Open capability row

```khora
fn use_clock<A, 'e>(body: () -> A with { clock: Clock | 'e }) -> A
  with { clock: Clock | 'e }
{
  body()
}
```

A row variable preserves additional requirements not interpreted by the generic function.

## Failure row

Single type:

```khora
raises StoreError
```

Several failure types:

```khora
raises StoreError + ValidationError + HttpError
```

Open failure row:

```khora
raises 'r
```

## Effects on function types

```khora
Request -> Response with { db: Db }
```

```khora
Request -> Response raises HttpError
```

```khora
Request -> Response
  with { db: Db }
  raises DbError + HttpError
```

Clauses belong to the function arrow they follow.

## Handler expression

```khora
handler for Clock {
  now: fn _ => fixed_instant,
}
```

Fallible operation implementation:

```khora
handler for Store {
  load: fn id => load_from_disk(id)!,
  save: fn user => save_to_disk(user)!,
}
```

The implementation must satisfy the effect's operation types.

## Install handlers

One expression:

```khora
job() with {
  clock: fixed_clock,
}
```

Lexical block:

```khora
with {
  clock: fixed_clock,
  store: memory_store,
} {
  job()!
}
```

Named contexts and overrides are specified in [Capabilities](./capabilities/).

## Effect-polymorphic higher-order functions

```khora
fn map<A, B, 'e, 'r>(
  values: List<A>,
  f: A -> B with 'e raises 'r,
) -> List<B>
  with 'e
  raises 'r
{
  // ...
}
```

The higher-order function preserves the capabilities and failures of its callback rather than wrapping it in a separate effect value.

## Failure propagation and handling

```khora
operation()!
```

```khora
operation()! catch {
  Error::Case(value) => recover(value),
}
```

See [Failures](./failures/) for `raise`, `!`, `catch`, and `attempt` semantics.

## Capabilities and failures are independent

A capability operation may be infallible, and a pure function may still raise a typed domain failure. `with` records authority; `raises` records recoverable non-local control flow. Neither implies the other.