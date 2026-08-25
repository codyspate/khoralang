---
title: Shared state
sidebar:
  order: 10
---

Khora is designed so most code can stay purely functional, but real services still need shared state: caches, counters, registries, in-memory test doubles, and coordination structures.

`Shared<A>` is the explicit boundary for values that may be shared safely across fibers.

Use shared state when multiple concurrent owners genuinely need to coordinate around one logical value. Do not reach for it merely to avoid passing ordinary immutable data through function arguments.

## Keep critical sections small

Updates to shared state should perform only the work needed to read or replace the shared value. Network calls, filesystem work, sleeps, and other suspension points should remain outside the shared-state update itself.

That keeps contention understandable and avoids coupling synchronization to external latency.

## Prefer domain operations

Wrap shared state behind functions that express the operation the program intends rather than exposing a mutable container everywhere. `increment_requests()` or `remember_session()` is easier to audit than arbitrary callers all performing unrelated updates against one global cell.

For persistence or cross-process coordination, use the database or another external capability instead of treating process-local sharing as durable state.
