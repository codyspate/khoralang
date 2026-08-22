# How a fiber suspends

Phase 5.3. This decides the one genuinely new subsystem in the phase, and it is
written before any of it is built because the wrong answer here is expensive to
reverse.

## The question, and what already constrains it

A fiber is a computation that can stop and continue later. Khora has no
machinery for that: `docs/design/effect-runtime.md` §3 deliberately declined to
build continuations, and §1 observed that the decided syntax cannot even name
one. So a fiber has to be a real stack, and the question is who owns it.

Five things already settled bear on the answer.

- **No garbage collector, and therefore no stack maps** (non-negotiable 5).
  Whatever a fiber is, nothing walks its stack looking for pointers.
- **Direct style, no colouring.** The argument the whole language rests on is
  that async, failure and dependency injection belong in a *row* on the
  signature rather than in a wrapper type that propagates through every
  caller.
- **Fibers across cores** (A5), which is also phase 5's exit criterion.
- **Cancellation is already built** (§6): a flag, checked at `!`, turning into
  a tagged return that unwinds by the path errors already take.
- **Reference counts are atomic** (D10), so a value crossing to another fiber
  is safe today.

## The three candidates

### Threads

A fiber is an operating-system thread. Suspension is blocking. Direct style
needs no help at all, parallelism is real and immediate, and the cancellation
flag becomes per-thread.

The cost is per-fiber weight: a stack reservation and a kernel object each,
and tens of microseconds to create one. Thousands of fibers is comfortable;
hundreds of thousands is not.

### Stackful coroutines

A fiber is a small stack of its own, switched in user space and multiplexed
onto a pool of worker threads. This is what Go does, and it is the experience
Khora wants: a few kilobytes per fiber, a switch in tens of nanoseconds,
direct style, no colouring.

The cost is that it is a real project. Context switching is per-target
assembly. Stacks that start small and grow need guard pages or segmentation.
Multiplexing across cores needs a scheduler, and a good one needs work
stealing. None of it is research, and all of it is work.

### A state-machine transform

A fiber is a compiler-generated state machine, driven by a poll loop. This is
Rust's `async`, and it is the cheapest at runtime.

It is also the one candidate that argues against the language. "Can this
suspend" is a property that propagates through call graphs — through function
values, through generics, through trait methods — and the compiler has to
track it. Rust surfaces that as `async fn` and `Future`, and it is the single
most complained-about thing about Rust's concurrency.

Khora could in principle put it in a row instead, and rows do compose in a way
`Future` does not. But it would still change the calling convention of every
function that can suspend, require a closure for every continuation, and force
every owned value live across a suspension point into the state machine — which
is a second, parallel ownership story next to Perceus's. That is a large amount
of machinery bought to avoid allocating stacks.

## Decided

> **A fiber is a stackful coroutine multiplexed onto worker threads. The first
> implementation makes each one an operating-system thread.**

Two statements, and the second is not a hedge — it is the same argument that
decided D10. The interface a program sees is `spawn`, `join`, `cancel` and a
nursery that owns them; a program written against it is correct under either
implementation, and which one is running is not observable except in how many
fibers are affordable. Swapping threads for coroutines is a change inside
`khora-rt`, exactly as making refcounts atomic was.

The state-machine transform is rejected outright rather than deferred. It is
the one option that would have to be designed into the compiler now, and it
buys speed at the cost of the property the language is *for*.

### What that costs while it is threads

Fibers are worth roughly what a thread is worth: use them for concurrency, not
as a data structure. A program that wants a hundred thousand of them is a
program to write after the scheduler lands, and the roadmap should not pretend
otherwise.

Nothing about the interface encourages the wrong thing, which is what makes the
staging safe. There is no `spawn` per element of a list in any example, and the
nursery shape below makes the number of live fibers something you can see.

## Structured, because a nursery is a region

The mechanism is the smaller half. What makes concurrency *structured* is that
a fiber cannot outlive the block that started it, and Khora already has the
thing that enforces that: a region (§10) whose finalizers run on every path out,
including a raise passing through.

So a nursery is a region, and the fibers spawned into it are its finalizers'
business:

```
export effect Nursery {
  spawn: (() -> ()) -> Fiber,
}

export fn nursery<A, 'e, 'r>(body: () -> A with { 'e | nursery: Nursery } raises 'r) -> A
  with 'e
  raises 'r
```

`nursery` opens a region, installs a handler whose `spawn` registers each fiber
with it, and every path out of the block — the end of it, an early `return`, a
raise, a cancellation — runs the finalizers that wait for the children. There
is nothing new to enforce, and no way to write a fiber that escapes: the
`Nursery` capability is only in scope inside the block, and the block cannot
end while a child is still running.

This is the second time regions have paid for themselves, and it is worth
noticing that neither use needed the other to be designed for it.

### Two ways to end, without asking which happened

Trio and its descendants distinguish two ways a nursery can end. On the normal
path it *waits* for its children; on an error it *cancels* them and then waits,
because the answers they were computing are no longer wanted.

A release cannot tell which happened — it is handed a pointer and nothing else
— and the obvious fix is to hand it the reason as well. That turned out not to
be needed. **The normal path waits explicitly, before the release**, so by the
time the release runs there is only one case left:

```
export fn nursery<A, 'e, 'r>(body: ..) -> A with 'e raises 'r {
  let crew = Fibers::open();
  let value = with { nursery: .. } { body()! };
  Fibers::wait(crew);      // only reached when `body` finished
  value
}
```

`Fibers::wait` empties the nursery, so releasing it afterwards finds nothing to
stop. Every other way out of the block skips that line, and the release cancels
and then waits. One value means both, and nothing has to be told which happened
— the control flow already said it by arriving or not.

This is worth noticing beyond the nursery: **a cleanup that differs between the
normal and the abnormal path can often be written as an abnormal-path cleanup
plus a normal-path statement that defuses it.** Handing the reason down is a
generalisation nobody has needed yet.

## Per-fiber, not per-process

Cancellation is a process-wide flag today, which was the right thing to build
before there was anything to be per-fiber *of*. With fibers it becomes one flag
per fiber, and three things follow:

- `khora_cancelled` reads the running fiber's flag. Generated code already goes
  through that call and never touches the flag directly, so this is a change
  inside the runtime.
- Cancelling a nursery cancels its children, transitively. That is the whole
  point of the tree.
- A cancellation that reaches a fiber's root stops *that fiber*, not the
  process. The frame with no error channel described in §6 stops being the
  end of the world: a fiber root can carry a cancellation, which is what
  `khora_cancel_stop` is standing in for until one exists.

## What phase 5.3 builds, in order

1. **A fiber and its handle** — *built*. `spawn` on a thread, `join`, `cancel`,
   and the per-fiber cancellation flag in place of the global one.

   The structured half came free, and it is worth saying how: **a fiber
   handle's release joins it.** So a fiber cannot outlive the binding that
   holds it, on every path out including a raise, because that is what
   releasing a binding already does. Put a handle in a region and the region
   waits; put it in a block and the block waits. Nobody has to write `join`,
   and there is no way to write a fiber that escapes.

2. **A fiber root that absorbs a cancellation** — *built*.

   The spawned thunk is `() -> () raises 'e`. A thunk that can fail returns the
   tagged pair, so the runtime reads how the fiber ended — done, cancelled, or
   failed — and a cancellation stops *that fiber* rather than the program.

   Which makes the rule about cancellation points read the same from a fiber's
   side as from anywhere else: **a fiber with no error row has no channel to be
   interrupted on**, and runs to its end. That is not a limitation to explain
   away, it is the same sentence as "a cancellation point is a `!` in something
   that can raise".

   A child's error nobody is waiting for is reported on stderr rather than
   dropped in silence, which is what a panicking thread does everywhere else.
   The error object is freed but not its fields, because the runtime cannot
   know a value's drop routine and the row said `'e` — a bounded leak on a path
   that goes away with (3), where the error reaches a parent who knows exactly
   what it is.

3. **The nursery** — *built*, and it is a value whose release stops what is
   still running. A fiber cannot outlive the block that spawned it, on every
   path out, and nobody writes the cancel.

4. **Failure propagation**: a child that raises makes the nursery raise. The
   tag already carries it and there is now a parent to give it to; what is
   missing is somewhere on the parent to put it, which is a mutable cell and
   therefore D11.
5. **`khora test` across cores**, the exit criterion's second half.

Work stealing, stack growth and the coroutine switch itself are phase 9 or
later, and are not on the path to the exit criterion.
