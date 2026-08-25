---
title: HTTP service
sidebar:
  order: 1
---

Khora's HTTP stack is layered so applications can use the reference router or build a different framework over the public codec and connection layers.

A production service should separate three limits:

1. how many client connections may remain open;
2. how many request handlers may actively consume application capacity;
3. how much concurrency downstream resources such as a database can sustain.

Cheap fibers do not remove the need for backpressure.

## Keep handlers explicit

Parse external input into domain types early. Return structured application failures to an HTTP boundary that decides the status code and response shape. Do not scatter transport-specific status decisions throughout domain code.

## Bound work

Use bounded concurrency around work that can exhaust a downstream dependency. When the server reaches sustainable capacity, prefer bounded queues and controlled rejection over accepting unlimited work and allowing latency and memory to grow without limit.

## Timeouts and cancellation

A disconnected or timed-out request should cancel work that exists only for that request. Cleanup must still run for resources owned by the request scope.

## Observability

Extract incoming trace context at the HTTP boundary, attach it to the request fiber, and create child spans around meaningful operations. Propagation should remain structural as fibers spawn or move between scheduler workers.

The reference applications in `examples/` demonstrate the current HTTP APIs while the generated standard-library reference is being built.
