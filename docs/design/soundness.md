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

| | | |
| --- | --- | --- |
| `unsafe` blocks | **282** | 179 at the first audit |
| — with an argument | **282** | 138 at the first audit, 241 before this pass |
| C symbols exported to generated code | 100 | 61 `unsafe fn`, 39 safe |
| `unsafe impl Send` | 3 | all in `khora-rt` |
| Thread-locals | 9 | 6 at the first audit |
| Places a fiber suspends from Rust | 4 | |

The block count grew by a hundred and the *annotated* count by rather more,
which is the useful half: the first audit left 41 blocks with nothing saying
why they were sound and nothing was watching the number. It is now a gate
step — see below — so the second column of this table cannot drift again.

Everything outside `khora-rt` and `khora-codegen-llvm` is safe Rust; the two
`unsafe` mentions elsewhere are in comments.

## Finding 1 — a `main` program that publishes a symbol counted without atomics

**Reachable, and memory corruption when reached.**

Generated code counts references non-atomically when the compiler can prove the
program has one thread. The proof was: this is a `main` build, and no body
mentions `Fiber::spawn`. `khora_fiber_spawn` aborts if that proof turns out to
be wrong, so a spawn cannot sneak past it.

A second thread can get in without a spawn. `emit_c_exports` runs for **every**
entry point, so a `main` program containing an `pub extern fn` publishes
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

## Every block has an argument, and a script says so

`scripts/no-bare-unsafe.sh`, in `scripts/baseline.sh`.

**The first audit annotated 179 blocks by hand and recorded that 28 had no
note. The number was 41 when it was next counted**, because nothing was
checking and every block written since had started life unannotated. Counting
by hand once produces a number; it does not produce a property.

A block is covered two ways:

- **A `// SAFETY:` note above it, inside the same item.** The window stops at
  the enclosing `fn`, which matters: a fixed window of N lines reaches the note
  on the *previous* function when a block sits near the top of a short one. The
  first version of the script did exactly that, and a bare `unsafe { *p }`
  appended to `heap.rs` came back covered.
- **A blanket note, spelled `SAFETY, for`,** covering every block after it in
  the file. `channel.rs`'s tests open a handle, use it and release it inside one
  function, twenty-three times; the argument is identical every time and writing
  it out twenty-three times is how the load-bearing note stops being read. The
  distinct wording is deliberate — a reader typing `SAFETY, for` is making a
  claim about a *run* of blocks and should know it.

Two blanket notes exist, both over test code. Everything in library code is
annotated individually.

**What this does not check** is whether an argument is *true*. It checks that
one was written, which is the difference between a reviewer being able to
disagree with it and there being nothing to disagree with.

## Thread-locals under fiber migration

**The rule**: no thread-local address may survive a suspension. A fiber that
parks may be resumed on another worker, and a reference into this thread's copy
of a `thread_local!` then points at the wrong thread's data.

Two mechanisms, and both were checked rather than assumed.

**`LocalKey::with` hands out a reference that dies with its closure**, so the
question reduces to whether any `.with(..)` closure contains a suspension.
Forty-six closures in `khora-rt`; none does. A reference cannot outlive a
closure that never yields.

**The one thread-local read by address is `CURRENT`**, and it is protected by
`#[inline(never)]` on the four functions that touch it. Not inlining moves the
address computation into the callee, where it runs on the thread actually
executing; the stack switch's inline assembly clobbers memory, so the *value*
cannot be carried across a suspension either. `current.rs` has the argument in
full.

That one is load-bearing and deleting the attribute compiles, passes clippy,
and reintroduces a fiber reading another fiber's cancellation flag — which is
how it was found. `a_fiber_keeps_its_identity_across_workers` is the test, and
it failed `left: 30, right: 28` as soon as migration became common.

## `unsafe impl Send`

Three, all in `khora-rt`, and each carries its argument at the impl.

- **`Task`** — a fiber's stack moving between workers. The argument has three
  legs: reference counts are atomic whenever a program can spawn, so moving a
  stack cannot race a count; what may cross *into* a fiber is `Share`, checked
  by the type checker; and foreign code is excluded by policy rather than by
  types, because `scheduler.md` §8 forbids suspending inside an `extern` call.
  The residual obligation is on Rust bodies in this crate: anything held across
  a `suspend()` must be `Send`. Captures are already checked — `Task::new`
  requires a `Send` closure — but a local created inside the body and held
  across a suspension is not.
- **`Migrating`** — the same argument, inside the test that exercises it.
- **`Handed`** — a Khora pointer being moved to another fiber. Sound because
  reference counts are atomic and a spawned closure is *handed over* rather
  than shared: the caller gives up its reference at the `spawn`.

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

`sh scripts/tsan.sh`. Thirty-eight tests across six modules, **no warnings**.

| Module | What it covers |
| --- | --- |
| `channel` | A bounded queue with senders and receivers on real threads — the newest primitive here, and the one carrying values between fibers |
| `wait` | The park/wake protocol, whose entire content is the race between a suspension and the wake that beats it |
| `contain` | Thread-locals and the trap flag |
| `region` | Finalizers, which two fibers may defer to at once, and the cancellation shield 13.3 added beside them |
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

**Read the report, not the terminal.** `${TMPDIR:-/tmp}/khora-tsan-report.txt`
is what `tee` writes and what the sentinel is grepped out of; a run piped
somewhere else can arrive with most of it missing, because the inner shell's
stdout is block-buffered through WSL while cargo's stderr is not. The file on
disk has every filter's block in order. The verdict is unaffected either way —
it is read from the file.

## Finding 4 — `attempt` turned a cancellation into a typed null

Found after this audit closed, on the way to 13.3, and recorded here because it
is the same kind of thing and this is where the kind is kept.

`effect-runtime.md` §6 promises that **nothing a program writes can swallow a
cancellation**: it travels the tagged return under a `which` no error type can
be assigned, and a `catch` dispatches on error type ids, so it matches no case.
`lower_catch` keeps the promise deliberately — under a `_` arm it routes
`CANCELLED_WHICH` and `FAILED_WHICH` back to the propagate path by name.

`attempt` is the *other* total handler, and it did not. It branched on "the tag
is not zero" and packed whatever it found into `Err`. A cancelled computation
came back as an ordinary failure, so a retry policy would retry a fiber that
had been asked to stop — and, worse, a cancellation carries no payload, so the
`Err` held **a null typed as the body's error**. One `problem.show()` from a
read through it.

Fixed in `lower/failure.rs` by giving `attempt` the routing `catch` already
had. Two tests in `tests/fibers.rs`: a cancellation passes through, and a real
failure is still a value.

**How it survived the audit.** The audit read the runtime, and this is in the
code generator; the promise it breaks is in a design document rather than in a
`# Safety` comment. What found it was writing a program that took the promise
literally — a rollback doing fallible work inside a finalizer — which is the
argument for the reference applications rather than for more reading.

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

**A finalizer that hangs cannot be interrupted.** 13.3 made finalizers
uncancellable — see `cancel::Shielded` — because cleanup caused by a
cancellation must not be cut short by that same cancellation. Everything with
cancellation pays this price, and the usual answer is a deadline on the cleanup
itself, which Khora does not have. Nothing is unsound; a program can hang where
it would previously have corrupted a transaction.
