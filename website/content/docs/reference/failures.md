---
title: Failures
sidebar:
  order: 8
---

Recoverable failures are declared with `raises` and participate in the function type.

A call that may produce a declared failure can propagate it with postfix `!` when the caller's failure row permits that failure. Otherwise the current scope must handle or transform it.

Failure types should model conditions a caller can reasonably react to. Infrastructure-specific detail can be converted at an abstraction boundary rather than leaking every driver or protocol error through the entire application.

`raises` is not the mechanism for programming errors such as arithmetic overflow or bounds violations. Those are traps and have a separate runtime policy.

Generic code may preserve failure rows it does not itself interpret, allowing reusable abstractions to remain transparent about the caller's own failure set.
