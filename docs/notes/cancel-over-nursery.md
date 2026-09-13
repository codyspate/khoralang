# Cancelling a fiber whose body is a nursery never returned

Status: **fixed.** Kept because the diagnosis took four attempts and three of
them failed for reasons worth writing down.

## What happened

```khora
let hand = Fiber::spawn(fn () => body(seen)!);   // body opens a nursery
clock.sleep(100);
Fiber::cancel(hand);      // returned immediately
Fiber::wait(hand);        // never returned
```

No exit code, no message, no backtrace. The same worker spawned directly and
cancelled the same way stopped in about 100 ms. The nursery was the whole
difference.

## Why

`khora_fiber_cancel` flagged the one fiber it was handed. That fiber was inside
`khora_fibers_wait`, blocked in `JoinHandle::join` on a child nobody had told to
stop — so it could not reach the point where it would have read its own flag.
The child looped forever. The parent waited forever.

`khora_fibers_wait` does check for its own cancellation between rounds, and that
check is correct. It simply cannot fire: the cancellation arrives *during* a
join, and the next round comes after the join it is waiting on ends.

The trace, with the instrumentation that settled it:

```
[wait] round took 1 child(ren); held now 0     <- the crew is emptied
[wait] between-rounds check: stopping=false    <- runs before the cancel
[wait] joining child 0...
[wait]   child 0 is fiber 3
cancelling...
[cancel] flagging fiber 2                      <- the parent, and only the parent
cancel returned; waiting...                    <- forever
```

Fiber 3 is never mentioned again.

## The fix

Three parts, and all three are needed.

1. **`OPEN`**, in `nursery.rs`: every open nursery and the fiber that opened it.
   Cancellation is *delivered* to the children rather than waited for, because
   waiting for the parent to notice is what deadlocked.
2. **`Children::joining`**: the round a wait is currently joining stays visible.
   `std::mem::take(&mut crew.held)` moved those children into a local variable,
   which made exactly the fibers a cancellation needs to reach findable by
   nobody. This was invisible until `cancel_open_crews` reported `crew with 0
   child(ren)` on a nursery that plainly had one.
3. **Nothing locked while a child is cancelled.** The handles are copied out
   from under both locks first. A child's exit path takes the crew's lock to
   deregister itself, so cancelling while holding it deadlocks — and a deadlock
   here is indistinguishable from the original hang, which is why the second
   attempt looked like no progress at all.

Measured after: **106 ms**, against 107 ms for the same child with no nursery.

## What the first three attempts got wrong

**The stale archive.** `cargo build --bin khora` does not rebuild
`libkhora_rt.a`, and every compiled Khora program links that archive. Three
rounds of "still hangs" were testing a runtime from hours earlier. Any runtime
change needs `cargo build -p khora-rt` *before* `--bin khora`. The in-tree test
harness gets this right — `harness::ensure_runtime` rebuilds it — which is worth
knowing, because it also means disabling a fix and rebuilding by hand does not
reach a `cargo test` run.

**The wrong subsystem, twice.** An earlier note recommended recording a parent
on each `Fiber` so cancellation could walk the tree. That is a real thing to
want and it does not fix this: the walk needs a fiber's *nurseries*, not its
children, and on the thread backend there is no registry of live fibers to walk
in the first place. A second attempt moved the check into `std/core.kh`, where
it duplicated a check `khora_fibers_wait` already performs — and misses for the
same reason.

**The control experiment is what unstuck it.** Running the identical child body
under a direct `Fiber::spawn` — 107 ms — exonerated codegen and the scheduler by
measurement rather than by argument, and turned a three-subsystem guess into a
one-file target. It should have been the first thing tried, not the fourth.

## The regression test

`cancelling_a_fiber_inside_a_nursery_returns`, in
`crates/khora-codegen-llvm/tests/fibers.rs`.

It sleeps 100 ms before cancelling, and **that pause is the test**. A
cancellation arriving before the parent enters the join is caught by the
between-rounds check and succeeds even against the unfixed runtime; without the
pause the test passes on the very defect it exists to catch. It also runs the
program under a deadline of its own, because a regression here is a hang, and a
hang under `Command::output` takes the whole suite with it instead of failing
one test.
