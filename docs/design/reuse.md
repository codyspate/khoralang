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

*Bodies that can unwind.* These were skipped entirely at first, and the reason
was real: `raise`, `!`, `catch` and `return` leave a frame from the middle, and
what is still owned there depends on how far execution got. `lower.rs` holds
cleanups in `scopes: Vec<Vec<Cleanup>>` and unwinds by walking it, so the set it
releases is fixed at each *lowering position* — and a lowering position cannot
answer a question two runtime paths answer differently.

The way out is not to make the compile-time set path-dependent but to stop
asking it. **The block keeps its release, and a take clears the slot.** Before
the take the binding is the block's, and a `raise` passing through releases it;
after the take the slot holds null, and releasing null is a no-op the runtime
already tolerates. "Has this been handed on" becomes a question the slot answers
at run time, which is the only thing that can answer it.

That store is paid only where something can unwind. Where nothing can, the block
never lists the binding at all, the answer is static, and the store would be
dead — which is the common case and stays free.

It removed 56 of the 280 reference-count operations in an HTTP parse, a fifth of
them, **and moved the clock by nothing measurable**. Both halves of that are
worth stating. After §3 each remaining operation is a handful of instructions,
so fifty-six of them is somewhere near 1% of a 1,570ns parse and beneath what
this benchmark can resolve. It is a real win that this measurement cannot see,
not a win that is not there — and the honest summary of §1 and §3 together is
that reference counting has stopped being where the time goes.

**What it found was worth more than what it saved.** Clearing the slot turns a
wrong answer from the analysis into a null dereference instead of a stale
pointer that usually still works, and the first thing it caught was a bug older
than this pass: **a capability is read where nothing mentions it**. `with {
clock: Clock }` puts `clock` in scope, and a call to anything that also wants a
`Clock` is handed the evidence by code generation — there is no expression for
that read. The link shortener's `health` mentions `clock` once and forwards it
twice afterwards, so the pass called the mention a last use and handed the
binding away. `Body::capabilities` records exactly which binding supplies each
label at each call site, and is now what decides this.

That bug did not need an unwinding body. It was reachable before, and survived
because the binding kept pointing at a handler the enclosing `with` block still
held: the count was one short rather than the pointer being wrong. A leak, on a
path nothing measured.

What made the change survivable, and is worth keeping for 9.2:

- the object-count assertions the suite already carries, which is what makes
  this observable at all. `docs/design/compatibility.md` says allocation
  behaviour is not part of the language's promise — those tests are the
  compiler's own instrument, not a contract with anybody;
- the runtime's refusal to decrement a count that is already zero, which turns
  every one of the three rules above from a silent corruption into a message
  naming the program that did it.

#### The borrow table was a second calling convention — enforced since 10.2

`borrowed_arguments()` in `khora-perceus` is a hand-written table keyed by
`(type name, method name)`:

```rust
("Shared", "get" | "set" | "update" | "modify") => RECEIVER,
("Array",  "get" | "set" | "length" | "is_utf8") => RECEIVER,
```

It works, and its own doc comment already carries the two rules that keep it
honest — only bodyless declarations may appear, and a Khora-implemented function
listed here is a use after free. An outside review named the longer-term risk
and it is the right one: **this is a calling convention maintained in a
different place from the declaration it describes.** The runtime, the type
checker, this table and LLVM lowering can eventually disagree about whether an
argument is borrowed, and nothing would notice.

The eventual fix is one declaration of the convention, attached to the
intrinsic:

```khora
extern fn byte(borrow self: String, index: Int) -> U8
```

— not necessarily user-facing syntax, and possibly just compiler metadata
registered where the intrinsic is. That is still the right shape and it is still
not written.

**What 10.2 did instead was enforce the rule this table already stated.** Its
doc comment has always said only bodyless declarations may appear here, because
a Khora body owns its parameters and releases them, so telling its caller to
lend one is a use after free. That rule was kept by whoever edited the file.
Packages made it unkeepable: the key is a bare type *name*, and anyone may now
write

```khora
export type Shared = { .. };
impl Shared { export fn get(self) -> Int { .. } }
```

whose `get` would have been told its caller was lending. The caller makes no
reference, the callee releases one anyway, and the object is freed while
somebody still holds it — in a package whose only mistake was choosing a common
noun.

So the planner is handed `Defined`, the set of methods the program implements in
Khora, and the table is consulted only for a pair nothing implements. Built from
the source root by `rc_plans` and from monomorphization by the backend.
`a_packages_own_shared_is_not_borrowed` is the test; it fails without it.

**One wrong turn is worth recording, because it looks more obvious than the
right one.** The first attempt restricted the table to types `std` declares,
which is what "attach the convention to the declaration" sounds like it means.
It broke three fiber tests. A self-contained program may perfectly well declare
its own `Region` and let the runtime implement `defer` — most of
`khora-codegen-llvm`'s tests do — and refusing to lend to them reordered a
program's finalizers, which is the exact failure this table was added to fix.
Where a declaration lives is not the property that matters. Whether anybody
wrote a body for it is.

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
came from building the thing and comparing. Errata 47, with the general rule.

### 4. Borrowed parameters, and D10's escape analysis

**4a, borrowed parameters — done**, and it was the one that paid before reuse
did. `Region::defer` does not keep the region, `Shared::get` does not keep the
cell, `String::byte` does not keep the string; each was handed an owned
reference and dropped it. Written up above, under §1.

**4b, non-atomic counting — done for a program that cannot spawn.** Measured
first: with the counts kept correct but the atomics removed, an HTTP parse ran
1,670ns to 1,555ns and a browser's 8,360 to 7,345. Seven and twelve per cent,
and that is a ceiling rather than an estimate.

`Fiber::spawn` is the only way a Khora program starts a thread —
`khora_fiber_spawn` is the sole runtime entry that calls `std::thread::spawn`,
and a nursery adopts fibers that already exist rather than making them. So if
nothing in the reachable program so much as mentions `Fiber::spawn`, there is
one thread for the program's whole life and every reference count can be plain
arithmetic. Whole-program monomorphization is what makes that answerable: the
compiler already has every body it will ever emit.

The reachable set comes from `mono.instances`, and the scan is conservative in
both of the ways it can be — it looks for a *mention* rather than a call, since
`Fiber::spawn` handed around as a value is still a spawn, and it reads the whole
expression arena rather than walking from the root, since an expression a walk
skipped would be a wrong answer while one it need not have visited is only a
missed optimization.

**And then it is checked at run time**, because the failure mode is the worst
one available: a data race in a reference count is memory corruption arriving a
long way from its cause, and no test finds it reliably. The generated `main`
calls `khora_single_threaded` when the compiler decided so, and
`khora_fiber_spawn` refuses to start a thread in a program that said it would
not. `a_program_that_spawns_counts_references_atomically` spawns and checks the
live count returns to zero; forcing the flag on makes it print the abort
instead, which is how that test was confirmed to be testing anything.

#### What is left, why it is per-object, and what it is worth

**The ceiling for the case that matters cannot be measured without building
it.** The 7% and 12% above are from the parse benchmark, which never spawns and
therefore already counts non-atomically — they are the win *already taken*. What
is left applies only to programs that do spawn, and the obvious way to price
that is to force a spawning program non-atomic and time it. Doing so to
`bench/service` produced 82 requests a second and then zero: the server corrupts
itself within a few hundred requests. That is not a measurement. It is a
demonstration that this is the one optimization in the phase with no margin for
being approximately right.

What can be said without building it: atomics are worth about 7% of an HTTP
parse, and a parse is a fraction of what a server does between accepting a
connection and writing a reply. A few per cent of throughput, which is inside
the eight the benchmark varies by.

**Two type-level rules that look sound and are not.** The first is `Share`:
Khora knows which types *may* cross a fiber, so an unshareable type could be
counted non-atomically. `String` is shareable. So is `Map`, so is `Option`, so
is every ordinary immutable container, because sharing an immutable value is
precisely the thing that ought to be allowed. The unshareable ones are the types
with mutable fields, `Ptr`, and opaque handles — close to none of what a request
parse allocates.

The second is sharper and fails for a more interesting reason: restrict it to
the types a `Fiber::spawn` closure actually *captures* in this program, rather
than the types that could be shared in principle. In `bench/service` the spawned
closure captures the router, and a router holds its route paths — so `String` is
in the captured set anyway, and with it everything a request is parsed into. One
`String` in one long-lived structure poisons the whole type for the program.

So it has to be per-*allocation-site*, and there are two known shapes:

- **A real escape analysis.** Which allocation sites can reach a `Fiber::spawn`
  capture. Whole-program and flow-sensitive; tractable here in principle,
  because monomorphization means there is no dynamic dispatch to lose the trail
  in. This is what D10 asked for, and the one that would help a server — the
  strings a request is parsed into are made inside the fiber answering it and
  never leave, which is exactly the fact a site-level analysis can see and a
  type-level one cannot.
- **Biased reference counting.** Each object records an owning thread; the owner
  uses a non-atomic count and everybody else an atomic one. No analysis at all,
  at the price of a wider header and a thread-identity comparison on every
  operation — perhaps half the ceiling, for a much smaller change, and no
  soundness argument to get wrong.

**Neither is started, and neither should be started for the number.** A few per
cent, for a whole-program flow analysis whose failure mode is a data race in a
reference count — memory corruption arriving far from its cause. If it is built
it should be because escape information is wanted for something else too, or
because biased counting is judged cheap enough to be worth having on its own
terms. The measurement above is what either would be judged against.

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
