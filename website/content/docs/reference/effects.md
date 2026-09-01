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

### An operation may be generic in a row, but not in a type

An operation's `raises` row may mention a row variable the operation itself binds. It is quantified per call, not per handler, so one handler serves callers whose failures are unrelated:

```khora
pub effect Nursery {
  adopt: (Fiber<(), 'er>) -> (),
}
```

An operation cannot introduce a *type* parameter the same way. The asymmetry follows from representation. A capability crosses as evidence and an error as a tag, so the handler's closure is the same machine code for every row — nothing in it depends on which failures the caller can raise. A type parameter decides a layout and must be monomorphized, and a handler's fields are closures, which have nowhere to put a per-layout instantiation.

An effect may still take type parameters of its own on the `effect` declaration, as the general form above shows. Those are fixed when the handler is written; a row variable on an operation is not.

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
fn use_clock<A, 'ef>(body: () -> A with { clock: Clock | 'ef }) -> A
  with { clock: Clock | 'ef }
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
raises 'er
```

## A row variable on an ordinary function

`raises 'er` is usually seen on a higher-order signature, where the row belongs to a closure the caller supplies. It belongs on an ordinary function too, and there it means something worth having a name for:

```khora
fn nap<'er>(ms: Int) -> () with { clock: Clock } raises 'er {
  clock.sleep(ms)!
}
```

**A helper with no failure of its own that is still a cancellation point.** The `!` inside is what makes it one; the `'er` is what lets it sit inside a caller that raises anything at all without widening that caller's row. Written `raises Never` it would be a helper nobody could call from a fallible function without a discharge; written with a concrete error it would invent a failure it does not have.

Any helper that waits — on a clock, a channel, a lock — wants this signature.

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
  now: fn () => fixed_instant,
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
fn map<A, B, 'ef, 'er>(
  values: List<A>,
  f: A -> B with 'ef raises 'er,
) -> List<B>
  with 'ef
  raises 'er
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