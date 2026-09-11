---
title: Effects and handlers
sidebar:
  order: 2
---

A Khora signature says what a function needs and how it can fail:

```khora
pub fn analyze(id: String) -> Report with { ledger: Ledger } raises DbError
```

Both rows are checked at compile time, and both compile to something ordinary.
There is no effect interpreter, no handler stack to search, and no unwinder.

## A capability is an argument

A capability row is static: the compiler knows at every call site exactly which
handlers a function needs, because the signature says so and the checker has
verified it.

So the handler is not looked up. It is passed in. The function above compiles
to one taking an extra argument — the `Ledger` handler, which is a record of
closures and therefore an ordinary heap value. Each call site supplies it from
its own capability parameter, or from the enclosing `with` block.

Performing an operation is then a field read and a call through a function
value. That is the same cost as calling any closure.

```khora
with { ledger: Ledger::real() } {
  analyze(id)          // `ledger` is handed to `analyze` as an argument
}
```

**Nothing is searched at run time.** There is no stack of installed handlers,
no walking of frames to find the innermost one, and no cost that grows with
nesting depth.

Row polymorphism (`'ef`) specialises the same way generics do. Khora
monomorphises the whole program, so a row variable is concrete at every call
site reachable from `main` for the same reason a type variable is.

## A failure is a tagged return

A function whose `raises` row is non-empty returns two words: a tag and a
payload. The tag is `0` when the call returned normally, and otherwise
identifies the *type* of the error that was raised.

At each `!` the compiler emits a branch. On the error path it releases what
that frame owns and returns the tag upward, which is the same machinery an
early `return` already uses.

This has consequences worth knowing:

**There is no unwinder.** No DWARF tables, no landing pads, no personality
routine, no `longjmp`. A raise is a return with a tag, and each frame it passes
through runs the releases it was going to run anyway. That is what keeps
foreign frames out of the story — see [FFI](/docs/next/reference/ffi/).

**`!` is where the branch is.** The mark is usually explained as readability,
and it is: a reader taught by `?` and `try` expects a mark where control can
leave. It is also exactly the point where the compiler emits the test. The
notation and the machine agree.

**The tag names the error's type, not just "something failed".** `catch`
handles part of a row, so a function raising `DbError + ModelError` whose
caller catches only `ModelError` has to know at run time which arrived. A
variant index cannot answer that — `DbError::Timeout` and
`ModelError::RateLimited` are both variant zero of their own types — so the tag
is a program-wide id for the type, assigned once by the whole-program compiler.
An error carries the same tag through every frame it crosses, whatever the rows
in between look like.

The payload is one word because every Khora value is word-sized. Two registers
in total, which is what a bare success/failure bit would have cost anyway.

## Handlers do not suspend

A handler runs, returns, and control continues. It cannot capture the rest of
the computation and resume it later, which is what a first-class continuation
would allow.

That is deliberate, and it is why handlers need no stack machinery at all.
Stopping and continuing a computation is what a *fiber* does, and fibers are a
separate mechanism — see [Fibers](/docs/next/internals/fibers/). Effect
(TypeScript) draws the same line between dependency injection and the runtime
that suspends it.

What you give up is the generator-shaped uses of algebraic effects: an operation
that yields several times, or a handler that restarts a computation. What you
get is that performing an operation costs a call rather than a stack switch,
and that a handler can be passed to foreign code without the foreign frames
becoming part of anybody's control flow.

## `with` and `raises` are one mechanism

Both are rows. Both are settled at compile time. Both are checked at every call
and reported at the call rather than at the definition.

They compile to different things because they *do* different things: a
capability is called and returns, so it is an argument; a failure leaves and
does not, so it is a tag on the return. Keeping them separate in the syntax is
what makes that difference legible.
