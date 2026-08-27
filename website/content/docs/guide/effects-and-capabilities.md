---
title: Effects and capabilities
sidebar:
  order: 7
---

Khora uses effects and capabilities to make external authority visible in function types without turning application code into explicit plumbing.

A capability requirement appears in a `with` row:

```khora
fn load_user(id: Id) -> User
  with { db: Db }
  raises DbError
```

Read `with { db: Db }` as: this function requires a database capability named `db`. The function cannot silently reach a global database connection that its type does not mention.

A handler supplies the capability for a scope. Code inside that scope can call operations provided by the capability directly; the handler decides what those operations mean in the current environment.

This gives Khora dependency injection without a runtime service locator and makes tests straightforward: provide a different handler with the same capability contract.

## Capabilities are authority

Treat a capability as permission to perform an effectful class of operations, not merely as a bag of helper functions. Database access, clocks, tracing, files, randomness, and external services are natural capability boundaries because they cross from pure computation into the outside world.

For example, randomness appears as an explicit requirement:

```khora
fn choose_bucket() -> Int
  with { random: Random }
{
  random.in_range(0, 10)
}
```

Tests can provide a deterministic `Random` handler without changing `choose_bucket`.

## Keep application APIs small

Do not expose every infrastructure capability at every layer. A high-level function should require the capabilities it genuinely uses. Narrow rows make authority reviewable and keep tests focused.

## Effects compose with typed failure

`with` answers what authority a computation needs. `raises` answers what recoverable failures it may produce. They are separate dimensions of the API and can be read independently:

```khora
fn determine_random() -> Bool
  with { random: Random }
  raises RandomFailure
```

The function needs access to randomness and may fail with `RandomFailure`; neither fact implies the other.

For the full failure flow—including explicit `raise`, propagation with `!`, pattern-based `catch`, translating one failure type into another, converting failures into API responses, and collecting failures with `attempt`—see [Typed failure with raises](./errors-and-raises.md).
