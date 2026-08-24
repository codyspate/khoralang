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

**Built in 11B, and it cost less than predicted.** The check is a call to
`khora_safepoint` at every back-edge, not the inlined flag load described
above, and what decides when to yield is a budget of safepoints granted at each
resume rather than a timer. Measured on `bench/service`:

| | req/s |
| --- | --- |
| without safepoints | 800,730 |
| with safepoints | 796,116 / 781,456 / 784,215 |

The spread across the three "with" runs is wider than the gap to the "without"
run, so the call is under this benchmark's noise floor. A tight loop over a
large byte buffer would show more, and the inlined flag check is what to reach
for then.

**A program that cannot spawn emits no safepoints at all.** The compiler
already proves that to decide whether reference counting is atomic, and the
same proof says there is nobody to be fair to.

The budget is fairness measured in safepoints rather than in time, which is
worth being honest about: a fiber doing something expensive between two
safepoints still holds its worker for exactly that long. A timer setting a flag
is the refinement, and it now has something to measure against.

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

### What 11C.2 actually built, and a course correction

**`poll`, on all three platforms** — `WSAPoll` on Windows and `poll` by the
same name on Linux and macOS, with the same struct in a different width. Not
IOCP, not epoll, not kqueue.

The interface above it is unchanged and is still the operation-oriented one
this section argues for: `wait_until_ready` is called by an operation that has
already tried and would block, and returns when it is worth trying again.
Nothing above the reactor learns which mechanism answered, which is the whole
property that makes swapping the mechanism a local change.

So this is a course correction about the *backend*, not the interface, and it
is the same call made about `io_uring` two paragraphs up: take the simple
mechanism that is correct everywhere, prove the architecture, and reach for the
better one when there is something to measure. IOCP is Windows' `io_uring`
here.

**What it costs is scale, and the number is knowable in advance.** `poll` is
O(n) in registered descriptors per call, so a hundred thousand waiting sockets
would spend their time in the kernel walking a list. **The socket row of the
table at the top is therefore not claimed**, and epoll, kqueue and IOCP are
what claim it.

What is claimed, and tested against real loopback sockets: a fiber that would
have blocked suspends instead, its worker carries on running everything else,
the right fiber is woken by the right peer, a hangup wakes a reader rather than
leaking it, and a fiber waiting on a socket nobody will ever write to is still
cancellable.

**Linux runs these tests, and one of them crashes there.** This paragraph
used to say the suite passed under WSL2, and that claim was wrong twice over.
The script that produced it had no `set -e` in its inner shell, so a failing
`cargo test` followed by a passing `cargo clippy` reported success; and the
failure it was hiding is intermittent, so even a careful single run would have
had a two-in-three chance of looking fine. Both are fixed — the script now
stops on failure and runs the suite fifteen times — and what it reports is:

> `scheduler::tests::many_sleeping_fibers_all_wake` dies of a signal in
> **17 of 60 runs** on Linux, at `27b051b`.

That test is now ignored on Linux and the crash has been reduced to something
smaller with no timers in it at all.

See [the open problem](#the-linux-crash) below. macOS remains unverified
entirely; `.github/workflows/runtime.yml` is what would close that.

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

### Measured, on Windows: waiting fibers

The criterion's headline row, through the park-and-wake path 11C built:

| fibers | resident | per fiber | woke |
| --- | --- | --- | --- |
| 10,000 | 46 MB | ~4,269 B | all of them |
| 100,000 | 418 MB | ~4,240 B | all of them |

**A hundred thousand fibers waiting at once, 418 MB, and every one of them was
woken and ran to completion.** That is the row this phase exists for, and it is
reached before the reactor — because what a fiber waits *on* does not change
what waiting costs.

What is not yet proven is the third row of the table at the top: fibers waiting
on *sockets* at that scale. The reactor exists and a fiber does suspend on a
real socket without blocking its worker — but on `poll`, which is O(n) per
call. The registration itself is cheap; walking a hundred thousand of them
every millisecond is not. epoll, kqueue and IOCP are what that row waits for.

### Measured, on Windows: idle fibers

11A's context switch uses `corosensei`'s default stacks, which reserve a range
and let the operating system commit pages as they are touched. Building fibers
and running each to its first suspension:

| fibers | resident | per fiber |
| --- | --- | --- |
| 1,000 | 8.8 MB | ~4,177 B |
| 10,000 | 45 MB | ~4,168 B |
| 50,000 | 208 MB | ~4,156 B |
| 100,000 | 410 MB | ~4,155 B |

**A hundred thousand suspended fibers cost 410 MB and did not fail**, which is
one page each and flat all the way up. Against roughly 33 KB for a thread, that
is a factor of eight — and threads stop for other reasons long before a hundred
thousand of them.

So on Windows the target is already reachable and the elaborate slot strategy
above is not yet needed. That is a real result and it is also *one platform*.

**The Linux question is still open**, and it is the specific one this section
predicted: guard pages split mappings, `vm.max_map_count` defaults to 65530,
and a hundred thousand fibers wants more mappings than that. Windows has no
equivalent limit, so the machine this was measured on could not have found it.
The experiment to run is the table above on Linux; if it stops at about 65,000,
the single-reservation strategy is the answer and the numbers here say what it
has to beat.

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

**Built in 11C, and the way it is kept is a rule about ownership.** Three
states — `RUNNING`, `WAITING`, `NOTIFIED` — and: *only the worker running a
fiber holds its `Task`*. A waker never does, so a wake cannot enqueue a fiber
that is still running; it can only leave a `NOTIFIED` behind. The worker reads
the state after the suspension returns and decides — `WAITING` means file it
where a waker can find it, `NOTIFIED` means the wake already arrived and it
goes straight back on the queue.

The gap that leaves — between the worker reading the state and filing the task
— is closed by doing both under the same lock the waker takes. `crate::wait`
holds the state machine; `crate::scheduler` holds that lock, because it holds
the tasks.

Two tests are the ones that matter. `a_wake_racing_a_wait_never_disappears`
runs the race two thousand times and asserts that either the waker owns the
wake or the fiber never waited, never neither.
`a_wake_that_beats_the_park_does_not_strand_the_fiber` is the same property end
to end through the real scheduler, with no timer and no second waker — so a
lost notification hangs rather than fails, which is what the deadline in it is
for.

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
thing to reach for if measurement says the heap is the problem, and not before —
and a hundred thousand timers sort correctly and fire in one pass, so it is not
the problem yet.

Built in 11C on a thread of its own. One thread sleeping is not a worker
blocked, which is the whole distinction; a condvar it could wait on instead of
polling at a millisecond is the obvious refinement and wants a reason.

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
| **11B** N workers, local and global queues, migration, safepoints — **done** | CPU parallelism, and fairness |
| **11C** timers, suspend, wake, and a `poll` reactor — **done** | a hundred thousand waiting on *timers*; sockets need a scalable backend |
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
- Whether a custom stack allocator is needed at all. On Windows it is not:
  4 KB per idle fiber, flat to a hundred thousand. Linux has not been measured
  and is where `vm.max_map_count` would bite.
- Whether Windows' commit accounting makes the single-reservation strategy
  workable there, or whether it needs its own.

## The Linux crash

Four workers, four hundred fibers, each parking once and being woken a
millisecond later: `SIGSEGV` in roughly a quarter of runs on Linux, never on
Windows. Present at `27b051b`, which is the scheduler's own commit, so it
arrived with this design and not with anything layered on since.

Two tests carry it. `many_sleeping_fibers_all_wake` found it and is now
`#[ignore]`d on Linux; `park_and_wake_at_scale_reproduces_the_linux_crash` is
the reduction and is ignored everywhere. Both are in `scheduler.rs`.

### What it is

The faulting thread is executing corosensei's stack switch, inlined into
`park_current` — `rax` holds `park_current+270`, which resolves to the switch
in `corosensei/src/unwind.rs`. `rip` is `0`. So the switch ran and jumped to a
parent link that was not a return address.

Where it landed is the useful part. In a core dump, two threads have
**byte-identical stack pointers**:

    LWP 1  sp=0x7aadfbffeb18  pc=(nil)          <- the fault
    LWP 5  sp=0x7aadfbffeb18  pc=0x7aae00734c8d <- parked in the worker condvar

`0x7aadfbffeb18` is LWP 5's own thread stack — the arithmetic is unambiguous,
since every worker sits exactly `0xba8` below its TLS block and the stacks are
`0x201000` apart. The faulting thread switched onto another worker's live
stack, at the frame where that worker is asleep waiting for work.

### What it is not

  - **Not corosensei.** `coro.rs` has
    `a_coroutine_survives_being_resumed_by_a_different_thread`: four hundred
    coroutines, four threads, four hops each, the same install-on-entry and
    re-install-after-wake dance, and no scheduler. Clean over forty runs. Its
    docs warn that yielder *references* must not cross threads, which is a
    narrower claim than migration being unsafe, and the test is there so that
    distinction stays checked rather than assumed.
  - **Not the timers.** The reduction has no deadlines, no `Timers`, and no
    timer thread.
  - **Not a panic.** `stderr` is empty on a crashing run.
  - **Not a fiber that never suspends.** A waker that spins instead of sleeping
    a millisecond is clean over forty runs, because the fibers take the
    already-notified path in `declare` and never park, so nothing migrates.
    Migration is necessary; the millisecond is what produces it.
  - **Not a stack overflow.** corosensei's default stack is a megabyte with a
    guard page, and these bodies are shallow.

### The part that makes it hard

**It disappears under observation.** Under `gdb`, under `valgrind`, and under
every assertion written to test a theory about it, the rate goes to zero. A
probe comparing the thread-local yielder against one recorded on the fiber
itself reported no mismatch in forty runs — and also crashed zero times in
those forty runs, so it proved nothing. Each of double-resume,
resume-after-finish, foreign-thread drop and wrong-yielder was tested the same
way and is unrefuted rather than ruled out.

Core dumps are the way around this, and they cost nothing: `core_pattern` is
already `core` on WSL2, so `ulimit -c unlimited` in a writable directory and a
loop of eighty runs produces one in under a minute, with no debugger attached
and no timing disturbed.

### Nothing shipped depends on it

`Fiber::spawn` still gives each fiber an operating-system thread. The coroutine
scheduler is not wired into it, and no compiled Khora program reaches this
code. That is the only reason this is written down rather than blocking
everything else — but it does block 11D, since work stealing makes migration
more frequent, not less.

### Where to look next

The evidence points at a parent link being followed after it stopped being
current, on a thread that was not the one that installed it. The two places
that can produce that are the `YIELDER` thread-local in `coro.rs` and the
handoff in `wake`/`run` around the `parked` map. ThreadSanitizer on nightly is
the untried tool that is actually built for this; a nightly toolchain is
installed in WSL2 for it. Failing that, reduce further — the reduction above is
forty lines from being a program with nothing in it but the bug.
