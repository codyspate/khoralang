---
title: Bounded concurrency
sidebar:
  order: 3
---

Structured concurrency tells you who owns concurrent work. Bounded concurrency tells you how much work is allowed to compete for a constrained resource at once.

Use a bounded nursery or another explicit limit when fan-out can exceed the capacity of a database, remote service, filesystem, or memory budget.

## Choose the limit from the constrained resource

A server may be able to hold tens of thousands of mostly-idle connections while only allowing hundreds of active database operations. Those are different limits and should not be collapsed into one global fiber count.

## Prefer admission control to collapse

Under overload, healthy behavior is:

- completed throughput reaches a sustainable plateau;
- runnable queues remain bounded;
- memory remains bounded;
- latency degrades predictably;
- excess work waits only within explicit limits or is rejected;
- the service recovers quickly when offered load falls.

If increasing offered traffic causes unbounded queues or memory growth, the system has hidden its overload policy instead of solving it.
