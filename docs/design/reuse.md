# Reuse and FBIP

How phase 9 gets from correct reference counting to minimal reference counting,
and what has to be true before a `map` over a list can allocate nothing.

> **The fusion is the easy half. The analysis is moving a release to the last
> use of a value, on every path, and that is what turns a wrong answer into a
> double free rather than a slow program.**

## Where things stand

`khora-perceus` inserts reference counting that is *correct* and deliberately
not *minimal*. A local owns one reference for its whole scope, reading it
`dup`s, and the block releases what it declared on the way out.

That scheme is why nothing can be reused today, and the reason is worth being
precise about, because it is not "the fusion has not been written yet":

```khora
fn increment(xs: List<Int>) -> List<Int> {
  match xs {
    List::Nil => List::Nil,
    List::Cons(head, tail) => List::Cons(head + 1, increment(tail)),
  }
}
```

At the `List::Cons(..)` in the second arm, the cell that was matched is held
twice — once by the parameter binding `xs`, which is not released until the
function's outermost block ends, and once by the `dup` the read of `xs`
performed. A uniqueness test at that point sees two references and correctly
declines to reuse. Adding a reuse primitive without changing the analysis would
produce a program that allocates exactly as much as it does now.

## What has to change

Four things, in this order. Each is separately testable, and the first is the
only one that can corrupt memory.

### 1. Ownership at the last use

A binding's release moves from "the end of the block that declared it" to
"after the last expression that reads it", and a read that *is* the last use
does not `dup` — it hands its reference over.

The hard part is "on every path". A value read in one arm of a `match` and not
in another must be released in the arm that did not read it, or leaked. A value
read inside a loop is read on every iteration. A `break` out of a loop leaves
scopes early, and `raise` leaves several at once — `unwind_to` already knows
how to release along that path and will need to release a different set.

This is the whole of the risk. Getting it wrong is a double free or a use after
free, and both present as a crash somewhere unrelated. It wants:

- a per-expression **liveness** result, computed backwards over the body;
- the existing conservative plan kept as an oracle, so a differential test can
  assert that the two agree on *what* is released, and differ only on *where*;
- the object-count assertions the suite already carries, which is what makes
  the change observable at all. `docs/design/compatibility.md` says allocation
  behaviour is not part of the language's promise — those tests are the
  compiler's own instrument, not a contract with anybody.

### 2. Reuse tokens

Once a scrutinee is uniquely held at the point an arm allocates:

```
token = khora_drop_reuse(xs, drop_glue)   // null unless uniquely held
...
result = khora_alloc_reuse(token, size, tag)
```

`khora_drop_reuse` decrements; if that was the last reference it runs the drop
glue over the fields and returns the object's memory **without freeing it**.
Otherwise it returns null, having released normally.

`khora_alloc_reuse` takes the token: null allocates, a token whose `field_bytes`
matches writes the new tag and a refcount of one into the memory it was given,
and a token of the wrong size is freed and replaced. The live-object counter
stays honest in all three cases, which matters because the tests read it.

**Every token must be consumed on every path**, or the memory leaks — a token
is an allocation with no owner. The conservative rule for the first version is
to emit `drop_reuse` only where the arm unconditionally constructs, so that the
pairing is syntactic and visible in one place.

### 3. Drop specialization

A drop whose object type is known statically does not need the runtime's
generic path: the field count, the glue and the layout are all compile-time
constants. This is the cheapest of the four and the least interesting, and it
should be measured before it is written — errata 45's rule applies, and two of
the four optimisations in the throughput work were worth nothing.

### 4. Borrowed parameters, and D10's escape analysis

A parameter only *read* by a function does not need to be owned by it, which
removes a dup at every call site. And an object that provably does not leave
its fiber can use non-atomic reference counting, which decision D10 promised.

**Priced, because it was about to be assumed.** A throwaway build with the
atomics replaced by plain reads and writes, against the same build with them:

    parsing an 80-byte request     2,365 ns -> 2,075 ns
    parsing a browser's request   13,210 ns -> 11,600 ns

**Twelve per cent**, and that is the ceiling rather than the estimate — a real
escape analysis would only reach some of the operations. Worth having and not
worth reordering the phase for, which is the opposite of what "every `dup` is a
lock-free atomic" sounds like.

The first version of this measurement said 1.7x, because it compared the spike
against a number taken before the nullary-constructor change. Errata 45's rule,
caught in the act: a benchmark that is off by a constant factor everywhere is
measuring the wrong pair of builds.

The same spike run against the *server* is not a measurement at all — it
corrupts memory within a second, because a fiber is an OS thread and the
refcounts are shared. That is D10 being right, observed directly.

Both of these need an escape analysis and both change a calling convention, so
they come after the first two have settled.

## Where the operations actually go

Counted, on the same workloads, with a temporary counter in `khora_dup` and
`khora_drop`:

| workload | allocations | reference-count operations |
| --- | --- | --- |
| parse an HTTP request | 55 | 677 |
| a JSON round trip | 128 | 2,290 |
| 100 `Option` constructions | 51 | 2,498 |
| 100 failed `Map::get` | 5 | 5,704 |

**Reference counting outnumbers allocation by more than ten to one, and by a
thousand to one where nothing is allocated at all.** Fifty-seven operations for
one failed map lookup is the conservative scheme working exactly as written: a
read `dup`s and the block that declared the binding `drop`s, whether or not
anything needed the reference to survive.

That is the argument for ordering §1 first and it is stronger than the argument
from reuse. Making each operation cheaper is worth twelve per cent; **not
performing it** is worth whatever fraction of those 677 were never needed.

## How it will be measured

`bench/` measures throughput; this needs allocation counts, which are a
different instrument. `khora_live_count()` gives live objects, and the runtime
also has a total-allocations counter — the exit criterion for phase 9 is
expressed in the second:

> `map` over a uniquely-owned list performs **zero** allocations.

The test is a counting-allocator assertion in the codegen suite, in the shape
`crates/khora-codegen-llvm/tests/vector.rs` already uses. It should be written
first, marked as the target, and watched to fail — a phase whose exit criterion
passes before the work starts was measuring the wrong thing.

## What this does not change

**Nothing a program can observe.** `docs/design/compatibility.md` decides that
when memory is allocated and freed is not observable, and that decision was
taken before this work rather than after it, precisely so this work would be
legal. A program whose behaviour changes because an allocation stopped
happening was relying on something it was never promised — and if one is found,
it is a bug in this analysis, not a licence to keep the allocation.

The one exception is the compiler's own tests, which assert exact object
counts. Those numbers will move, and each move should be read as the finding it
is rather than updated to whatever the new output says.
