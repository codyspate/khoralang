# Reuse and FBIP

How phase 9 gets from correct reference counting to minimal reference counting,
and what has to be true before a `map` over a list can allocate nothing.

> **The fusion is the easy half. The analysis is moving a release to the last
> use of a value, on every path, and that is what turns a wrong answer into a
> double free rather than a slow program.**

## Where things stand

`khora-perceus` inserts reference counting that is *correct* and, until §1
below, deliberately not *minimal*. A local owned one reference for its whole
scope, reading it `dup`ed, and the block released what it declared on the way
out.

Nothing can be reused today, and the reason is worth being precise about,
because it is not "the fusion has not been written yet":

```khora
fn increment(xs: List<Int>) -> List<Int> {
  match xs {
    List::Nil => List::Nil,
    List::Cons(head, tail) => List::Cons(head + 1, increment(tail)),
  }
}
```

At the `List::Cons(..)` in the second arm, the cell that was matched is still
held. §1 has since dealt with the two references that used to hold it — the read
of `xs` now takes the binding's reference rather than copying it — but one
remains, and it belongs to the `match` itself: `lower_match` puts the scrutinee
in a `Cleanup::Temp` for the duration of the arms, because something has to
release it and an arm is not guaranteed to. A uniqueness test at the constructor
sees that reference and correctly declines to reuse.

So §2's problem is now a single, well-located one: **release the scrutinee
before the arm's constructor rather than after the arm**, on the arms that reach
a constructor of the same shape. That is a smaller and more tractable thing than
what this paragraph used to describe, and it is the whole of what stands between
here and reuse. Adding a reuse primitive without it produces a program that
allocates exactly as much as it does now.

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

**Done, for a body that cannot unwind.**

*The last-use move.* A backward liveness pass over the body: `live` is the set
of bindings still needed after the point being looked at, and a read of a
binding that is not in it takes the reference rather than copying it. This began
as a forward pass that could only settle a binding all of whose reads were
unconditional, which was worth 7% of the reference-count operations in an HTTP
parse and no measurable time — in a parser almost every read sits inside a
`while` or an `if`.

*Borrowed parameters.* `Region::defer` does not keep the region, `Shared::get`
does not keep the cell, `String::byte` does not keep the string. Each was handed
an owned reference and dropped it — two atomic operations to pass something the
callee only reads. Naming them is worth far more, because **a borrow applies
inside a loop**:

    parsing an 80-byte request    2,440 ns -> 2,165 ns
    parsing a browser's request  14,560 ns -> 10,210 ns
    lowercasing a 4-byte header     310 ns ->     90 ns

Three things this found, all from tests:

- **`&&` and `||` short-circuit**, so the right-hand side is a branch that does
  not look like one. Treating it as straight-line leaked one object.
- **A borrowed read is still a read.** Leaving borrows out of the last-use list
  meant `f(s)` followed by `String::byte(s, 0)` moved the binding into `f` and
  then read the freed object. A borrow cannot take ownership; it can certainly
  come after the read that would have.
- **Only a bodyless declaration may be borrowed.** A Khora function owns its
  parameters, so promising its caller a borrow is a use after free.
  `Array::prefix` and `String::matches_at` are written in Khora and were briefly
  on the list.

And the diagnosis that borrows corrected. Moving a `Region` to its last use ran
its finalizers early, which looked like "some types have an observable release"
and produced a restriction to `String`. The real cause was that `defer` borrows
and the plan said it consumed. With borrows named, a region passed to `defer`
keeps its reference and ends with its scope, and the last-use move applies to
every type.

*Branches.* A branch **consumes** a binding when every path through it does. So
where one arm takes the reference, each arm that does not is given a release —
and the only place on exactly the right set of paths is the arm's head, which is
why the rule is narrow: an arm may be given one only if it never mentions the
binding at all. An arm that merely reads it would be reading freed memory, and
that branch settles nothing; its reads go back to copying and its block releases
as before.

That second half is where the value is. Together: 314 reference-count operations
in an HTTP parse down to 278, and 1,955ns to 1,855ns. Measured with a throwaway
counter in `khora_dup` and `khora_drop` — not kept, because an unconditional
atomic in the hot path perturbs the thing being measured.

Three rules earn their keep, and each was found by a double free rather than by
thinking about it:

- **A `match` arm's bindings own nothing.** They are projections of the
  scrutinee's payload — no block releases one, which is what
  `match_arm_bindings_are_not_released_by_the_arm` asserts — so a read of one
  has to copy. There is no reference sitting in the binding to hand over.
  Taking one freed the list node the recursion was standing on.
- **Only an arm that never mentions the binding may release at its head.** An
  arm that borrows it reads after the free. Stated above; it is repeated here
  because the tempting generalization is "an arm that does not take it", which
  is wrong.
- **A binding an arm introduces itself is not the branch's to settle.** It does
  not exist on the other paths, so a release at their head reads a slot nothing
  ever wrote. Excluded by name: an arm's own pattern bindings, and anything a
  `let` inside an arm declares.

**A body that can unwind keeps the conservative plan entirely**, and that is the
piece still owing. `raise`, `!`, `catch` and `return` leave a frame from the
middle, and what is still owned there depends on how far execution got.
`lower.rs` holds cleanups in `scopes: Vec<Vec<Cleanup>>` and unwinds by walking
it, so the set it releases is fixed at each lowering position and cannot be made
path-dependent without changing that representation. The analysis above is
already written to answer the question; the code generator cannot yet act on the
answer.

What made the change survivable, and is worth keeping for 9.2:

- the object-count assertions the suite already carries, which is what makes
  this observable at all. `docs/design/compatibility.md` says allocation
  behaviour is not part of the language's promise — those tests are the
  compiler's own instrument, not a contract with anybody;
- the runtime's refusal to decrement a count that is already zero, which turns
  every one of the three rules above from a silent corruption into a message
  naming the program that did it.

### 2. Reuse tokens — done

**A ten-element `map` over a list nothing else holds allocates nothing**, which
is phase 9's exit criterion and is asserted by
`a_uniquely_owned_walk_allocates_nothing`. Two steps got there.

The reference standing in the way was exactly one, and it is the `match`'s
own: `lower_match` pushes `Cleanup::Temp(scrutinee)` before the arms and leaves
that scope after them. §2 needs that release to happen *at the arm's head*
instead, where an arm can replace it with a `drop_reuse`.

Which cannot be done by moving one line, because **an arm's bindings do not own
what they point at**. `bind_pattern` stores the loaded field straight into the
slot and reads of it `dup`; the binding is a borrowed view into the scrutinee's
payload, valid only for as long as the match holds the scrutinee. Releasing the
scrutinee at the arm's head would free the payload out from under every one of
them.

So the order had to become the ordinary owning one, and this was §2's first
step:

1. at the arm's head, `dup` each boxed binding the pattern introduced, so the
   arm owns them;
2. then release the scrutinee — the point that later becomes `drop_reuse`;
3. reads of those bindings stop `dup`ing, because they are now ordinary owning
   locals, and the last-use pass in §1 applies to them like any other;
4. the arm's block releases whatever it did not hand on.

The operation count does not move: `Cons(h, t) => Cons(h + 1, f(t))` performs a
dup and a drop either way. What changes is *when* — the scrutinee's count
reaches zero before the arm's constructor rather than after it, which is the
entire prerequisite for handing its memory over.

`RcPlan::arm_binds` says which bindings a pattern owns, and it holds only the
ones the arm's *body* reads — owning one the body never touches would be a copy
and a release for nothing. `unowned` in `settle_last_uses` keeps the rest, and a
`catch` arm's bindings entirely: a partial rollout of this is a use after free,
so the two sets are complements by construction rather than by intention.

Two paths needed saying out loud, and neither is the arm:

- **A guard runs before any of it.** The `match` still pushes the scrutinee as a
  scope cleanup, because a guard that raises, breaks or continues unwinds
  through there. `emit_arms` empties that scope level for the length of each
  arm's body, which is the only stretch where the release has already happened.
- **A guard on the last arm can fail**, and then no arm ran and nothing
  released. That edge gets a block of its own that releases and joins, rather
  than going straight to the join.

Then, where an arm reaches a constructor:

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

**Every token must be spent on every path**, or the memory leaks — a token is an
allocation with no owner, which no counter is watching and nothing will free.
That single requirement decides the whole shape of the rule, and the rule is
deliberately syntactic: an arm qualifies when its body **is** the constructor
and nothing inside it can leave the frame early — no `!`, `raise`, `return`,
`break`, `continue` or `catch`. There is then one path from the release to the
allocation and no way to take another. Everything this declines is a missed
optimization; anything it wrongly accepted would be a leak.

Two details that make the rule cheaper than it looks:

- **The shapes are compared at run time, not compile time.** The size is in the
  header, so `khora_alloc_reuse` needs one comparison rather than the analysis a
  static answer would want. An arm may hand over a token without proving
  anything about what it is going to build.
- **The token is matched by expression id.** A constructor's *arguments* can
  contain constructors of their own, and the arm promised this one in
  particular — `Tree::Node(Tree::Leaf(1), t)` must not let the inner one take
  the cell.

And a safety net that is not part of the design: `khora_free_reuse`, emitted at
the end of every arm that took a token, freeing one nothing spent. The rule
above says it is unreachable. "Unreachable" and "unreached" differ by one
lowering path nobody foresaw, and the difference between them is memory nothing
owns.

What it is worth, beyond the exit criterion. On an HTTP request parse, 54
allocations to 50 and 1,855ns to 1,770ns; `bench/service` 507k to 548k req/s,
which is at the edge of the 8% that benchmark varies by. The honest reading is
that reuse pays where a program walks a structure and rebuilds it, and that a
request parser mostly does not — it builds strings and hashes them. The list
walk is not a toy chosen to flatter it, but it is the shape reuse is for.

### 3. Drop specialization — done, and it was not the cheapest

Written expecting nothing. It was the second largest win in the phase.

The claim here used to be that a drop whose object type is known statically does
not need the runtime's generic path — the field count, the glue and the layout
are compile-time constants — and that it was "the cheapest of the four and the
least interesting". The reasoning was about the *work* the runtime does. What
actually cost was the **call**: an HTTP parse performs 280 reference-count
operations against 50 allocations, and 230 of those calls did nothing but add or
subtract one from a word.

The refcount is the first field of the header, so the pointer generated code
already holds is a pointer to it. A `dup` is now a null test and one relaxed
atomic add, emitted inline. A `drop` is a null test, one release atomic
subtract, and a branch that is not taken — only the *last* reference calls
`khora_drop_last`, which holds the fence, the field-dropping callback and the
deallocation. The already-zero abort lives there too, rather than being a second
branch every drop in the program pays for to catch something that must not
happen.

    parse, 80-byte request       1,770 ns -> 1,670
    parse, browser request       8,935 ns -> 8,360

`khora_dup` and `khora_drop` stay in the runtime. `khora-rt` is a C ABI anything
may link against, drop glue still calls them for the fields it releases, and the
module documentation's claim that "generated code never touches the refcount" is
the one thing here that had to be retracted rather than refined.

**How the measurement nearly went wrong.** The first attempt at an envelope was
a throwaway runtime with `khora_dup` and `khora_drop` returning immediately. It
measured *slower* — nothing is ever freed, so the working set grows without
bound and the allocator loses its cache. A no-op runtime does not measure what
reference counting costs; it measures a program with a leak. The number above
came from building the thing and comparing.

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
