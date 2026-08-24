# The scheduler

What Phase 11 builds, and the decisions that have to be made before any of it
is written. `docs/design/fibers.md` decided *what a fiber is*; this decides how
one stops being an operating-system thread.

## What success is

Not "fibers are not threads any more". That is the mechanism, and a mechanism
can be delivered while the thing it was for stays out of reach.

> **A hundred thousand mostly-waiting Khora fibers are routine on an ordinary
> developer machine, with no change to synchronous Khora source, and with
> structured cancellation and finalization behaving exactly as they do today.**

Broken into things that can be measured, and failed:

| | target |
| --- | --- |
| 100k sleeping fibers | works, repeatedly |
| 100k fibers waiting on timers | works, repeatedly |
| 100k fibers waiting on sockets | works, repeatedly |
| committed bytes per idle fiber | measured, and published |
| worker threads | about the available parallelism |
| idle fiber CPU | indistinguishable from zero |
| creating a fiber | no kernel thread created |
| a worker blocked on socket I/O | never |
| cancellation | bounded latency, at cancellation points |
| nursery exit | every child stopped or joined |
| mass cancellation | no leak, no deadlock |
| one fiber in a tight loop | cannot hold a worker indefinitely |
| thread-per-fiber | gone from the normal path |

**No requests-per-second target.** `bench/service` already does 538k req/s on
threads, so throughput is not what is in question and a number here would only
invite tuning the benchmark. What is in question is how many fibers can be
*waiting* at once, which is a memory property, not a speed one.

## Five things that must be true

1. A Khora fiber stops being an operating-system thread. M:N is the outcome,
   not an implementation detail of it.
2. Blocking-looking Khora I/O suspends the *fiber*, never the worker.
3. **Scheduling fairness and cancellation are separate.** Infallible work may
   yield without becoming cancellable.
4. Structured concurrency survives unchanged: cancellation, then finalization,
   then join; a child cannot escape its nursery.
5. Scale is demonstrated by memory and waiting-fiber measurements, not by spawn
   throughput.

---

## 1. Yield points are not cancellation points

This is the largest addition to what `fibers.md` decided, and the one most
likely to be got wrong by treating the two as the same thing.

Today a cancellation is observed at `!` in something that can raise, and a
function with no error row has no channel to be interrupted on. That is a good
language rule and it stays. But it means this fiber has no cancellation points
at all:

```khora
fn crunch() -> () {
  while true { calculate() }
}
```

On a thread that is the operating system's problem. On M:N it is ours: that
fiber owns a worker until the process ends.

So the runtime needs two ideas that today are one:

- a **cancellation point** asks *should this fiber stop?* — it exists only
  where a failure can propagate, and it unwinds through ordinary Khora cleanup;
- a **safepoint** asks *should somebody else run?* — it exists everywhere, it
  cannot fail, and it does nothing but switch stacks and come back.

A safepoint is far cheaper than a cancellation point precisely because nothing
can unwind through it. No error row, no finalizers, no tag.

**Where safepoints go.** A loop back-edge is the one that is required: it is
the only place a Khora program can spin without doing anything else. Beyond
that, `khora_alloc`'s slow path is nearly free to instrument and covers most
real loops, because Perceus-managed code allocates constantly. Function entry
is tempting and probably unnecessary — measure before adding it.

The check itself is a load of a per-worker flag and a rarely-taken branch. The
flag is set by a timer or by another worker wanting to preempt.

**This needs the compiler**, which is why it is decided here and not
discovered in 11C. A back-edge safepoint is emitted by code generation, and
nothing in `khora-codegen-llvm` emits anything of the kind today.

---

## 2. The reactor, and where this disagrees with the obvious design

A scheduler whose fibers make blocking syscalls parks a worker per blocked
fiber and has bought nothing. So the reactor is not a later optimization; it is
part of the exit criterion.

The path a read takes:

```
Khora:      let request = connection.read()!

runtime:    try the syscall
              ├─ it worked         → return the bytes
              └─ it would block    → register, suspend the fiber, run another
                                     ... the operating system says ready ...
                                   → make the fiber runnable, resume, retry
```

The property worth defending aggressively is the first line. No `async`, no
`await`, no `Future`, no coloured functions. `std::net::socket` keeps its
blocking shape and every existing Khora program benefits without being touched.

**One interface, three backends — but not the interface you would expect.**
The obvious shape is a readiness API:

```
register(handle, interest, fiber) / modify / unregister / poll(deadline) / wake
```

That is epoll's and kqueue's model, and it is the wrong neutral abstraction,
because **IOCP is completion-based**. Forcing IOCP to pretend it reports
readiness is what `mio` does, and it costs an internal buffer, an extra copy,
and a permanent seam of awkwardness on the platform this compiler is developed
on.

The reverse emulation is strictly easier. So the scheduler-facing interface is
**operation-oriented**:

```
submit(operation, fiber) -> suspends until the operation has completed
```

- **Windows / IOCP** — native. Submit the overlapped operation, the completion
  packet wakes the fiber.
- **Linux / epoll**, **macOS / kqueue** — try the syscall; on `EWOULDBLOCK`
  register interest, suspend, and retry on readiness. The retry is inside the
  backend and the scheduler never learns it happened.

The scheduler sees the same semantics everywhere. The backends do not have to
share mechanics, and pretending they do is how the awkwardness gets in.

**Not io_uring**, not yet. epoll is a simpler baseline that proves the
architecture, and io_uring is a Linux optimization to reach for after there is
something to profile.

---

## 3. Stacks, and a tension worth naming

Two requirements pull against each other, and the resolution is not obvious.

- Stacks should **grow**, or deep recursion becomes a language limitation
  nobody can see coming.
- The scheduler must stay **ignorant of Khora's object graph** — no stack maps,
  no knowing which slots hold references. That separation is one of the best
  properties of the current architecture and Perceus is what makes it possible.

They conflict. Growing a contiguous stack by *copying* it — Go's approach —
requires identifying and rewriting every pointer into the stack, which is
exactly the knowledge the scheduler is supposed not to have.

Growing by **guard page** avoids that: reserve a large range, commit a page at
a time, let a fault extend it. Nothing moves, so nothing needs rewriting. This
is the usual recommendation and it has a problem at the scale being targeted.

**`vm.max_map_count`.** A guard page has different permissions from the stack
below it, so committing one splits the mapping. Linux counts mappings per
process against `vm.max_map_count`, which defaults to **65530**. A hundred
thousand fibers, each with a stack and a guard page, is two hundred thousand
mappings, and the process fails to allocate long before it runs out of memory.
Raising a sysctl is not "routine on an ordinary developer machine".

**The recommendation**, to be confirmed by measurement in 11A:

> One large reservation, carved into fixed-size slots, with a stack-limit check
> in the function prologue instead of a guard page.

- One mapping, so the mapping count is a constant rather than a multiple of the
  fiber count.
- Anonymous memory is committed lazily by the operating system when touched, so
  an idle fiber costs the pages it has actually used and not its slot size.
- Overflow becomes a **clean runtime error naming the fiber**, rather than a
  silent write into the neighbouring stack. That is strictly better than a
  guard page's segfault.
- No stack maps, because nothing ever moves.

The cost is a compare-and-branch in every prologue, and a fixed ceiling per
fiber. Both are acceptable; neither is free. The prologue check is another
compiler change, and it is the same mechanism as the safepoint above, which is
an argument for designing them together.

Windows reserves and commits differently and charges commit against the page
file, so the slot strategy needs checking there specifically before it is
locked in.

---

## 4. The fiber's state machine, and one invariant

Deliberately small:

```
NEW → RUNNABLE → RUNNING ─┬→ RUNNABLE        (yielded at a safepoint)
                          ├→ WAITING         (I/O, timer, join)
                          ├→ CANCEL_PENDING
                          └→ COMPLETE

WAITING ─┬→ RUNNABLE
         └→ CANCEL_PENDING
```

The hard part is not the states. It is the moment between deciding to wait and
being visibly waiting:

```
fiber decides to sleep
                       ↘
                         the socket becomes ready
```

If the wake lands in that gap it can be lost, and the fiber sleeps forever on
an event that already happened. This is the classic lost-wakeup and it is worth
stating as a rule the implementation must be able to point at:

> **If a wake races with a suspension, the fiber either stays running or
> becomes runnable. It may never end up in `WAITING` having consumed a
> wakeup.**

Every suspend/wake pair in the scheduler is checked against that sentence.

---

## 5. Cancelling something that is asleep

Today cancellation is observed by running code. That is sufficient when every
fiber is a thread the operating system will schedule regardless.

It stops being sufficient the moment a fiber can be `WAITING` on a socket that
will never become ready. Setting a flag it will never run to read is not
cancellation; it is a leak with good intentions.

So cancelling a fiber must:

1. record the cancellation;
2. if it is suspended, **remove or neutralize its wait registration** and make
   it runnable;
3. let it resume through the ordinary cancellation path.

The last step is what keeps the language semantics intact: the fiber wakes,
observes the cancellation at its next cancellation point, and unwinds through
ordinary Khora finalizers. The scheduler's only job is to guarantee it gets
another chance to look.

---

## 6. Timers are scheduler waits

`sleep` must not block a worker. A timer is the same shape as I/O: suspend the
fiber, record a deadline, make it runnable when the deadline passes.

A timer heap is enough to establish the semantics. A hierarchical wheel is the
thing to reach for if measurement says the heap is the problem, and not before.

**There is an existing interaction to migrate.** `std::net::http` sets a read
deadline with `SO_RCVTIMEO` before reading, and relies on the socket timing out
to stop a slow client parking a fiber. Under the reactor that becomes a
scheduler timer racing a readiness event, which is a different mechanism with
the same meaning — and `http.kh`'s comments about deadlines will need to be
true of the new one.

---

## 7. Fiber identity is not thread identity — and this is already a bug

The runtime keeps per-fiber state in thread-locals today, which is correct
exactly as long as a fiber *is* a thread:

- `cancel.rs` — `CANCELLED`, the cancellation flag, and `ON_FIBER`;
- `shared.rs` — `FIBER`, a per-fiber id.

After M:N, fiber 42 may start on worker 3 and resume on worker 7. Anything in
native thread-local storage because it was "per-fiber" is then wrong.

**`shared.rs`'s `FIBER` is the sharp case, and it fails in both directions.**
It exists so that `Shared::update` can refuse re-entry — a change function runs
under the cell's lock, so touching the same cell inside one would wait for
itself, and the runtime reports that instead of hanging. The holder is recorded
as this thread-local id.

- **False positive.** Fiber A takes the lock and suspends. Fiber B is scheduled
  onto A's old worker, reaches the same cell, reads the same thread-local id,
  matches the recorded holder — and is killed with `fatal()` for a re-entry it
  did not perform. A correct program aborts, dependent on timing.
- **False negative.** A fiber that migrates while holding the lock no longer
  matches its own recorded holder, so the guard misses the real deadlock it
  exists to report, and the program hangs instead.

The false positive is the worse of the two: it turns a working program into an
intermittent abort.

**So the runtime needs an explicit current-fiber pointer**, maintained across
every context switch, and every "which fiber am I?" question resolves through
it. Native thread-local storage may still hold `current_worker` and
`current_fiber`, because those are updated at each switch by definition.
Persistent fiber state may not live there.

This is a change to code that exists and passes its tests today, which is why
it belongs in 11A rather than being found in 11F.

---

## 8. Foreign code

A Khora `extern fn` may reach a C library that cares which thread it is on:
thread-local state, COM initialization, an OpenSSL provider, a graphics
context. If a fiber enters foreign code on one worker and resumes on another,
that can matter.

The policy, which is cheap and can be relaxed later:

> **A fiber may not suspend inside an `extern` call.** Foreign calls run
> synchronously on the current worker unless they are explicitly classified as
> blocking, in which case they go to the blocking pool.

The runtime must not accidentally promise foreign code a stable operating-system
thread identity. If some API eventually needs one, a pinned section is the
answer — and it should not appear in the language until something concrete
demands it.

---

## 9. A bounded pool for things that genuinely block

Not everything fits a reactor: some filesystem operations, DNS, subprocess
waits, arbitrary foreign code. These must not block scheduler workers.

```
                 ┌─ CPU workers          (about one per core)
Khora fibers ────┤
                 └─ blocking workers     (bounded, with backpressure)
```

**Bounded matters.** An unbounded blocking pool quietly recreates
thread-per-fiber under a different name, which is the thing this whole phase
exists to remove.

---

## 10. The scheduler itself

Conventional, and deliberately so:

```
worker 0..N   local deque
              + global injection queue      (external wakes)
              + reactor wake queue
```

- spawning from a running fiber pushes to the local deque, for locality;
- the reactor and the blocking pool inject globally;
- an idle worker checks local, then global, then steals, then parks.

**Not NUMA-aware placement, not priorities, not scheduler classes.** Get
something measurable first.

---

## 11. What is already done that a reader might assume is not

- **Bounded concurrency exists.** `std::core::bounded_nursery(limit, body)`
  holds at most `limit` children at once. Cheap fibers make unbounded spawning
  possible, and the primitive that answers it is already written and used.
- **The mutable cell exists.** `Shared<A>` is in `std::core`, which unblocks
  the one item `fibers.md` left open: a child's failure propagating into its
  nursery rather than being reported on stderr. That is worth finishing
  alongside the scheduler, because structured concurrency is not complete
  until a failing child cancels its siblings and hands a typed error to its
  parent — and Khora's typed rows make that unusually clean. The scheduler
  transports the completion; it never interprets the error.
- **Counters exist.** `khora-rt`'s `counters` module is where the scheduler's
  instrumentation goes.

## 12. What stays out of scope

Unless a measurement forces it: io_uring, NUMA awareness, priorities, real-time
guarantees, lock-free everything, a fiber-specific allocator, user-visible
`yield`, user-visible worker pinning, `async`/`await` in any form, and a
fiber-local storage API.

Each is somewhere a runtime can spend a year for very little.

**And specifically: reference counting stays atomic.** M:N changes where values
run, which is enough risk for one phase. The compiler's existing single-threaded
analysis already removes the atomics it can prove unnecessary; cross-fiber
refinement is later work.

---

## 13. Order

Staged so that a failure is attributable to the thing that just changed.

| | what it proves |
| --- | --- |
| **11A** context switch, one worker, many fibers, explicit yield, join | stack switching, and the current-fiber pointer replacing thread-locals |
| **11B** N workers, local and global queues, migration, safepoints | CPU parallelism, and fairness |
| **11C** reactor and timers: suspend, wake, cancel-while-waiting | a hundred thousand waiting fibers |
| **11D** work stealing | locality, once the simple scheduler's behaviour is understood |
| **11E** bounded blocking pool | that unavoidable blocking cannot stall a worker |
| **11F** scale and soak | the adversarial tests below |

Khora stays buildable throughout. Threads remain the implementation on any
platform whose backend has not landed.

## 14. Instruments, from the beginning

Cheap counters, sampled or compiled out, not added after something is slow:

workers; runnable, running and waiting fibers; creations and completions;
steals attempted and succeeded; reactor wakeups; parks and unparks; timers
outstanding; blocking-pool queued and active; cancellations requested and
observed.

Without them a bad result says *a hundred thousand connections is slow*. With
them it says *97k waiting, 2.8k runnable, queues imbalanced, steal success 2%,
blocking pool saturated* — which names the thing to fix.

Tracing comes later and must never become program-observable.
`docs/design/compatibility.md` already says timing is not part of the promise,
and scheduler identifiers must not creep into anything a program can read.

## 15. The tests that matter are not benchmarks

The worst scheduler bugs do not appear under load; they appear under
*interleaving*. Randomized stress tests, run thousands of times, for:

wake against suspend; cancel against wake; cancel against completion; join
against completion; a nursery cancelled while children are being spawned;
worker shutdown during a wake; a timer firing during cancellation; socket
readiness arriving after deregistration; a fiber dropped while waiting; steal
contention; a child failing as a sibling completes.

A **deterministic test scheduler**, where queue decisions are controlled rather
than raced, is worth building for this. It turns "it failed once in ten
thousand runs" into a case that can be replayed.

---

## What this document does not decide

- Whether the prologue check is one instruction sequence serving both the stack
  limit and the safepoint, or two. It should be measured in 11A.
- The slot size for a stack, which is a measurement: committed bytes per idle
  fiber at one thousand, ten thousand and a hundred thousand.
- Whether Windows' commit accounting makes the single-reservation strategy
  workable there, or whether it needs its own.
