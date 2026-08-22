# Reference counting, closures, and cycles

**Status: partly decided.** Records what is already true and load-bearing,
decides what can be decided now, and names what cannot.

Written after an outside design review whose central point was that the
unresolved interactions — closures, handlers, cancellation, reference counting,
threads and foreign code — matter more than any syntax question. That is
correct, and this document exists because two of those interactions had already
been settled *by implementation* rather than by decision.

---

## 1. The invariant that currently holds

**Every heap object can only reference objects created strictly before it.**

Four facts produce this, none of them coincidental:

- **ADTs are built bottom-up.** A constructor's arguments are evaluated before
  the object is allocated, so a node can only point at nodes that already exist.
- **Closures capture by value at creation.** A capture is read out of its slot
  and stored into the closure object at the moment the closure is built.
- **Assignment rebinds a name, it does not mutate an object.** `a = b` changes
  what `a` refers to; it does not reach into an object and change a field.
- **A `let` initializer cannot see its own binding.** `let x = f(x)` does not
  resolve the inner `x` to the one being declared.

The heap reference graph is therefore a **DAG**, and a cycle cannot be
constructed. Which means:

> **Perceus reference counting is currently complete.** No object can leak,
> because the only way reference counting leaks is a cycle, and there is no way
> to build one.

That is a real guarantee and it is worth stating plainly, because it is easy to
assume reference counting always leaks something and to stop looking.

It is also **temporary**, and the way it ends is specific.

## 2. What would break it

Exactly two things, and they are the same thing wearing different hats — both
make an older object point at a newer one.

**Mutable fields.** `node.next = node` is a cycle in one line. Nothing prevents
this today except that records do not exist; note that `check_assignable` in
`khora-hir` already accepts `Expr::Field` as an assignment target, so the door
is propped open for whenever they land.

**Recursive closures.** `let go = fn n => .. go(n - 1) ..` captures `go`, which
is the closure being built, so the closure holds a counted reference to itself.
That is a cycle on the first program anybody writes, and it leaks. Today it is
rejected — "cannot find `go` in this scope" — for the unrelated reason that a
`let` initializer cannot see its own binding.

An outside review listed these as two separate open questions. They are one.

## 3. Recursive closures without cycles — implemented

A self-reference does not need to be a counted reference, because it does not
need to be a *reference* at all.

A lifted lambda already takes its own closure object as its first argument —
that is how it reaches its captures. So a closure calling itself can call
through the parameter it was handed, with no capture, no refcount traffic and no
cycle:

```khora
let go = fn n => if n == 0 { 0 } else { go(n - 1) };
```

`go` inside the body resolves to "the closure this invocation was called
through", which is guaranteed live for the duration of the call because the
caller holds a reference to it. Zero cost, and the DAG invariant survives.

This covers direct self-recursion, which is what people write. **Mutual
recursion between two closures is not covered** and genuinely needs a cycle;
that is a case for named functions, which have no closure object and therefore
no counting at all.

Implemented. A `let` whose initializer is a lambda binds that name inside the
body as `Expr::LambdaSelf`, which code generation resolves to parameter 0. The
name reaches only the *innermost* lambda: an inner closure naming an outer one
would capture it, and a closure holding a closure that holds it is the cycle
this design avoids, so it is rejected with a message pointing at named
functions.

One detail is load-bearing. A call through `LambdaSelf` must **not** release
its callee. Every other closure call does — reading a local duplicates the
reference, so the call site owns one — but a closure's own name is the argument
it was handed, which it borrows. Releasing it decrements a count the frame
never took and frees the closure out from under the caller still running in
it.

## 4. Cycles in general — the shape of the answer

Once mutable fields exist, cycles are constructible and something must be said.
Non-negotiable 5 in `docs/vision.md` rules out the usual escape: a cycle
collector is a tracing collector, and Khora does not have one. Ever.

That leaves the answer Swift and Rust both give, which is also the one this
audience already knows: **a cycle leaks, and a weak reference is how you break
it.** A Swift developer reads this immediately; a Rust developer knows
`Rc`/`Weak`; a TypeScript or Go developer has never had to think about it and
will need the diagnostic to be good.

Not decided here, because it should be decided alongside records and mutable
fields rather than in the abstract. Logged as **D11**.

## 5. Reference counts are atomic

`khora-rt` increments and decrements `KhoraHeader.refcount` atomically. This
section used to argue the opposite at length, and to log the question as
**D10**; both are settled now, and the argument is worth keeping only in
summary.

The section claimed non-atomic counts *constrain code being written now*, on
the grounds that every `dup` and `drop` already emitted assumes the
single-threaded answer. That was wrong. Code generation never touches a
refcount directly — every `dup` and `drop` is a call into `khora-rt` — so
atomicity was a change inside the runtime, invisible to everything already
emitted, and it was made in phase 5 in about thirty lines.

What settled it was not performance. A5 promises fibers running across cores,
and a spawned fiber shares at least the closure it was handed, so a non-atomic
count is a data race in the first concurrent program anyone writes. And the
`Rc`-versus-`Arc` escape hatch is *colouring* — the thing Khora's rows exist to
avoid — paid in every library signature to save an increment.

`docs/design/effect-runtime.md` §9 has the decision in full, including where
the cost comes back: phase 9, as an optimization for objects that provably do
not escape their fiber, chosen by the compiler and invisible in every type.

## 6. Closures and handlers — open

A closure captured across a handler boundary, or a continuation captured inside
a closure, is unspecified. It cannot be specified before **D1** settles whether
continuations are one-shot or multi-shot, because that decides whether a
captured environment has to be copyable.

Recorded so it is not mistaken for an oversight: the closure implementation is
currently compatible with either answer, but that is luck rather than design,
and it should be re-checked when D1 lands rather than assumed.

## 7. What is settled about closures today

For reference, since the rest of this document is about what is not:

| Question | Answer |
| --- | --- |
| Capture mode | By value, at the moment the closure is built. |
| Ownership of captures | The closure object owns a reference to each; its drop glue releases them. |
| Assigning to a capture | Rejected. It would change the closure's copy and nothing else, silently. |
| Representation | A heap object under the ordinary header: field 0 is the code pointer, captures follow. |
| Nested capture | An inner lambda's free variables are captures of the outer one too, so the chain is complete. |
| A named function as a value | A closure that captures nothing, forwarding through a one-line adapter. |
| Calling itself | Through parameter 0, borrowed. Not a capture, not counted, no cycle. Direct self-recursion only. |
| Cost of calling one | One indirect call. No dictionary, no allocation per call. |
