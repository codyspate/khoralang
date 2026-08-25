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

**Verified on Linux as well as Windows — after Linux found a real bug that
Windows could not.** `scripts/check-linux.sh` runs these tests under WSL2, a
real kernel with real sockets and the real `poll`, and `scripts/baseline.sh`
calls it whenever `wsl` is present.

It earned its keep immediately. `many_sleeping_fibers_all_wake` was dying of
`SIGSEGV` in 17 runs out of 60 there while passing every single time on
Windows — see [the cached thread-local](#the-cached-thread-local) below, which
is the most valuable thing this phase has produced. Both platforms are green
now, and the script runs the suite fifteen times rather than once, because one
green run of a racy suite is not evidence.

That leaves **macOS as the only unverified platform**, and it reaches `kqueue`
through the same `poll` call with the same struct, so the remaining risk is
narrow. `.github/workflows/runtime.yml` is what closes it, and it is now
essentially a macOS-only job.

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

**11F found the shape of the eventual problem, and left it alone.** A fiber
released before its deadline — woken by I/O, or cancelled — leaves the deadline
in the heap, because taking it out means rebuilding the heap and doing that per
wake is quadratic. The entry is discarded when it comes due, so the waste is
bounded by how long deadlines are and how often sleepers are released early,
not unbounded. `timers_dead` counts them: 367 against 2,668 fired in one soak, about one in
seven, in a workload built to release sleepers early and so an over-estimate of
what a real program would see. The rule above still holds — measure a real
program before replacing the heap — and this is the counter to measure.

### The heap is off the socket path entirely, which was the whole problem

**Closed, and the answer was worse than the anomaly.** The counters said a
`bench/service` run registered 763,737 ten-second deadlines in a process that
lived about ten seconds, and that 692,795 of them came due — impossible on the
face of it, and every one belonged to a fiber the pool no longer had in `live`.

11J found why by measuring what nobody had: `timers_added` and `sockets_ready`
were **the same number**, 1,262,225 against 1,262,221. Every socket read that
would block was pushing a deadline onto this heap — a global mutex and an
`O(log n)` insertion apiece — and the heap was growing to a million entries of
deadlines the process would never see come due. Roughly thirty megabytes per
five seconds of load, released only as deadlines passed, which on a server is
memory that grows with request rate and reads as a leak.

A deadline belongs to the wait it bounds, and the reactor already holds every
wait, so it rides on the `Watch` now. `poll` shortens its own timeout to the
soonest deadline and reports whatever has passed, a timeout and a readiness
leave by the same door, and the same benchmark registers **zero** timers. The
heap is for `sleep` again.

It bought no throughput — 62% of the thread figure against 63% before, inside
the noise — so the mutex was never the contended thing. It bought the memory
back and it made deadlines exact, and the anomaly went with it: there is
nothing left to be anomalous about.

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

11E built it, in `crates/khora-rt/src/blocking.rs`. Two limits: how many
threads may exist, and how much work may be queued for them. When the queue is
full the **fiber** waits rather than its worker, so a program that
oversubscribes the pool gets slower instead of larger — that is the whole of
what backpressure means here. Threads start on demand and retire after ten
seconds idle, so a program that never blocks never pays for any of this, and
one that blocked at startup does not keep the threads for its whole life.

`KHORA_BLOCKING_THREADS` overrides the default of two per core. That default is
deliberately modest and is a starting point rather than a result; the counters
below are how it should be argued with.

**A blocking call is not a cancellation point.** A fiber cancelled inside
`fread` keeps waiting. The pool cannot interrupt foreign code, and returning
early would mean handing the fiber back while another thread still holds its
buffer. The cancellation is observed at the next `!`, which is the rule
safepoints already follow — §1.

**The caller is `std::fs`.** Its `open`, `read`, `write` and `close` used to be
foreign calls straight to C stdio, which was right while a fiber was a thread
and became wrong the moment it was one of many on a worker: a read of a cold
file held the worker and everything queued behind it. They now go through
`khora_fs_*`, which does the C call on a pool thread. `fseek` and `ftell` stay
direct — they adjust a buffer rather than reach a disk, and both deal in C's
`long`, which is thirty-two bits on Windows and a question worth settling on
its own.

Two things worth keeping from building it:

  - **Wake the fiber last, after the pool's own bookkeeping.** Waking it from
    inside the job lets it run, finish, and have somebody read the counters
    while the pool thread has not yet decremented `active` or counted `ran`.
    The pool never gives itself a wrong answer that way, but an observer sees
    one, and it failed two tests about one run in ten with `active: 1, ran: 39`
    after everything had finished. A fiber back from a blocking call should be
    proof that the call is accounted for.
  - **Grow when the queue outruns the *idle* threads, not when every thread is
    busy.** A thread that has been spawned but has not yet reached its first
    job is neither busy nor able to help, so `active == started` reads as "we
    are keeping up" at exactly the moment the pool most needs another thread.
    Forty jobs arriving faster than one thread could pick them up left the pool
    at a single thread, running the lot in series.

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

11D built the third of those. A worker takes from the front of its own deque
and a thief takes from the back, so the two contend for a lock but never for a
fiber, and the owner keeps the end it is about to touch. A sweep takes half a
victim's queue rather than one fiber, because taking one puts the thief
straight back into contention. Victims are visited starting at `me + 1` so
that idle workers do not all descend on worker 0.

Measured on Linux, one fiber spawning eight hundred children of a hundred
microseconds each:

| workers | 1 | 2 | 4 | 8 |
| --- | --- | --- | --- | --- |
| time | 99.9 ms | 59.3 ms | 46.9 ms | 34.2 ms |
| fibers moved | 0 | 396 | 994 | 1,324 |

Sublinear, and the reason is visible in the shape of the test rather than in
the scheduler: one fiber has to create eight hundred coroutine stacks before
there is anything to steal, and that part is serial. What the first column
shows is the point — without stealing every one of these is the one-worker
number, because everything a fiber spawns lands on its own worker's queue and
nobody else could reach it.

**Parking is where this went wrong twice**, and both mistakes are recorded in
`park` because they are easy to make again. A parked worker must return to the
scheduling loop to steal, since stealing lives there; a version that kept
re-checking the shared queue and its own from inside the parking loop left
three workers asleep through twenty milliseconds of work in the fourth one's
queue. And the opposite fix — check every worker's queue and stay awake if any
has something — livelocks, because seeing work is not taking it, and four
workers spinning on a losing steal can starve the one making progress.

---

## 10a. Where the reactor should end up

11H established that the reactor's wake path is what a request costs on the
scheduler, and left the architecture as it was: readiness is discovered on a
thread of its own and handed to a worker. This is where that should go, written
before it is built so the reasons survive the building.

**The framing that makes it obvious.** A fiber becomes runnable because a timer
expired, because a join completed, because a cancellation arrived, or because
its socket can make progress. Those are four spellings of *this fiber can run
now* — and three of them already reach the scheduler directly while the fourth
crosses an operating-system thread first. **I/O readiness is a scheduling
event**, and the only reason it is treated as an outside interruption is that
the reactor was easiest to write on its own thread.

So the shape to move toward:

```
Khora application  — synchronous, direct style, no colour
        │
   a blocking-looking call
        │
Khora scheduler    — local queue, injection, stealing, timers, I/O progress
        │
platform backend   — epoll / kqueue / IOCP
```

rather than a scheduler and a reactor side by side exchanging queues. That is
not a call to collapse the modules: `reactor.rs` and its backends stay a clean
boundary. It is a call to stop making an OS-thread hop mandatory because of how
the mechanism was first implemented.

### What not to touch on the way there

11H measured the scheduler under load and the result points in one direction
only. **Preemption, work stealing, the context switch itself and the local
queues are not implicated by any measurement**, and one of them is positively
exonerated: turning preemption off entirely moved throughput by three per cent,
on a workload where 78% of resumes ended in it.

So none of them should be optimised on suspicion. If a later measurement
implicates one, that measurement is the licence; until then, effort spent there
is effort not spent on the wake path, and a change made there muddies the next
benchmark.

### Workers should poll when they have nothing to run

The worker loop becomes local queue, then injection, then steal, then *ask the
backend for progress*, and only then park. A worker that polls runs the fiber it
just found, instead of waking another worker to run it.

**Who is allowed to block in the backend is the question that decides whether
this helps**, and it does not have one answer, which is itself an argument for
keeping the abstraction operation-oriented. `epoll_wait` from every idle worker
is a thundering herd unless it is asked for exclusively; an IO completion port
with several threads dequeuing is IOCP working exactly as designed. The
plausible shapes are one designated poller among the idle workers, with
ownership handed on when that worker gets work, or short non-blocking polls by
any worker with one taking the blocking wait when the whole pool is idle. They
should be compared, and the comparison belongs behind the backend boundary
rather than above it.

### Keep the interface operation-oriented, whatever the backend does

§2 already argues this and it becomes load-bearing here. epoll and kqueue
report *readiness*; IOCP reports *completion*. An interface shaped like the
first forces the third to pretend, and the pretence is where correctness goes.
What the scheduler asks for is "tell me when this operation can make progress",
and what a backend does about that is its own business.

### What may not change to get it

The runtime exists to keep the language simple, so a runtime change that
complicates the language has the bargain backwards. Any new I/O architecture
has to leave all of these true:

  - Khora source stays synchronous and direct — no `async`, no `await`, no
    futures, no second colour of function, no executor handles;
  - a socket that would block suspends a **fiber**, never a worker;
  - safepoints stay separate from cancellation points — §1;
  - cancelling a fiber waiting on I/O makes it runnable, so its finalizers run;
  - the lost-wakeup invariant holds — `crate::wait`;
  - nursery and structured-concurrency semantics are untouched;
  - fibers migrate between workers, and no thread identity is ever a fiber
    identity;
  - the thread-local rule survives — see the cached thread-local below;
  - an `extern` call cannot silently suspend across thread-affine foreign code
    — §8;
  - a `Task` has exactly one owner at any instant, which is what `Audit`
    checks.

**If a design benchmarks better but requires any of those to bend, it is the
wrong design.** A faster HTTP number bought with `await` in Khora source would
be a bad trade at any ratio.

### What "good enough" is

Not parity with thread-per-connection on 48 connections, which is a workload
unusually kind to operating-system threads and unusually unkind to a scheduler
— the fibers there are never numerous enough for the scheduler's advantage to
appear. But the scheduler should not keep a gross penalty at low concurrency as
the price of a hundred thousand waiting fibers.

**Seventy to eighty-five per cent of the thread figure** is the target, which
against 782,149 is roughly 530,000 to 650,000. E2 reached 613,571 by spinning,
so the target is known to be mechanically reachable rather than hoped for.

### Measure the stages before rebuilding anything

The remaining gap is 429,000 against 613,571 spinning, and nothing yet says
which part of the wake path it is. Before the redesign, sample the path in
stages — registration, nudge, backend return, readiness decoded, wake begun,
parked task taken, injection, worker receives, `Task::resume` begins — and
report p50, p90 and p99, with per-request counts of socket waits, injections,
condvar wakes, `live` lookups, `parked` acquisitions and the syscalls spent
waking the reactor — the nudge is two of them, a write and the read that drains
it, and 11H did not measure how often they are actually paid.

That is the same discipline as 11H's trail, and 11H is the argument for it: the
counter that looked most damning was the one that cost nothing.

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

**And speculative busy-spinning, which needs saying because 11H did it.** E2
spun the reactor instead of sleeping and recovered ten times the throughput;
that was a diagnostic, spending a core to find out where the time went, and it
is written up as one. A runtime that burns a core to look fast on a benchmark
is not a runtime anybody can run several of on one machine.

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
| **11C** timers, suspend, wake, a `poll` reactor, and socket calls that suspend a fiber — **done** | a hundred thousand waiting on *timers*; sockets need a scalable backend |
| **11D** work stealing — **done** | locality, once the simple scheduler's behaviour is understood |
| **11E** bounded blocking pool — **done** | that unavoidable blocking cannot stall a worker |
| **11F** scale and soak — **done** | the adversarial tests below |

Khora stays buildable throughout. Threads remain the implementation on any
platform whose backend has not landed.

## 14. Instruments, from the beginning

Cheap counters, sampled or compiled out, not added after something is slow:

workers; runnable, running and waiting fibers; creations and completions;
steals attempted and succeeded; reactor wakeups; parks and unparks; timers
outstanding; blocking-pool queued and active (`khora_blocking_queued`,
`khora_blocking_active`, `khora_blocking_ran`, `khora_blocking_waited`);
cancellations requested and
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

### What 11F built instead, and why

`crates/khora-rt/src/soak.rs`. Eight workloads mixed by a seeded generator —
one for each ownership transition, plus the reactor — with two threads
interfering: an adversary that
wakes and cancels fibers at random, including ones that finished long ago, and
a releaser that gives every parked fiber exactly one wake.

**Exactly one, and that is the whole design.** Waking until something moves
would hide a lost wakeup, which is one of the things this exists to find. The
protocol has to survive a single wake landing on either side of the park.

The deterministic scheduler was not built, because the thing it buys —
replaying a failure — turned out to be available more cheaply. What a scheduler
gets wrong is not a wrong answer but a wrong *count*, and counts can be checked
exactly at the one moment when nothing is moving:

  - `Scheduler::audit` names all six places a fiber can be — the shared queue,
    a worker's queue, the parked map, in transit between two of those, a
    worker's hand, or finished — and `Audit::settled` says the pool is empty
    and self-consistent.
  - `Scheduler::settle` waits for that, because `drain` answers a different
    question: every fiber can have finished while deadlines are still
    registered.
  - `coro::ResumedOnce` aborts at the instant two workers enter one coroutine,
    which is stronger than any count, and costs nothing outside debug builds.
  - Every fiber in the soak asserts its own identity on every resume.

**What cannot be checked while the pool is busy, and the hour it took to
accept that.** `Audit` reads six places without a lock across them, so a task
moving from one to another can be seen in both, and the arithmetic goes
negative with nothing wrong. Making it sound while busy needs a single counter
maintained at every push and pop — an atomic on the hottest path in the file,
to catch what quiescence catches for free. The one thing that does survive a
skewed read is `completed <= spawned`, because `audit` reads `completed`
before everything else and `spawned` after everything else; that one is
asserted throughout the run.

A hang is the least informative failure a soak can produce, so a watchdog turns
one into a state dump and aborts. It earned itself twice over, both times
reading `parked: 1, waiting: 1` with nothing else outstanding — and both times
the fault was the test's, not the scheduler's. Without the dump those are
indistinguishable from a lost wakeup, which is exactly the bug the soak exists
to find, and either one would have been believed.

### What it found

  - **A waiting count that went negative.** `NOTIFIED` means two things —
    "a wait is being released" and "do not sleep next time" — and only the
    first owes the waiting total anything. A fiber that took a wake while
    running and then yielded for fairness had a decrement charged against a
    wait it never made. `Wait::start_counting` pairs them properly, and
    `a_wake_for_a_running_fiber_is_not_counted_as_a_wait` is the deterministic
    case.
  - **A sixth place a fiber can be.** A waker holds a task between taking it
    out of the parked map and injecting it; a thief holds half a queue between
    two others. Neither is a worker, so neither is bounded by the worker count,
    and an audit that did not know about them reported ten fibers unaccounted
    for on four workers.
  - **An unwritten precondition on `wake_fiber`.** It can only wake a fiber the
    pool has been told about, and `spawn` is what tells it; a wake that arrives
    first finds nothing and is dropped. That is right — remembering
    notifications for fibers that may never arrive is an unbounded leak — but
    it is a trap for anything that publishes a fiber's id before handing over
    its task, and the soak fell into it and stranded a fiber about twice in a
    hundred runs. Now written down where the function is.

### What it ran

Eight workloads, four worker counts, on both platforms:

| | Windows | Linux |
| --- | --- | --- |
| runs of 6,000 rounds | 120 | 120 |
| failures | 0 | 0 |
| repeated passes, one process | 5,771 in 5 min | 4,648 in 4 min |
| resident drift over those | 880 KB | 684 KB |

The repeated-pass figure is the leak check, and it has to be one process:
every individual soak proves its own pool came back to empty, and none of them
can see something accumulating *between* pools. Five thousand schedulers built
and destroyed, something over two million fibers, and resident memory flat to
within a megabyte.

### Scale

A thousand, ten thousand and a hundred thousand fibers, all waiting at once,
then all woken. Windows and Linux agree to within one per cent:

| | 1,000 | 10,000 | 100,000 |
| --- | --- | --- | --- |
| resident | 4 MB | 40 MB | 407 MB |
| per fiber | 4,464 B | 4,290 B | 4,266 B |
| mappings (Linux) | 2,069 | 20,068 | 200,068 |
| round trip (Windows) | 12 ms | 104 ms | 1.04 s |
| round trip (Linux) | 41 ms | 419 ms | 4.49 s |

About 4.3 KB a fiber against roughly 33 KB for a thread, and flat with scale.

**`vm.max_map_count` is answered.** Exactly two mappings per fiber — a guard
page and the stack — so the traditional default of 65,530 caps a program near
**32,700 fibers**, and nothing about that is visible until the allocation
fails. The kernel this was measured on allows 1,048,576, which is why 100,000
works here. A slot allocator that carves many stacks out of one mapping is the
answer if it ever matters; it is not needed to reach the phase's number on a
modern kernel, and knowing which kernels need it is worth more than building
it now.

---

## What this document does not decide

- Whether the prologue check is one instruction sequence serving both the stack
  limit and the safepoint, or two. It should be measured in 11A.
- Whether a custom stack allocator is needed at all. On Windows it is not:
  4 KB per idle fiber, flat to a hundred thousand. Linux has not been measured
  and is where `vm.max_map_count` would bite.
- Whether Windows' commit accounting makes the single-reservation strategy
  workable there, or whether it needs its own.

## The cached thread-local

The worst bug in this phase, and the one most likely to come back in another
form, so it is written up at length.

**The symptom.** Four workers, four hundred fibers, each parking once and being
woken a millisecond later: `SIGSEGV` in roughly a quarter of runs on Linux,
never once on Windows. The faulting thread was inside corosensei's stack
switch, inlined into `park_current`, with `rip` at zero — the switch had
followed a parent link that was not a return address. A core dump showed two
threads with **byte-identical stack pointers**, and the address belonged to
another worker's own thread stack, at the frame where that worker was asleep in
the condvar waiting for work.

**The cause.** `coro.rs` kept the running fiber's yielder in a thread-local:

```rust
let yielder = YIELDER.with(|y| y.get());
unsafe { (*yielder).suspend(()) };   // may come back on a different worker
YIELDER.with(|y| y.set(yielder));    // ... and this is the bug
```

A thread-local is reached through a base address the compiler holds in a
register, and the compiler is entitled to compute that address once and reuse
it across the whole function. Nothing in the language says a thread can change
in the middle of one. Here it can — that is what a scheduler *is* — so the
second access wrote through an address computed before the switch, into the
thread-locals of the worker that used to be running this fiber.

The lost write is the smaller half. The thread that should have received the
yielder never got it, so it kept a stale one, and its next suspension switched
to a stack pointer belonging to some other worker. The crash therefore lands on
a different thread, at a different time, doing something unrelated.

**The fix** is three lines: put each access behind an `#[inline(never)]`
function, so the address is computed inside the callee, which runs after the
switch on the thread that is actually executing. `coro::installed` and
`coro::install`. The inline assembly in the switch does not help on its own — it
clobbers memory, and a cached thread-local address is a register.

**What found it.** Not reading the code; that was tried for a long time. Every
theory — double resume, resume after finish, dropping on a foreign thread, the
wrong yielder — was tested with an assertion, and none fired, because *any*
instrumentation perturbed the timing enough to hide the crash. Under `gdb` and
under `valgrind` the rate went to zero.

Two things worked, and both are worth keeping:

  - **Core dumps**, which disturb nothing. `core_pattern` is already `core`
    under WSL2, so `ulimit -c unlimited` in a writable directory and a loop of
    eighty runs produces one in under a minute with no debugger attached. Two
    threads sharing a stack pointer is not something an assertion would have
    told us.
  - **ThreadSanitizer**, which named it outright:

        Read of size 8 at 0x7ffff43fd668 by thread T2:
        Previous write of size 8 at 0x7ffff43fd668 by thread T5:
        Location is TLS of thread T2.

    Three reports before the fix, all on that slot. After it: zero, across a
    clean completed run of the crashing test and a run of the suite that got
    through 64 tests.

TSan is itself unreliable here and the numbers above should be read with that
in mind. It needs `__tsan_switch_to_fiber` around a stack switch, corosensei
only annotates for AddressSanitizer, and so a run has perhaps an even chance of
dying on its own shadow memory before the suite ends. It reports races on
ordinary memory perfectly well up to that point, which was enough to name this
one — but the load-bearing evidence that the fix works is not TSan. It is 80
runs of the reduction and 60 of `many_sleeping_fibers_all_wake` without a
single failure, against roughly one in ten and 17 in 60 before.

**What it means beyond this bug**, and the first version of this paragraph
got it wrong, which is worth leaving on the record. It said the other
thread-locals — `CURRENT`, `SHARED`, `LOCAL`, `REMAINING` — had been checked
and were fine, on the reasoning that each reads into a value before suspending
and never touches the thread-local again. That reasoning is sound about a
single call and useless about a loop: the compiler hoists the address
computation out of one, and then the *caller's* suspension sits between two
uses of it.

11D proved it within a day. `a_fiber_keeps_its_identity_across_workers` asks a
fiber who it is in a loop with a suspension in it, and once stealing made
migration common it started answering with somebody else's identity — `left:
30, right: 28` — because `current()` was inlined and its address hoisted. The
worker panicked, and the fiber it was holding went with it, so the visible
symptom was a scheduler that lost a task and hung in `drain`.

So the rule is applied mechanically now rather than argued site by site:

> **Never let a thread-local address be computed on one side of a suspension
> and used on the other.** In practice: every thread-local in `khora-rt` is
> reached through an `#[inline(never)]` accessor.

`current.rs` has `running`, `set_running`, `swap_running` and `root_fiber`;
`coro.rs` has `installed` and `install`; `scheduler.rs` has `local_queue`,
`shared_pool` and `attach`. `#[inline(never)]` puts the address computation in
the callee, where it runs on the thread actually executing, and the switch's
inline assembly clobbers memory so the loaded *value* cannot be carried across
a suspension either.

**`REMAINING` is the one exception, deliberately.** The safepoint budget is per
worker, not per fiber: `refill`, the check and `withdraw` all run inside `run`
on the worker, and `khora_safepoint` reaches it across an `extern "C"` boundary
that forces a fresh computation on every call. No access to it can straddle a
switch. It is also the only hot path in the file — one call per loop back-edge
— so it is the one place where an un-inlinable call would show up.

`coro.rs` also gains
`a_coroutine_survives_being_resumed_by_a_different_thread`, from the day spent
suspecting corosensei rather than ourselves: four hundred coroutines, four
threads, four hops each, no scheduler underneath. corosensei's docs warn that
references to a `Yielder` must not cross threads, which is a narrower claim
than migration being unsafe, and that distinction should stay checked rather
than re-argued.
