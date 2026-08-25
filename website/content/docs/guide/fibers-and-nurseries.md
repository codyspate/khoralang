---
title: Fibers and nurseries
sidebar:
  order: 9
---

Khora uses structured concurrency. Concurrent work belongs to a nursery rather than becoming detached background work with an unrelated lifetime.

A nursery owns the fibers started inside it. The scope does not finish successfully while owned work is still running, and failure/cancellation propagates through that ownership relationship.

## Fiber lifetimes are structured

The useful mental model is not "a cheaper thread." It is "a concurrent child whose lifetime is part of this scope."

That means code can answer:

- who owns this work?
- who waits for it?
- what happens if one child fails?
- what happens when the parent is cancelled?
- which cleanup runs before the scope exits?

without relying on process-global task registries.

## Blocking and suspension

A fiber may suspend when waiting for I/O, a timer, another fiber, or scheduler capacity. Suspending a fiber must not block the scheduler worker that happened to be running it.

Application code remains direct style: ordinary calls rather than a second `async`/`await` version of every API.

## Bound external concurrency

Cheap fibers are not permission to create unlimited pressure on databases, remote APIs, or memory. Use bounded nurseries or resource-specific limits around externally constrained work.

Structured concurrency gives you ownership; backpressure gives you survival under load. Production code usually needs both.
