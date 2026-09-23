---
title: Memory
sidebar:
  order: 1
---

Khora has no tracing garbage collector and no lifetimes to write down. Memory
is managed by reference counting, and most of the counting is removed before
the program runs.

## Reference counting, mostly at compile time

Every heap object carries a count of how many things refer to it. When the
count reaches zero the object is freed immediately, at a point you could
predict from the source. There is no collector pass and no pause.

Counting every reference at run time would be slow, so the compiler does most
of the work first. It tracks ownership through each function and emits a
retain or release only where it cannot prove one is unnecessary: a value
created and consumed in one function usually costs nothing at all, because the
compiler can see the whole story and there is nothing to count.

This is [Perceus](https://www.microsoft.com/en-us/research/publication/perceus-garbage-free-reference-counting-with-reuse/)
reference counting, from Microsoft Research, which is also where Khora's effect
system comes from.

### Reuse in place

The compiler also knows when a value is the last reference to its storage. When
such a value is released and an object of the same shape is allocated
immediately afterwards, the allocation reuses the storage rather than asking
the allocator for more.

The visible effect is that the obvious way to write a transformation is often
the efficient one. Mapping over a list whose elements you own does not have to
allocate a second list; the cells are rewritten as the walk goes.

What you can rely on:

- **Reuse never changes behaviour.** If the optimisation cannot be applied the
  program allocates instead, and nothing observable differs.
- **You cannot detect it from inside the program**, other than by counting
  allocations.

## The counts are atomic

A fiber can run on any worker thread, and a spawned fiber shares at least the
closure it was handed — so the counts have to be safe to change from two
threads at once. They are.

This costs an atomic instruction on each retain and release that survives to
run time. There is no cheaper single-threaded variant; the cost is the same in
every program.

## Cycles leak

Reference counting cannot free a cycle: each object in the loop is referred to
by another, so no count reaches zero.

Khora has mutable fields, so a cycle is constructible:

```khora
pub type Node = { name: String, mut next: Option<Node> };

let a: Node = { name: "a", next: Option::None };
let b: Node = { name: "b", next: Option::None };
a.next = Option::Some(b);
b.next = Option::Some(a);
```

Those four objects are never freed.

**This is a leak, not unsoundness.** Nothing is freed early, nothing is read
after being freed, and the program stays correct — the memory is simply never
returned.

It is also hard to do by accident. Three properties of the language make the
reference graph acyclic unless you deliberately write a cycle:

- **Values are built bottom-up.** A constructor's arguments are evaluated
  before the object exists, so a new object can only point at older ones.
- **Closures capture by value** at the moment they are created.
- **A `let` initialiser cannot see its own binding.** `let x = f(x)` refers to
  an outer `x`, not the one being declared.

So a cycle requires assigning through a `mut` field, into an object that
already reaches back. Parent pointers and doubly-linked structures are the
shapes to watch.

Khora has no weak reference today, which is the usual way to break a cycle
deliberately. Structures that need one have to be built with an index or a key
instead of a direct reference.

## What crosses a fiber

A value captured by a spawned fiber must be shareable. Mutable state is not:

> A record with a mutable field cannot be captured by a spawned fiber, and nor
> can anything holding one.

The rule is structural and transitive, and the compiler checks it at the one
place a value can cross — the captures of the closure handed to `spawn`. There
are no references in Khora, so nothing escapes any other way, and one check
covers the whole language.

To share state deliberately, use `Shared<A>`, which takes a lock and is safe to
capture. Where another language needs `Arc<Mutex<HashMap<K, V>>>`, Khora has
`Shared<Map<K, V>>`: the counting is implicit and there is no lifetime to name.

[Sharing](/docs/reference/sharing/) has the rules as they apply to code
you are writing.

## Resources are not memory

Memory is reclaimed when the last reference goes. A file handle, a socket or a
transaction has an *observable* end — the other side notices — so those are
closed by [regions](/docs/reference/memory-and-resources/#region-syntax)
rather than by the reference count, at a point the program states rather than
one the optimiser chooses.
