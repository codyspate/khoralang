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
export fn analyze(id: String) -> Report with { ledger: Ledger }
```

compiles to a function taking one extra argument: the `Ledger` handler, which
is a record of closures and therefore already an ordinary heap value. A call
site supplies it from its own evidence parameter, or from the enclosing `with`
block. There is no handler stack, no dynamic lookup, no walking of frames.

Performing an operation is then a field read and a closure call — the same cost
as calling any function value, which closures already pay for and which
`docs/design/memory.md` already accounts for.

Row polymorphism (`'e`) specializes the same way generics do. Khora already
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
does not survive an open row `'r`, where the index is not known at the raise
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

## 6. Cancellation points are the `!` marks

A5 promises interruption that runs finalizers. With failures implemented as
tagged returns, cancellation is a raise the runtime injects: a cancelled fiber's
next checked call returns the cancellation instead of its result, and every
frame between there and the fiber's root runs its drops on the way out.

That gives a property worth promising out loud: **a computation can only be
interrupted at a point the source marks with `!`.** Nothing is torn down
between two statements that do not mention it. That is a stronger and far more
explainable guarantee than "interruption can happen anywhere", which is what
thread cancellation usually means, and the reader can see the points.

The cost is the flip side: a loop with no `!` in it is not interruptible.
Whether long pure loops need an implicit yield is a phase 5 question, logged
there rather than decided here.

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

## 9. D10, revisited and downgraded

`docs/design/memory.md` §5 says non-atomic reference counts constrain code being
written now. Checking the backend, that is not accurate: code generation never
touches a refcount directly — every `dup` and `drop` is a call to the runtime.
Atomicity is a change *inside* `khora-rt`, invisible to everything that has been
emitted so far.

D10 is therefore a performance and type-system question, not a blocker, and the
recommendation is:

- **Atomic counts once fibers can share values**, because correct-by-default is
  the right starting point and Swift demonstrates that a whole language can
  live with it.
- **Non-atomic where an object provably does not escape its fiber**, as an
  optimization, not as a user-visible distinction. Khora does not get `Rc`
  versus `Arc`; that split is one of the things that makes Rust hard, and
  paying for it in every library signature is worse than paying for an
  uncontended atomic.

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
export effect Scope {
  defer: (() -> ()) -> (),
}
```

and the polymorphism moves to an ordinary generic function:

```
export fn acquire<A, 'e>(value: A, release: (A) -> ()) -> A
  with { 'e | scope: Scope }
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
