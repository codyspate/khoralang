# Shared mutable state

**Decided and implemented.** `Shared<A>` is a cell of shareable values, not a
lock over a mutable record.

## What it is for

`docs/design/sharing.md` refuses three things on purpose: a mutable record
crossing into a fiber, a handler capturing one, and a `Map` doing either. All
three are the same request — shared mutable state — and this is the answer.

The one that mattered most is the test double:

```khora
let calls = { mut n: 0 };
handler for Ledger { balance: fn id => { calls.n = calls.n + 1; id * 10 } }
//                                       ^ refused: a handler may not capture something writable
```

## The problem, which is not the lock

The lock is easy. **Escape** is the problem, and Khora has no lifetimes.

The obvious API is a scoped borrow, matching `String::with_data` and `acquire`:

```khora
Shared::with(cell, fn v => { v.n = v.n + 1 })
```

Nothing stops `fn v => v` returning the inner value, or `fn v => { holder.slot = v }`
stashing it in a captured binding. Either one leaves an unshareable value loose
outside the critical section, where two fibers can reach it. Rust stops this
with `&'a mut`; matching that would mean a borrow checker, which is a much
bigger language than this one is trying to be.

## What was rejected

**The scoped borrow, made sound.** It can be: certify the callback the way
`SharedFn::of` does so its captures must be shareable, require its result to be
`Share`, and note that Khora has no globals to stash into because a module-level
`let` is a constant. Four steps, and each one holds. It would also let
`Shared<Map<K, V>>` work today, with the mutating `Map`.

Rejected anyway, because it is a **four-step argument** and the cell's is one
step: nothing unshareable goes in, nothing unshareable comes out, so there is
nothing to leak. The first version of the sharing rules was a similar argument,
and review found three holes in it in a day — an opaque type answering yes by
default, a generic function laundering anything, a pre-bound closure dodging
certification. A rule that has to be defended cleverly is one that will be got
wrong again, and a rule that is sound in the doc and leaky in practice is worse
than not having the feature.

## What is decided

```khora
export type Shared<A>;
impl<A> Share for Shared<A> {}

impl<A: Share> Shared<A> {
  fn of(value: A) -> Shared<A>;
  fn get(self) -> A;
  fn set(self, value: A) -> ();
  fn update(self, change: (A) -> A) -> A;
}
```

`A: Share` is what makes the escape question not arise. Mutation becomes
replacement, serialized by the lock. This is Effect-TS's `Ref`, Clojure's atom,
Haskell's `IORef` — the functional half of the thesis would expect it anyway.

The test double becomes:

```khora
let calls = Shared::of(0);
handler for Ledger { balance: fn id => { Shared::update(calls, fn n => n + 1); id * 10 } }
```

The handler captures a `Shared<Int>`, which is shareable, so it passes the
certification at the `handler for` literal that every handler goes through.

### `change` cannot fail, and that is the point

`update` runs the change function **once, under the lock**. It has no error row,
and that is what turns lock safety from a discipline into a fact: a function
with no error row has no channel to be interrupted on — the same reason
`khora_fiber_spawn` takes a null trampoline for one — so nothing can leave the
critical section except by returning, and there is no path on which the lock is
still held. Work that can fail belongs outside: compute it, then `set` the
answer.

`update` gives back what it left in the cell, so a caller can see what it did
without a second read that another fiber could get between.

### Re-entering traps

The lock is held for the whole of the change function, so a `get`, `set` or
`update` on the same cell from inside one is a deadlock. Every operation checks
and stops the program with a message rather than waiting for itself, on the same
reasoning as trapping when a program runs off its own array. The check reads a
fiber id that lives *outside* the mutex, because a check that had to take the
lock would be the very thing it is reporting.

### What crosses the boundary

The value, as the one word every Khora value fits in. The runtime cannot know
`A`, so how to release it — and whether the word is even a pointer — is recorded
once when the cell is opened rather than passed to every operation. The change
function's own parameter and result are `A`, which has no single machine type,
so the shim converting them is emitted per instantiation in the backend
(`Backend::change_shim`). Only scalars and pointers cross, as everywhere else.

## `Dict<K, V>`, because a cell needs something to hold

`Map` mutates its buckets in place — which is fast, and is exactly why it is not
`Share` and cannot go in a cell. So a shared table needed a map that is never
written.

`Dict` is an ordered persistent map: a weight-balanced tree built out of
ordinary variants, holding nothing writable, shareable in the plain structural
way. An insert gives back a new map and the old one is still there, the two
sharing everything the insert did not touch — one path from the root is rebuilt
and the rest is the same objects with one more reference each. That is what
makes read-modify-write of a whole table affordable.

Weight-balanced with a slack of three, so the depth is logarithmic and the
recursion is bounded by it rather than by the number of entries — the
distinction `String::slice` got wrong and paid for.

The cost is real: a `Shared<Dict>` copies a path per update where a locked
`Map` would write one bucket. Phase 9's reuse analysis is the answer, and this
is close to its ideal case — when the cell holds the only reference, the
rebuilt path is an in-place write.

## What is still open

- **A move-in spawn.** Captures are copied and both fibers keep theirs, so the
  sharing rule is `Sync`, not `Send`. A consuming spawn could transfer an
  otherwise mutable value safely, and would take pressure off this.
- **A `Shared` that holds something big.** Every update copies a path. Fine for
  a counter, fine for a tree; not for an array somebody wanted to fill.
- **`Map` and `Dict` are two maps**, and a reader has to know which. That is
  honest today — one is fast and fiber-local, the other is shareable — but it
  is two names for one idea and worth revisiting once reuse analysis makes the
  persistent one cheap enough to be the default.
