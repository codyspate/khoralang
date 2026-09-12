---
title: Internals
sidebar:
  order: 0
---

How Khora works underneath: what a reference count costs, how a handler is
reached, what a fiber is made of, why a build is reproducible.

Nothing here is needed to write Khora. The [Language
Reference](/docs/reference/) and the [Cookbook](/docs/cookbook/)
describe the language as you use it, and mention the machinery only where it
changes what you would write. This section is for when that is not enough —
when you are debugging something strange, judging whether Khora suits a
problem, or simply want to know what the words mean.

## What is here

**[Memory](/docs/internals/memory/)** — reference counting without a
tracing collector, what the compiler removes, when storage is reused in place,
and the one case that leaks.

**[Effects and handlers](/docs/internals/effects/)** — what `with`
compiles to, why a capability costs a parameter rather than a lookup, and how a
failure leaves a function.

**[Fibers](/docs/internals/fibers/)** — what runs a fiber, what suspending
costs, and what a nursery guarantees.

**[The build](/docs/internals/build/)** — from source to a native
executable: what is cached, what makes a release build reproducible, and why a
linker is the one thing Khora cannot bring with it.

## What is not here

Decisions and their alternatives. This section says how Khora works, not why it
was built one way rather than another — that argument lives in the
repository's design notes, which are written for somebody changing the
compiler.

The distinction matters for reading: everything here describes the language as
it is now. If a page mentions something Khora does not do, it is a limit that
is still true, not a stage it passed through.
