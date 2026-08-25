---
title: Cancellation-safe resources
sidebar:
  order: 5
---

Cancellation is not an exceptional afterthought in a concurrent service. A fiber may be cancelled while it is waiting for I/O, a timer, a child, scheduler capacity, or a downstream dependency.

Resource-owning code should therefore register cleanup with the region that owns the resource as soon as acquisition succeeds.

## The rule

If a scope can exit by success, typed failure, or cancellation, all three paths must leave the resource in a valid state.

Examples include:

- close sockets and files;
- roll back open database transactions;
- return pooled connections only after cleanup;
- release permits/semaphores;
- remove temporary files where the API promises that behavior;
- finish or abandon tracing spans consistently.

## Do not rely on the next line running

Code shaped conceptually as "acquire; do work; close" is only safe if `close` is structurally guaranteed. A cancellation point inside "do work" can prevent ordinary sequential cleanup from executing.

Prefer region/defer or scoped helper APIs that make cleanup part of the lifetime contract.

## Test it

For every production resource abstraction, include a test that cancels the owning fiber while it is suspended inside the resource scope and verifies cleanup still happens before the scope is considered finished.
