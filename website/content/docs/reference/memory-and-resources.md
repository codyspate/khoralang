---
title: Memory and resources
sidebar:
  order: 20
---

Khora manages ordinary memory automatically. Resource lifetimes that have external meaning—files, sockets, transactions, foreign handles—use explicit structured cleanup.

## Ordinary memory

There is no source-level borrow or lifetime syntax for ordinary Khora values. The runtime uses reference counting and the compiler may remove retain/release operations or reuse uniquely owned storage when doing so cannot change program meaning.

The programmer-visible rules are:

- a value remains valid according to ordinary lexical and type semantics;
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

Finalizers execute in **reverse registration order** when the region is released.

`Region::root()` refers to the outer program region. Its finalizers run as the program exits.

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

A `catch` handles failures in a `raises` row. Cancellation is not a failure variant and is not consumed by `catch`.

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

Foreign or thread-affine resources can impose rules beyond ordinary Khora values. See [FFI](/docs/reference/ffi/) for pointer and suspension constraints, [Concurrency](/docs/reference/concurrency/) for fiber lifetime rules, and [Sharing](/docs/reference/sharing/) for cross-fiber values.
