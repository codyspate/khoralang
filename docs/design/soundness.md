# The runtime soundness audit

Roadmap 13.6. An inventory of every place the runtime steps outside what the
compiler checks, what each one depends on, and which of those dependencies is
enforced rather than merely believed.

**Three defects found, all fixed. Two of them were reachable.** The third was a
mis-declaration rather than a live bug, and is the kind that makes the next one
invisible.

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

## What is still open

**A fiber must not suspend inside an `extern` call**, or a C library's
thread-affine state is live across a migration. This is `scheduler.md` §8's
policy and nothing enforces it. It is not currently violable from Khora — an
`extern fn` body is foreign code, and Khora cannot suspend from inside one —
but it becomes violable the moment a foreign function takes a Khora callback
that can suspend. Worth a check when that becomes expressible.

**33 `unsafe` blocks had no note; 28 remain**, most of them test helpers
calling the C API. The five in the code generator are now annotated, and they
are a *different kind*: `build_in_bounds_gep` is unsafe because an
out-of-bounds `inbounds` GEP is undefined behaviour **in the program being
generated**. Nothing a Rust reader sees locally discharges it — what does is a
`check_index` or a `clamp` emitted a few lines above. Delete the bounds check
and the compiler still builds, still passes its Rust tests, and starts emitting
programs that read off the end of a string.

**No sanitiser has been run.** `core-dumps-beat-assertions-on-races` records
that instrumenting the runtime hides at least one heisenbug, so TSan under WSL2
is the tool and it has not been pointed at the channel or the pool yet. That is
the largest remaining piece of 13.6 and it is a machine-time job rather than a
reading job.

**Nothing here checked the scheduler's work-stealing** beyond the invariants
its own soak asserts. 13.2 is where that belongs.
