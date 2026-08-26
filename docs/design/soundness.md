# The runtime soundness audit

Roadmap 13.6. An inventory of every place the runtime steps outside what the
compiler checks, what each one depends on, and which of those dependencies is
enforced rather than merely believed.

**Three defects found, all fixed. One was reachable.** The other two were
mis-declarations rather than live bugs, and are the kind that make the next one
invisible.

ThreadSanitizer found nothing in the thirty-five tests it can run. It cannot
run the scheduler's, which is recorded below rather than glossed over.

## The surface

| | |
| --- | --- |
| `unsafe` blocks | 179 (146 in `khora-rt`, 33 in the code generator) |
| C symbols exported to generated code | 100 — 61 `unsafe fn`, 39 safe |
| `unsafe impl Send` | 3, all in `khora-rt` |
| Thread-locals | 6 |
| Places a fiber suspends from Rust | 4 |

Everything outside `khora-rt` and `khora-codegen-llvm` is safe Rust; the two
`unsafe` mentions elsewhere are in comments.

## Finding 1 — a `main` program that publishes a symbol counted without atomics

**Reachable, and memory corruption when reached.**

Generated code counts references non-atomically when the compiler can prove the
program has one thread. The proof was: this is a `main` build, and no body
mentions `Fiber::spawn`. `khora_fiber_spawn` aborts if that proof turns out to
be wrong, so a spawn cannot sneak past it.

A second thread can get in without a spawn. `emit_c_exports` runs for **every**
entry point, so a `main` program containing an `export extern fn` publishes
that symbol — and a C library it is linked against will call the callback it
was handed on whichever thread it likes. That program never writes
`Fiber::spawn`, so it was compiled with non-atomic counting for a function a
foreign thread can enter.

There is no way to observe this going wrong except as corruption long
afterwards, which is why the fix is a condition rather than a note.
`counts_non_atomically` is now a named function with seven tests, including one
that fails if the export condition is removed.

## Finding 2 — four exported functions declared no preconditions and had them

`std::fs`'s four shims — `khora_fs_open`, `_read`, `_write`, `_close` — take
raw pointers from Khora and hand them to a thread in the blocking pool. Each
already carried a `SAFETY` comment discharging the dereference.

**But the obligation those comments discharge had never been given to anyone.**
A safe `extern "C" fn` says there is nothing to get wrong. Generated code
cannot tell the difference; a Rust caller can, and the runtime's own tests are
Rust callers — which is exactly what happened: the tests called all four with
no `unsafe` and no justification, and the compiler had no reason to ask.

Now `unsafe fn` with `# Safety` sections, like the other sixty.

## Finding 3 — `khora_array_new` keeps a drop routine and was safe

Same shape, found by scanning rather than by reading. It takes
`glue: Option<extern "C" fn(*mut u8)>`, stores it, and calls it once per
element when the array is released. A routine belonging to another type
releases these elements through the wrong field list.

That is not hypothetical. The code generator made exactly that mistake in the
week before this audit — drop glue was cached by a type's *printed* name, and a
program importing both `std::net::http`'s `Request` and `postgres::db`'s got
one routine for two layouts. `khora_shared_open` and `khora_channel_open` take
the same argument and both say so; this was the outlier.

## What is now enforced rather than believed

**`crates/khora-rt/tests/ffi_surface.rs`** reads the runtime's own sources and
fails if any exported function takes a pointer, or keeps a `glue` routine,
without being `unsafe`. It is the reason there will not be a fourth of these.
Verified by reverting finding 3 and watching it fail.

**`counts_non_atomically`'s tests** cover each way a second thread arrives: a
spawn, a published symbol, a library, a test binary — and the two cases that
must *stay* fast, a plain `main` and one that merely declares a foreign symbol
without publishing one.

## What was checked and found sound

**`unsafe impl Send for Task`** — a suspended fiber's stack crossing to another
worker. Its argument rests on three premises, and each holds: reference counts
are atomic whenever a program can spawn (finding 1 was a hole in exactly this,
now closed); what crosses into a fiber is `Share`-checked; and no fiber suspends
inside an `extern` call. The third is policy rather than a type, and is
recorded below as still unenforced.

**Thread-affinity.** Six thread-locals. Four are per-worker by construction —
the run queue, the yield budget, the coroutine yielder, the trap flag. The
running-fiber slot is the dangerous one and was already hardened: `running()`
is `#[inline(never)]` so the thread-local's address is computed on the thread
that executes, with a named regression test. That was found the hard way once
already.

**Trap containment cannot span a migration.** Its registry is a thread-local,
which would be wrong if a guarded call could move workers. It cannot: the guard
is emitted in the C export wrapper, so it only runs on a thread the host
entered on, and `khora_fiber_spawn` calls `contain::disarm()` — so a guarded
call that starts a fiber gives up containment rather than freeing under it.

**The residual obligation on Rust bodies** — anything held across a suspension
must be `Send` — is met at all four suspension points. Each drops its
`MutexGuard` before parking. One of the four relied on a temporary-scope rule
rather than an explicit `drop`; it is now explicit, because a safety property
should not need the reader to recall when an `if let` scrutinee's temporary
dies.

**Reference counting.** Relaxed increment, release decrement, acquire fence on
the last one. The textbook pair, correctly applied.

**Unwinding.** `fatal` aborts rather than panicking, because these functions are
entered across a C ABI with no frames to unwind. Panics inside a fiber unwind
across a stack switch safely — `corosensei` emits the CFI for it, which is one
of the reasons it was chosen over writing the assembly here.

**The blocking pool cannot exist without a scheduler.** `blocking_on` returns
`work()` inline when `waker_for_current()` is `None`, which is the case in
every program that never spawns. That line reads as a convenience and is doing
safety work: it is why a single-threaded program has no pool thread touching
its non-atomic reference counts. Written down here because nothing at the site
says so.

## ThreadSanitizer

`sh scripts/tsan.sh`. Thirty-five tests across five modules, **no warnings**.

| Module | What it covers |
| --- | --- |
| `channel` | A bounded queue with senders and receivers on real threads — the newest primitive here, and the one carrying values between fibers |
| `wait` | The park/wake protocol, whose entire content is the race between a suspension and the wake that beats it |
| `contain` | Thread-locals and the trap flag |
| `decimal`, `trap` | Single-threaded; cheap to include |

**It cannot see through a stack switch, and that is not a theoretical
reservation.** TSan keeps shadow state per thread; `corosensei` moves a whole
stack between workers without telling it. Pointed at `blocking::`, whose tests
run their work on a `Scheduler`, it does not produce false positives — it dies:

```text
ThreadSanitizer: SEGV on unknown address 0x7ffff6a00000
ThreadSanitizer: nested bug in the same thread, aborting.
```

That is the sanitizer reading a fiber's guard page, before any test result. So
**the scheduler, the fibers, the reactor and the blocking pool are not covered
here** — not because they are trusted but because the tool cannot answer for
them. Annotating the switch with `__tsan_switch_to_fiber` is the supported
answer and is not reachable from safe Rust today; it is what would extend this.

Three things about the setup are worth keeping, because each cost time:

- **`-Zbuild-std` is not optional.** The host and target are the same triple,
  so the toolchain's precompiled `std` is a link candidate and was built
  without the sanitizer. `rustc` refuses that as an ABI mismatch on the first
  dependency it reaches.
- **The flags must be target-scoped.** Plain `RUSTFLAGS` instruments build
  scripts and proc macros too, and those link against the host `std`. The fix
  is `CARGO_TARGET_<TRIPLE>_RUSTFLAGS`.
- **The verdict is a sentinel line, not an exit status.** Run through
  `wsl -e bash -lc`, the inner status is lost: every module printed
  `test result: ok`, every filter returned zero when run alone, and the
  invocation still came back `1`. The script now says `KHORA_TSAN_ALL_CLEAR` in
  words and the caller greps for it.

## What is still open

**A fiber must not suspend inside an `extern` call**, or a C library's
thread-affine state is live across a migration: wrong `errno`, wrong
thread-locals, a lock held by nobody. This is `scheduler.md` §8's policy and
nothing enforces it.

It is **unreachable today**, and that was checked rather than assumed. The only
way to suspend with C frames on the stack is for foreign code to re-enter
Khora, and it cannot: `docs/design/ffi.md` records under "still open" that a
Khora function cannot yet be passed as a C callback. An `extern fn` call is one
instruction into foreign code with no Khora frame inside it.

**The cheap fix does not work**, which is worth knowing before somebody reaches
for it. "A callback must have an empty `with` row" sounds sufficient: a
function that declares no capability should not be able to reach a socket. It
is not sufficient — `std::net::socket`'s `receive` and `connect_to` take raw
handles, declare nothing, and suspend. Suspension is not in the effect row, so
no signature can be read to mean "this cannot suspend".

So the mechanism has to be dynamic: a per-thread depth raised around a foreign
call by the code generator and checked by the scheduler before it parks. Two
counter updates per foreign call, which is nothing beside a foreign call. It is
**not built**, because a guard against something no program can express is
machinery no test can exercise — the requirement is recorded in `ffi.md` beside
the feature that would make it reachable, so that feature cannot land without
it.

**33 `unsafe` blocks had no note; 28 remain**, most of them test helpers
calling the C API. The five in the code generator are now annotated, and they
are a *different kind*: `build_in_bounds_gep` is unsafe because an
out-of-bounds `inbounds` GEP is undefined behaviour **in the program being
generated**. Nothing a Rust reader sees locally discharges it — what does is a
`check_index` or a `clamp` emitted a few lines above. Delete the bounds check
and the compiler still builds, still passes its Rust tests, and starts emitting
programs that read off the end of a string.

**The scheduler has not been sanitised**, and cannot be until the stack switch
is annotated — see the section above. That is the largest remaining gap in this
document, and it is a tooling problem rather than an unread piece of code.

**Nothing here checked the scheduler's work-stealing** beyond the invariants
its own soak asserts. 13.2 is where that belongs.
