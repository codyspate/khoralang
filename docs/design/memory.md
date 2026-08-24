# Reference counting, closures, and cycles

**Status: partly decided.** Records what is already true and load-bearing,
decides what can be decided now, and names what cannot.

Written after an outside design review whose central point was that the
unresolved interactions — closures, handlers, cancellation, reference counting,
threads and foreign code — matter more than any syntax question. That is
correct, and this document exists because two of those interactions had already
been settled *by implementation* rather than by decision.

---

## 1. The invariant that used to hold

**Every heap object can only reference objects created strictly before it.**

**This section is history.** It held when this document was written, it is the
reason sections 2 and 3 are shaped as they are, and *it is no longer true* —
mutable fields landed in phase 6.1 and section 4 records what that cost. It is
kept rather than deleted because the four facts below still describe everything
except a `mut` field, and because a reader who quotes the guarantee at the end
of this section should meet the retraction in the same breath.

Four facts produced it, none of them coincidental:

- **ADTs are built bottom-up.** A constructor's arguments are evaluated before
  the object is allocated, so a node can only point at nodes that already exist.
- **Closures capture by value at creation.** A capture is read out of its slot
  and stored into the closure object at the moment the closure is built.
- **Assignment rebinds a name, it does not mutate an object.** `a = b` changes
  what `a` refers to; it does not reach into an object and change a field.
- **A `let` initializer cannot see its own binding.** `let x = f(x)` does not
  resolve the inner `x` to the one being declared.

The heap reference graph was therefore a **DAG**, and a cycle could not be
constructed. Which meant:

> **Perceus reference counting was complete.** No object could leak, because the
> only way reference counting leaks is a cycle, and there was no way to build
> one.

That was a real guarantee and it was worth stating plainly, because it is easy
to assume reference counting always leaks something and to stop looking.

It was also **temporary**, and it ended exactly where this predicted — see
section 4. Today this compiles, and leaves four objects alive:

```khora
export type Node = { name: String, mut next: Option<Node> };

let a: Node = { name: "a", next: Option::None };
let b: Node = { name: "b", next: Option::None };
a.next = Option::Some(b);
b.next = Option::Some(a);
```

## 2. What would break it

Exactly two things, and they are the same thing wearing different hats — both
make an older object point at a newer one.

**Mutable fields.** `node.next = node` is a cycle in one line. When this was
written nothing prevented it except that records did not exist, and
`check_assignable` in `khora-hir` already accepted `Expr::Field` as an
assignment target — the door was propped open. Both landed in phase 6.1, and
the door is through.

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

**Decided in phase 6, and the DAG is gone.** A `mut` field is shared by
reference, so `a.next = b; b.next = a` is a cycle and Perceus stops being
complete. Of the options above, a tracing collector was ruled out by
non-negotiable 5 before this was ever a live question, so what remains is what
was predicted:

> **A cycle leaks, and a weak reference is what breaks one.**

The leak is bounded and quiet rather than unsound — nothing is freed early,
nothing is read after free, the memory is simply never returned. That is the
right failure to have: a program that leaks is wrong in a way you can measure
with `khora_live_count`, and every leak test in this repository is already
watching for it.

Weak references do not exist yet. They are wanted the first time somebody
writes a parent pointer, and that is the moment to design them — the shape of
the problem will be in front of us, which is more than can be said now.

What is worth noticing is how little the loss costs in practice. The three
things that made the graph a DAG — bottom-up construction, capture by value,
and a `let` that cannot see itself — are all still true. A cycle now requires
*deliberately* writing one object into another that already reaches it. It is
no longer impossible; it is still not accidental.

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

## 5a. What may cross a fiber

Decided with mutable fields, because the two cannot ship apart.

Reference counts are atomic (D10), so *sharing an immutable value* across
fibers is safe today, and nothing is mutable — a data race is currently not
expressible in Khora. Mutable fields end that unless something says otherwise.

> **A mutable value cannot be captured by a spawned fiber.** Structural and
> transitive: a record with a mutable field is not shareable, nor is anything
> holding one.

The reason Khora can afford this where Rust needs `Send`, `Sync`, lifetimes and
a borrow checker is that **there is exactly one place a value crosses a fiber
boundary** — the captures of the closure handed to `spawn`. Nothing else
escapes anywhere, because there are no references. So the rule is one property,
checked in one place, over a list the checker already computes and publishes.

What crosses instead is `Shared<A>`, which exists now — `docs/design/shared.md`
decided its API and `std::core` exports it. Compare what it replaces:
`Arc<Mutex<HashMap<K, V>>>` becomes `Shared<Map<K, V>>`, because reference
counting is implicit and there are no lifetimes to name.

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
