---
title: Resources and regions
sidebar:
  order: 8
---

Resources such as sockets, files, transactions, and foreign handles have lifetimes that ordinary memory management cannot infer. Khora ties their cleanup to structured scopes so normal return, typed failure, early `return`, and cancellation use the same release path.

The two everyday tools are `scoped`, which opens a resource scope, and `acquire`, which registers how a value is released.

## Run work inside a scope

A function that acquires scoped resources states that requirement with `Scope`:

```khora
fn use_resource() -> Int
  with { scope: Scope }
{
  let handle = acquire(open_handle(), close_handle);
  read_value(handle)
}
```

`acquire(value, release)` returns `value` immediately and registers `release(value)` for the end of the current scope. The caller that owns the lifetime installs the scope with `scoped`:

```khora
fn run() -> Int {
  scoped(use_resource)
}
```

The `scope` capability appears in `use_resource` and disappears from `run`: `scoped` provides it for exactly the lifetime of the body.

The standard signatures are:

```khora
pub effect Scope {
  defer: (() -> ()) -> (),
}

pub fn scoped<A, 'ef, 'er>(
  body: () -> A with { 'ef | scope: Scope } raises 'er
) -> A
  with 'ef
  raises 'er

pub fn acquire<A, 'ef>(value: A, release: (A) -> ()) -> A
  with { 'ef | scope: Scope }
```

Pass a named function to `scoped` when the function receives the `scope` capability. That keeps capability passing explicit and lets row subtraction remove `scope` from the caller.

## Regions are the primitive underneath

`scoped` is built on `Region`. You can use a region directly when you need to register finalizers yourself:

```khora
fn work() -> Int {
  let region = Region::open();

  Region::defer(region, fn () => print("first cleanup"));
  Region::defer(region, fn () => print("second cleanup"));

  42
}
```

When `work` exits, the finalizers run in reverse registration order: `second cleanup`, then `first cleanup`.

The core region API is:

```khora
pub type Region;

impl Region {
  pub fn open() -> Region;
  pub fn root() -> Region;
  pub fn defer(self, finalizer: () -> ()) -> ();
}
```

`Region::open()` creates a region owned by the current scope. `Region::root()` refers to the program's outer region, whose finalizers run when the program exits.

## Cleanup runs on every structured exit

A finalizer is not merely an end-of-block callback. It runs when the owning region is released, including when control leaves through an early return or a typed failure:

```khora
pub type LoadError = | Failed;

fn load(should_fail: Bool) -> Int raises LoadError {
  let region = Region::open();
  Region::defer(region, fn () => print("released"));

  if should_fail {
    raise LoadError::Failed
  }

  7
}
```

The same rule applies to cancellation. Cancellation unwinds through the structured scopes between the cancellation point and the fiber root, so registered finalizers run before the cancelled work is discarded.

That is the property resource abstractions should depend on: **if the scope ends, cleanup runs**.

## Cancellation points matter

Cancellation is not an exception that can arrive between arbitrary instructions. A pending cancellation is observed at a cancellation/failure propagation point such as `!`.

That makes code between marked points understandable while still allowing blocked or suspended work to be woken so it can unwind and release resources. A `catch` handles declared failures; it does not swallow cancellation.

## Transactions follow the same rule

A database transaction is a resource scope with a richer finalizer policy:

- successful completion commits;
- typed failure rolls back;
- cancellation rolls back before the connection is returned to its pool.

Put that policy in the transaction abstraction rather than asking every caller to remember all three paths.

## Prefer scoped APIs

When designing a package, prefer this shape:

```khora
fn with_connection<A, 'ef, 'er>(
  body: (Connection) -> A with 'ef raises 'er
) -> A
  with 'ef
  raises 'er
```

over returning a raw handle whose caller must remember to close it on every exit path.

Use the [Memory and resources reference](/docs/reference/memory-and-resources/) for the exact lifetime rules and [Fibers and nurseries](/docs/guide/fibers-and-nurseries/) for how cancellation interacts with concurrent work.
