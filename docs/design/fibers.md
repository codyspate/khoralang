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

### Which one 0.1.0 ships, and why it is threads

**Both are built. Threads are the default; the scheduler is
`KHORA_FIBERS=scheduler`.** A decision, not an unfinished migration -- and not
the one this section expected to be writing, because the staging above assumed
the coroutine would replace the thread as soon as it worked. It works.

#### What could be measured, and what could not

**The table that used to be here was retired on 2026-09-02**, along with every
other throughput figure this project had recorded. `bench/load.py` reported one
connection's rate multiplied by the number of connections -- it ran one process
per connection, each timing itself, and divided the total by the duration it
was *asked* for rather than the one it took. That is why neither implementation
appeared to reach a ceiling: a rig whose output is proportional to its own
worker count cannot flatten. It is also the 1.85x spread that was recorded here
as irreproducibility, which was process startup time varying. `docs/errata.md`
77 has the account.

Re-measured 2026-09-02 with `bench/loadgen.exe`, which is a few threads driving
many non-blocking connections and whose own rate stops changing when it is
given more of the machine. Sixteen-core Windows desktop, release build,
`bench/service`, five-second runs:

| | 32 connections | 128 connections | p99 at 32 |
| --- | --- | --- | --- |
| threads | 180,715 / 175,908 / 178,510 | 180,400 | 579us |
| scheduler | 145,095 / 143,916 / 144,135 | 156,084 | 831us |

Three sittings each, spread 1.03x and 1.01x. The rate is flat from 32
connections to 128 for both, which is what a saturated server looks like and
what the old rig could never show.

**Threads are about 23 per cent ahead at 32 connections and about 16 per cent
ahead at 128**, on the median request as well as on the tail. The one place the
scheduler is better is median latency at 128 connections, 524us against 668us,
which is the density argument showing up where `scheduler.md` says it should.

So the conclusion below survives the correction, but it was previously drawn
from a measurement of something else. What the old rig actually compared was
single-connection latency on a nearly idle server, because its workers barely
overlapped -- not throughput at the concurrency a service sees. The claim and
the evidence agree now; before, they only appeared to.

#### The decision

1. **Threads win at the concurrency a service sees**, by a measurement that
   reproduces to within three per cent and that flattens across the ladder.
2. **The scheduler's compensating benefit is unverified where it would be
   claimed.** It exists for fiber *density* -- `scheduler.md` says throughput
   was never the point -- and 100,000 suspended fibers at ~4.2 KB each was
   measured on Windows. Linux caps `vm.max_map_count` at 65530 and guard pages
   split mappings; `scheduler.md` records that as open.
3. **It is the less-exercised path**, and #108 is the argument: an intermittent
   failure sat in the runtime for months because one platform ran `cargo test`
   and the other `nextest`. A second execution model with fewer users is where
   the next one lives.

**Nothing is given up by waiting.** A program cannot tell which it has -- this
section's own premise -- so the default is not a compatibility commitment and
can move in a patch release once Linux is measured and the harness can measure
a ceiling.

#### Two lessons, both about numbers

**A measurement pasted into a comment is a measurement with no owner.**
`fiber.rs` carried `782,149 against about 429,000` for months. The scheduler
had got meaningfully faster in that time and the comment was the only place
either figure lived, so nothing was checked against anything. The numbers are
in this document now and the comment points here.

**And the harness was already suspected of being the limit.** That was written
in `bench/README.md` in a blockquote, above the figures it qualified, and
rediscovered from scratch during this decision by somebody who had not read it
-- which is the cost of a caveat living next to the numbers rather than in the
tool that produces them.

The caveat was also too kind. It said the figures measured the harness, which
was right, and left them in the table anyway. They were not merely
harness-limited: they were one connection's rate times a constant, so they
described the harness and nothing else. A caveat that lets a number stay is a
number. `bench/measure.py` now checks the conditions on every run and prints
what failed *instead of* a figure, which is the version of that lesson that
cannot be skipped by not reading a paragraph.

### What that costs while it is threads

Fibers are worth roughly what a thread is worth: use them for concurrency, not
as a data structure. A program that wants a hundred thousand of them wants
`KHORA_FIBERS=scheduler`, and should measure the density claim on the platform
it deploys to before relying on it.

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
pub effect Nursery {
  spawn: (() -> ()) -> Fiber,
}

pub fn nursery<A, 'ef, 'er>(body: () -> A with { 'ef | nursery: Nursery } raises 'er) -> A
  with 'ef
  raises 'er
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
pub fn nursery<A, 'ef, 'er>(body: ..) -> A with 'ef raises 'er {
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

   The spawned thunk is `() -> A raises 'er`. A thunk that can fail returns the
   tagged pair, so the runtime reads how the fiber ended — done, cancelled, or
   failed — and a cancellation stops *that fiber* rather than the program.

   Which makes the rule about cancellation points read the same from a fiber's
   side as from anywhere else: **a fiber with no error row has no channel to be
   interrupted on**, and runs to its end. That is not a limitation to explain
   away, it is the same sentence as "a cancellation point is a `!` in something
   that can raise".

   **The rule now has a second clause: a loop back-edge, in something that can
   raise.** Which functions have a cancellation point is unchanged -- the error
   row is still the channel -- but `loop { sleep; work }` is how every periodic
   job is written and it had no `!` in it, so a fiber shaped that way could not
   be stopped and a nursery that had to unwind past one waited for ever. The
   back-edge already emitted a safepoint; it emits the cancellation check too.

   A child's error nobody is waiting for is reported on stderr rather than
   dropped in silence, which is what a panicking thread does everywhere else.
   The error object is freed but not its fields, because the runtime cannot
   know a value's drop routine and the row said `'er` — a bounded leak on a path
   that is now only taken by a fiber nobody joined.

3. **The nursery** — *built*, and it is a value whose release stops what is
   still running. A fiber cannot outlive the block that spawned it, on every
   path out, and nobody writes the cancel.

4. **A fiber that answers** — *built*. `Fiber<A, 'er>`, `join(self) -> A
   raises 'er`, and the child's failure comes back out of the join with its
   type intact.

   **The row is on the handle, and that is the whole design.** An erased
   `Fiber<A>` was written first and it worked; it was also unpleasant in a way
   that only showed up when the first test was written against it. Every join
   needed a `!` and an enclosing `raises` — *including on a fiber that provably
   cannot fail* — and since the caller's row could not name the child's error
   type, `catch { _ => .. }` was the only arm that compiled. Carrying `'er`
   costs a second type parameter and buys back both.

   A nursery adopts `Fiber<(), 'er>`: **the answer is fixed and the row is
   not.** The answer is fixed because an operation cannot be generic in a type
   — a handler's fields are closures — and because a nursery has nothing to do
   with a result it cannot hand back.

   The row is a different matter, and getting there took two wrong answers.
   `Fiber<(), {}>` was the first, and it reads better than anything else here:
   an empty row says "settle your failure before you hand this over", which
   turns a line on stderr into a compile error. It is wrong. **A cancellation
   travels out on the same tagged return an error does** — §"What phase 5.3
   builds" (2) — so a fiber whose row is empty has no channel to be stopped on.
   Requiring an adopted child to have settled its failures makes every adopted
   child uncancellable, and a nursery that cannot cancel its children is not a
   nursery. Three tests said so within a minute of the row being enforced.

   The second wrong answer was a `Task`: the same runtime fiber under a handle
   with no parameters, keeping the row where cancellation reads it and dropping
   it from the type where the operation needs one shape. It worked, and it was
   a type whose entire reason for existing was a signature — every one of its
   seven uses in the tree was `adopt(Task::spawn(..))`, it was never bound to a
   name, and it could do strictly less than a `Fiber`.

   The right answer is that **an operation can be generic in a row**, which it
   turned out the checker was eight lines from allowing. A row costs nothing to
   quantify: a capability crosses as evidence and an error as a tag, so a
   handler's closure is the same code whatever the row is — while a type
   parameter decides a layout and has to be monomorphized. That asymmetry is
   the whole of it, and it is a real property of this design rather than an
   accident of the implementation.

   The quantifier is rank-1: the *call* instantiates, the handler stays rigid.
   So an operation is row-generic exactly when its handler does not look at the
   row — `adopt` waits for a child and never asks how it can fail — and a
   handler that tries to use `'er` is refused, which `rows.rs` pins.

   Three things share the waiting, and they are not the same wait:

   | | waits | takes the answer | on a cancelled child |
   | --- | --- | --- | --- |
   | letting the binding go | yes | no | nothing |
   | `Fiber::wait` | yes | no | nothing |
   | `Fiber::join` | yes | **yes** | **unwinds the joiner** |

   The last cell is the one rule this changed rather than added. A cancellation
   stops a fiber and not its parent — §"Two ways to end" and four tests pin
   that, and it still holds. But a *joiner* has asked for an answer that will
   never exist, and there is no `A` to invent, so the ask fails the way the
   child did. A parent that did not ask is untouched, which is what `wait` is
   for and why it exists as more than a synonym.

5. **`Fiber::detach`** — *built*, and it is the valve the design was missing.

   Every other way out of a handle waits. That is right, and it is also how a
   program hangs: one finalizer that never returns holds its nursery, which
   holds its parent, up to `main`. `docs/design/scheduler.md` promises both
   bounded cancellation latency and that a nursery exit leaves every child
   stopped or joined, and those two are in tension exactly here. `detach`
   cancels, lets go, and does not wait — the fiber's answer is dropped when it
   arrives and a failure afterwards is silent, because the program said it was
   no longer listening.

6. **Failure propagation out of a *nursery***: a child that raises makes the
   nursery raise. Still open, and (4) narrowed it rather than solving it. The
   typed multi-failure shape wants to live where the error type is a parameter
   — a `par_map` answering `List<Result<A, E>>` — because at the `Fibers` level
   there is nothing typed to put in a list: every child's error type differs
   and the handles are bare. `docs/design/effect-survey.md` §3.1.
7. **`khora test` across cores**, the exit criterion's second half.

**Phase 11 is designed in `docs/design/scheduler.md`**, which decides the parts
this note only gestured at — and adds one it did not have. Cancellation is
observed at `!`, so a function with no error row has no cancellation channel;
that is the right *language* rule and it means an infallible loop has no way to
be preempted. On threads the operating system solves that. On a scheduler it
does not, so the runtime needs a **safepoint** that is separate from a
cancellation point: it asks whether somebody else should run, it cannot fail,
and it unwinds nothing.

Work stealing, stack growth and the coroutine switch itself are **Phase 11**,
and are not on the path to phase 5's exit criterion. That entry in
`docs/roadmap.md` carries what this one only implies: what the thread
implementation costs while it lasts, and the reactor a scheduler needs
underneath it so that a blocking syscall does not park a worker.
