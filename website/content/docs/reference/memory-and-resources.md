---
title: Memory and resources
sidebar:
  order: 13
---

Khora manages ordinary memory automatically. Resource lifetimes that have external meaning—files, sockets, transactions, foreign handles—use explicit structured cleanup.

## Ordinary memory

There is no source-level borrow or lifetime syntax for ordinary Khora values. The runtime uses reference counting and the compiler may remove retain/release operations or reuse uniquely owned storage when doing so cannot change program meaning.

The programmer-visible rules are:

- optimization cannot make behavior depend on whether storage was reused;
- writable state does not silently become cross-fiber shared state;
- external resources require their own cleanup policy.

## Region syntax

The primitive resource lifetime object is `Region`:

```khora
pub type Region;

impl Region {
  pub fn open() -> Region;
  pub fn root() -> Region;
  pub fn defer(self, finalizer: () -> ()) -> ();
}
```

Open a region and register finalizers with `defer`:

```khora
fn work() -> Int {
  let region = Region::open();
  Region::defer(region, fn () => release_second());
  Region::defer(region, fn () => release_first());
  42
}
```

Finalizers execute in **reverse registration order** when the region is
released, and a region is released **when the last reference to it goes** --
which for the ordinary case is the end of the block its binding is in, on every
way out of that block, including a cancellation. The exception is a reference
that outlives the block: a closure that captured the region keeps it open until
that closure goes too, so a captured region's finalizers run later than the
block, not at it. So the example above
runs its finalizers when `work` returns, and putting the `let` inside a smaller
block ends the region there instead:

```khora
{
  let region = Region::open();
  Region::defer(region, fn () => release());
  use_it();
};
// `release()` has already run here.
```

That is the difference between a lease that ends with the call and one that
ends with the caller, and it is worth being deliberate about: a `Region::open()`
at the top of a function holds everything until the function returns.

`Region::root()` refers to the outer program region. Its finalizers run as the program exits.

**A region stays on the fiber that opened it**, and so does a `Scope`: neither
is shareable, so neither can be captured by a fiber's body, sent on a channel,
or put in a `Shared` cell. Every finalizer therefore runs on the fiber that
deferred it, which is what lets one capture a record with `mut` fields that
the fiber goes on writing. A child fiber that acquires something is given a
scope of its own by handing `scoped` a named function,
`Fiber::spawn(fn () => scoped(work))`, and what it acquires is released when
that `scoped` ends. `Region::root()` and `Scope::root()` belong to the program's
own fiber, and a spawned fiber that calls either stops with a fatal error; a
`test` or `bench` block has a root region of its own, released when the block
ends. [Sharing](/docs/reference/sharing/#regions-stay-home) has the rule, why
the named function matters, and what it costs.

## Scope capability

The standard structured-resource capability is:

```khora
pub effect Scope {
  defer: (() -> ()) -> (),
}
```

`scoped` creates a fresh region, installs a `Scope` handler over the body, and removes that capability from the caller's required row:

```khora
pub fn scoped<A, 'ef, 'er>(
  body: () -> A with { 'ef | scope: Scope } raises 'er
) -> A
  with 'ef
  raises 'er
```

Example:

```khora
fn inside() -> Int
  with { scope: Scope }
{
  scope.defer(fn () => cleanup());
  7
}

fn outside() -> Int {
  scoped(inside)
}
```

A named function is the normal argument to `scoped` when that function requires the `scope` capability.

## Acquire and release

`acquire` registers a release operation and returns the acquired value:

```khora
pub fn acquire<A, 'ef>(value: A, release: (A) -> ()) -> A
  with { 'ef | scope: Scope }
```

Typical form:

```khora
fn use_connection() -> ResultValue
  with { scope: Scope }
{
  let connection = acquire(open_connection(), close_connection);
  query(connection)
}
```

`release(value)` runs when the enclosing resource scope ends.

## Exit semantics

A region is released on every structured path out of its owner:

```khora
pub type WorkError = | Failed;

fn work(fail: Bool) -> Int raises WorkError {
  let region = Region::open();
  Region::defer(region, fn () => cleanup());

  if fail {
    raise WorkError::Failed
  }

  return 1;
}
```

The finalizer runs on the normal path, the explicit `return`, and when the `raise` leaves the function.

Cancellation uses the same structured unwind path. A pending cancellation observed at a cancellation point releases intervening regions and runs their finalizers before the fiber terminates.

A `catch` handles failures in a `raises` row. Cancellation is not a failure
variant and is not consumed by `catch`.

Cancellation is not an exception that can arrive between arbitrary
instructions: a pending one is observed at a cancellation or failure
propagation point, such as `!`. That is what keeps the code between two marked
points readable as ordinary straight-line code, while still letting blocked or
suspended work be woken so it can unwind and release what it holds.

The invariant a resource abstraction should be built on is the short one: **if
the scope ends, cleanup runs.**

## Resource APIs

A resource-owning API should generally keep the lifetime inside one call:

```khora
fn with_resource<A, 'ef, 'er>(
  body: (Resource) -> A with 'ef raises 'er
) -> A
  with 'ef
  raises 'er
```

rather than returning an unmanaged handle that callers must remember to close on every path.

A database transaction is this shape with a richer finalizer policy, and it
belongs in the transaction abstraction rather than in every caller:

- completing normally commits;
- a typed failure rolls back;
- cancellation rolls back before the connection returns to its pool.

[Database transactions](/docs/cookbook/database-transactions/) is that policy
written out.

Foreign or thread-affine resources can impose rules beyond ordinary Khora values. See [FFI](/docs/reference/ffi/) for pointer and suspension constraints, [Concurrency](/docs/reference/concurrency/) for fiber lifetime rules, and [Sharing](/docs/reference/sharing/) for cross-fiber values.
