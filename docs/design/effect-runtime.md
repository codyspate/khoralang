# D1 — How handlers execute

**Status: decided.** This is the answer the roadmap called its largest unknown,
and the one an outside review independently picked out as the thing everything
else waits on.

`docs/design/effects.md` decided what effects *look like*. This decides what
they *do* at runtime: what a capability costs at a call site, what happens to a
stack when something raises, and what Perceus has to know about either.

---

## 1. The syntax already answered most of it

The question was framed as "one-shot versus multi-shot continuations". Reading
the decided syntax back, that framing does not fit what Khora actually has.

There is no `resume`. An operation is written as an ordinary function from its
arguments to its result:

```khora
let live_ledger = handler for Ledger {
  get_history: fn id => Db::query(pool, id) |> List::map(Txn::of_row),
  flag_account: fn (id, risk) => Db::exec(pool, Sql::flag(id, risk))!,
};
```

`get_history` returns a value, and that value *is* the result of the operation
at the point it was performed. In the literature this is a **tail-resumptive**
handler, and it is the shape every example in `effects.md` takes. A handler
that wanted to resume twice, or later, or not at all, would need a way to name
the continuation. Nothing names it.

So the real question is not which flavour of continuation to capture. It is
whether Khora should *grow* one. It should not, yet — §4 — and without one, the
implementation is far cheaper than the framing suggested.

## 2. Three mechanisms, not one

Effects in Khora are three different things that the syntax deliberately keeps
apart, and they get three different implementations.

| Concern | Written | Control | Mechanism |
| --- | --- | --- | --- |
| Capabilities | `with { ledger: Ledger }` | returns | evidence passed as parameters |
| Failures | `raises DbError`, `raise`, `!` | does not return | tagged return, checked at `!` |
| Suspension | fibers, phase 5 | resumes later | stack segments |

Collapsing them into one continuation-capturing runtime is what would have made
this expensive. Keeping them apart is what makes each one ordinary.

### Capabilities are implicit parameters

A capability row is static. The type system knows exactly which capabilities a
function requires, because it is written in the signature and checked at every
call. So the handler record does not need to be *found* at runtime — it can be
handed in.

```khora
pub fn analyze(id: String) -> Report with { ledger: Ledger }
```

compiles to a function taking one extra argument: the `Ledger` handler, which
is a record of closures and therefore already an ordinary heap value. A call
site supplies it from its own evidence parameter, or from the enclosing `with`
block. There is no handler stack, no dynamic lookup, no walking of frames.

Performing an operation is then a field read and a closure call — the same cost
as calling any function value, which closures already pay for and which
`docs/design/memory.md` already accounts for.

Row polymorphism (`'ef`) specializes the same way generics do. Khora already
monomorphizes whole-program, and a row variable is concrete at every call site
reachable from `main` for exactly the reason a type variable is. No new
mechanism, and the same code-size trade already accepted for generics.

### Failures are tagged returns, checked where `!` says

A function with a non-empty `raises` row returns a tagged value: the result, or
the error. At each `!` the compiler emits the branch — on the error path, run
this frame's pending drops and return the error upward.

That is the machinery code generation already has. `unwind_to` releases a
scope's live bindings on an early `return`; a raise is the same thing crossing a
function boundary, and `finish` already knows how to leave.

Two things fall out of this that are worth stating.

**`!` earns its keep twice.** `effects.md` justified the mark on readability:
this audience has been taught by `?` and `try` to expect a mark where control
can leave. It is also, exactly, where the branch is. The syntax and the
implementation want the same annotation, which is usually a sign the annotation
is real.

**No unwinder.** No DWARF tables, no landing pads, no personality routine, no
`longjmp`. A raise is a return with a tag, and every frame it passes through
runs the drops it was going to run anyway. That keeps the story portable and
keeps foreign frames out of it (§7).

**The tag is the error's type, not a bit.** `{ i32 which, i64 payload }`:
`which` is 0 when the call returned normally, and otherwise a program-wide id
for the error's *type*. The payload is one word because every Khora value is
word-sized. Two registers, the same as a bare bit would have cost.

The id has to be there because `catch` handles *part* of a row. A function
raising `DbError + ModelError` whose caller catches only `ModelError` needs to
know at runtime which of the two arrived, and the heap object cannot say: a
`tag` in the header is a variant index within one type, so `DbError::Timeout`
and `ModelError::RateLimited` are both tag 0.

Two alternatives were rejected. Indexing into the callee's row is smaller but
does not survive an open row `'er`, where the index is not known at the raise
and would have to be renumbered at every frame the error crosses. Stealing
high bits of the header `tag` costs nothing at the call but makes every
ordinary `match` mask, taxing code that never raises. A whole-program compiler
already knows every error type, so a program-wide id is free to assign and
never needs remapping — an error crossing a frame carries the same `which` it
was raised with, whatever the rows in between look like.

### Suspension belongs to fibers

Async I/O and generators need a computation to stop and continue later. That is
a *fiber* — a whole stack that suspends — and it is phase 5's problem, not a
handler's. Effect (TypeScript) draws the same line: services and dependency
injection are one thing, and the fiber runtime that suspends them is another.

This matters for scope. Handlers need no stack machinery at all, so phase 4
does not block on any of it.

## 3. Why not first-class continuations

The decisive argument is reference counting, and it is worth spelling out
because it is the one that cannot be engineered around.

Capturing a continuation means capturing the frames between the operation and
its handler. Those frames hold references to heap objects.

- **Multi-shot** means the captured frames may run more than once, so capture
  must *copy* them, and every reference in every copied frame needs a `dup`.
  The runtime therefore has to know, for each program point, which stack slots
  hold counted pointers. That is a stack map — precise-GC machinery, arriving
  through the back door of a language whose fifth non-negotiable is that it
  does not have a garbage collector.
- **One-shot** means the frames run at most once, so capture *moves* them.
  Ownership transfers wholesale and no count changes at all. No stack maps.

So one-shot is not merely cheaper than multi-shot; it is the difference between
needing stack maps and not. That is the line to hold.

And Khora does not need even one-shot capture yet, because §1: nothing in the
syntax can name a continuation. Tail-resumptive handlers plus abortive raises
cover state, readers, dependency injection, logging, errors and resource
scoping — which is the whole of what `std::core` and the reference application
ask for.

## 4. What is given up, and how it comes back

Given up: a handler that resumes somewhere other than tail position, resumes
more than once, or stores its continuation. Concretely — backtracking search,
probabilistic programming, and writing a scheduler *as a handler* rather than as
a fiber runtime.

None of those are in the vocabulary of the audience in `docs/vision.md`, and the
last one has a perfectly good alternative arriving in phase 5 regardless.

The route back, if it is ever wanted, is an extension rather than a break:

- Adding an operation form that names its continuation is **widening**. Every
  program written against tail-resumptive handlers stays valid, because a
  handler that returns a value is a handler that resumes in tail position.
- It would be **one-shot**, for the reason in §3, and would need stack segments
  — which fibers bring anyway.

Going the other way — shipping multi-shot and later restricting it — would
break programs. The order is not symmetric, so start narrow.

## 5. What Perceus has to know

Nothing new.

| Path | Ownership |
| --- | --- |
| Performing an operation | A closure call. The handler record is borrowed from the evidence parameter; arguments are passed owned, as to any call. |
| A handler returning normally | An ordinary return. |
| `raise` | The raising frame owns the error value and moves it into the tagged return. Each frame the error passes through runs its own drops and moves the error on. |
| Installing a `with` block | The handler values are owned by the enclosing scope and released when it ends, like any other binding. |

The point of the table is that every row is a mechanism that already exists and
is already tested. No new reference-counting rule is introduced by effects,
which is precisely what makes this design worth choosing over one that captures
continuations.

## 6. Cancellation points

A5 promises interruption that runs finalizers. With failures implemented as
tagged returns, cancellation is a return the runtime injects: a cancelled
fiber's next cancellation point returns the cancellation instead of carrying
on, and every frame between there and the fiber's root runs its drops on the
way out.

That gives a property worth promising out loud: **a computation is only
interrupted at a cancellation point** — never between two statements that are
not one. The points are few enough to list, and the list below is all of them.

### What it is, precisely

**A cancellation travels on a tagged return under a `which` no error type can
be assigned.** Error-type ids start at 1 and count up; a cancellation is
`u32::MAX`. Three things follow, and all three are the behavior wanted rather
than a consequence to work around:

- **`catch` cannot swallow it.** A `catch` dispatches on the error type id and
  routes a cancellation to the propagate path by an explicit case, `_` arm
  included. Nothing a program can write names it, because it is not an error
  the program declared.
- **It is not in any row.** No signature mentions it, no `raises` clause grows
  because of it, and the type system is untouched. Cancellation is the runtime
  asking a computation to stop, not a failure it can have. `!` marks only a
  row error; a cancellation leaves a function unmarked, as a panic does in
  Rust or cancellation does in Go, Trio or Kotlin.
- **The unwinding is the unwinding that already exists.** Every frame between
  the point and the root runs its drops on the way out, which is how a
  region's finalizers run — see §10.

### Every function that can reach a point carries the tag

A fallible function already returns `{ which, payload }`. **An infallible one
that can reach a cancellation point returns `{ which, answer }`**, the same
shape with its own answer type in the second half, and its caller branches on
`which` after the call exactly as it does after a `!`. So a cancellation has a
way out of every such frame, whatever its `raises` row.

Which functions those are is decided once, for the whole program, by
`crates/khora-codegen-llvm/src/backend/can_stop.rs`, before anything is
declared — the answer is each function's machine type, and a caller and callee
that disagree about it is a miscompile. The analysis errs toward tagging: an
unknown callee shape counts as a call through a function value, and an
intrinsic counts unless it is on an explicit stop-free list. A function it
**prunes** reaches no cancellation point, so no cancellation can arise inside
it and it keeps a plain return. The lowering refuses (a compiler panic in
`leave_with`) to emit a way out of a pruned frame, because there is nothing
correct to emit; a `catch` in such a frame seals its fall-through instead.

The points are:

- a **`!`**, checked before the call so a computation already asked to stop
  does not evaluate arguments it is about to throw away;
- a **loop back-edge**, behind one relaxed load of the poll word
  (`crates/khora-rt/src/poll.rs`);
- the **entry of a function in a call cycle** (including one through a
  function value), because recursion is a loop with no back-edge;
- a **call to a tagged function**, which is where a cancellation observed
  further down arrives;
- a **blocking operation** — channel send/receive (only when it comes back
  empty-handed, so a value is never dropped), `Fiber::wait`/`join`/`outcome`,
  `clock.sleep`, socket accept/read/write. Each gives up when its fiber is
  cancelled and the call site checks.

Not points, stated as limits: one foreign call or file-system syscall already
in progress; `connect_to` and waiting for a child process (the fiber stops
after they return); `Shared::get`/`set` waiting on a cell's lock.

### Cleanup

Finalizers run **shielded**: a cancellation arriving while one runs is
remembered, not observed, so a `ROLLBACK` can do I/O. `cancel` is idempotent
— the runtime itself cancels the same fiber more than once — so cancelling
again changes nothing. `Fiber::abort` is the separate, explicit operation that
cuts through the shield (and propagates to nursery children), and
`Fiber::cancel_within(h, millis)` asks for it after a caller-chosen deadline.
There is no built-in deadline. A `Shared::update`/`modify` change function is
**pinned**: nothing in it stops, blocking calls in it give up, and the cell
is left unchanged if it leaves on a tag (`crates/khora-rt/src/cancel.rs`,
`Pinned`).

### At the entry point

**130, not 1.** A `main` that leaves on a cancellation — tagged or fallible —
closes the root region and exits 130, which is 128 + SIGINT and what a shell
already means by interrupted. A program that raised and did not handle it
exits 1. A cancellation reaching a *spawned* fiber's root stops that fiber
only; `join` on it unwinds the joiner, `wait` returns, `outcome` reports
`Stopped`.

### What it costs

The branch after every tagged call and the widened return of every tagged
infallible function, plus the back-edge load. Measured at the end of the
redesign (release, Linux x86-64): a tight loop about 1.2×, recursion about
1.6×, a very short loop called in a hot path up to about 3.8×; ordinary
iteration unchanged. Windows is unmeasured.

## 7. Foreign code

A raise crossing a foreign frame is not supported, and cannot be: a tagged
return is a calling convention, and Rust and C frames do not participate in it.

The boundary rule is therefore simple and checkable: **a Khora function passed
to foreign code as a callback must have an empty `raises` row.** The type system
already tracks the row, so this is a diagnostic rather than undefined behavior
— which is more than a `longjmp`-based design could offer, and is a second
reason to prefer tagged returns.

D8 owns the rest of the interop boundary.

## 8. `raises` and `with` are one mechanism, two behaviors

`effects.md` left open whether errors are literally an effect in the same row.
They are the same *resolution* mechanism — both rows, both static, both settled
at compile time — and different *control*: a capability is called and returns, a
failure leaves and does not.

Keeping them separate in the syntax was right, and this is why: they compile to
different things. A row that mixed them would have to ask, per label, which one
it was.

## 9. D10 — reference counts are atomic

`docs/design/memory.md` §5 said non-atomic reference counts constrain code being
written now. That was not accurate: code generation never touches a refcount
directly — every `dup` and `drop` is a call to the runtime — so atomicity is a
change *inside* `khora-rt`, invisible to everything already emitted. That is
what made this a decision that could wait, and it is what made it cheap to make.

**Decided: atomic, and there is no way to opt out.**

The forcing argument is smaller than the performance one. A5 promises fibers
running across cores, and a spawned fiber shares at least the closure it was
handed — so a non-atomic count is a data race in the first concurrent program
anyone writes, not an edge case to warn about. Correct by default is the only
starting point that does not require every user to know this.

`khora_dup` is a relaxed `fetch_add`: the caller already owns a reference, so
nothing can be freed underneath it and nothing is being published. `khora_drop`
is a `fetch_sub` with release, and the thread that takes the count to zero
issues an acquire fence before touching the fields. That is the standard pair,
and it is the whole of the change.

### Why no `Rc` versus `Arc`

Two reasons, and the second is the one that settles it.

The performance case for a split is real but narrow: an uncontended atomic RMW
is more expensive than an increment, and code that never leaves one fiber pays
for a guarantee it does not use. Swift ships atomic counts for a whole language
and is not thought of as slow, so this is a cost to measure rather than a
reason to fork the type.

The decisive reason is that a split is **colouring**. `Rc<T>` and `Arc<T>` are
different types; the choice propagates into every signature that touches one,
and a library that guessed wrong is a library you cannot use. Khora's whole
argument is that the things which usually colour a codebase — async, failure,
dependency injection — belong in a *row* on the signature, where they compose
and can be abstracted over. Putting thread-sharing in the *representation*
instead would be the one piece of colouring the language has no vocabulary for,
and it would be there to save an increment.

### Where the cost comes back

Phase 9. An object that provably does not escape its fiber can use the
non-atomic operations, chosen by the compiler and invisible in every type. That
is the same whole-program shape as Perceus reuse analysis, which is what phase
6 is for, and it is an optimization rather than a promise — a program is
correct either way.

The cost until then is unmeasured, and saying so is more useful than a number
made up here. `bench` declarations parse but do not run yet; measuring this is
work for the phase that has something to compare against.

`memory.md` is corrected to match.

## 10. A region is an ordinary counted value

Phase 5 promises that a resource acquired in a region is released when the
region ends, *however* it ends. The mechanism is the one §5 already described,
used once more.

**A region is a reference-counted object whose release runs its finalizers.**
That is the whole design. Every path that ends a region is a path that releases
a binding, and code generation already emits all of them: `leave_scope` at the
end of a block, `unwind_to` at an early `return`, and `unwind_to` again when a
raise passes through. No new rule about unwinding, and no second notion of a
scope living beside the one Perceus has.

Two consequences worth stating.

**Finalizers run in reverse.** A finalizer deferred later may depend on one
deferred earlier — a transaction rolled back before the connection it ran on is
closed — so the last acquired is the first released.

**The root region ends after `main` returns**, on the failing path as well as
the ordinary one. A finalizer that runs only when nothing went wrong is not a
finalizer, and an uncaught raise is exactly when closing a file matters.

### The operation is not generic; the function on top of it is

`std::core` used to declare `acquire: forall <A> . (A, A -> ()) -> A` as the
operation of `Scope`. It cannot be one. A handler's fields are ordinary
closures, and a closure is monomorphic — its captures have a machine layout —
so an operation that quantifies over a type has no representation.

It does not need one. The operation is

```
pub effect Scope {
  defer: (() -> ()) -> (),
}
```

and the polymorphism moves to an ordinary generic function:

```
pub fn acquire<A, 'ef>(value: A, release: (A) -> ()) -> A
  with { 'ef | scope: Scope }
{
  scope.defer(fn () => release(value));
  value
}
```

This is the better factoring regardless of what closures can represent. The
effect declares the one thing a handler has to decide — *where finalizers go* —
and everything else is a library function anyone could have written.

### Where the runtime is involved, and why

Two places, both because deferring *grows* a list and nothing in Khora grows a
value in place.

The finalizers live Rust-side, behind a pointer in the region object's single
field, and the region's `drop_fields` callback is the runtime's rather than one
generated from a field layout. Everything else about a region is ordinary: it
is allocated by `khora_alloc`, counted like anything else, and released by the
same `khora_drop` every other object goes through.

And `Region::defer` is a code-generation intrinsic rather than an extern,
because the runtime has to be handed the closure's *drop routine* alongside the
closure. A closure's routine is generated — one shared function switching on
the site tag — so nothing but the code generator knows the pointer, and a Khora
declaration has nowhere to write it.

## 11. What phase 4 built, in order

1. **Rows in the type system** (4.2): `Type::Row`, unification with reordering
   and tail extension, subtraction when a `with` discharges a requirement, and
   the empty-row obligation at the entry point. No runtime work at all.
2. **Evidence as parameters** (4.3a): lowering `with { .. }` to an argument,
   and an operation to a field read plus a closure call. Every piece already
   exists — closures, records, monomorphization.
3. **Tagged returns** (4.3b): the calling convention for a non-empty `raises`
   row, the branch at `!`, and drops on the error path.
4. **`Layer` as handler composition** (4.4): a handler built from other
   handlers is a function returning a record, which needs nothing new.

Nothing in that list needs a stack segment, an unwinder, or a stack map. That
is the point of the decision.
