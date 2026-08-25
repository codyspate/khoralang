---
title: Concurrency
sidebar:
  order: 21
---

Khora concurrency is structured around fibers, nurseries, cancellation, and explicit sharing.

A fiber belongs to a structured owner. A nursery does not complete successfully while owned children are still running. Child failure and parent cancellation follow the nursery's propagation rules rather than creating detached work by default.

Fiber suspension is distinct from worker blocking. Waiting for nonblocking I/O, a timer, a join, or scheduler capacity suspends the fiber so the runtime can execute other runnable work.

Source functions are not colored `async`; suspension-capable operations remain ordinary direct-style calls.

Cancellation points are semantically distinct from scheduler safepoints used for fairness/preemption. Cancellation must make a suspended waiter runnable so structured finalizers can execute.

A fiber may resume on a different operating-system thread after suspension. Code crossing an FFI boundary must therefore not retain thread-local addresses, native-thread identity, or other thread-affine state across a Khora suspension unless the foreign contract explicitly supports that behavior.
