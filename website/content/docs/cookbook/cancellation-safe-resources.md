---
title: Cancellation-safe resources
sidebar:
  order: 5
---

A resource should register its cleanup as soon as acquisition succeeds. In Khora, `scoped` creates the lifetime and `acquire` ties a value's release function to that lifetime.

That means the same cleanup path runs when the body returns normally, raises a typed failure, or is unwound by cancellation.

## Complete example

This example intentionally raises after acquiring a resource. The registered release still runs before the failure leaves the scope:

```khora
module main;

import std::core::{Scope, acquire, print, scoped};

pub type Resource = {
  name: String,
};

pub type UseError =
  | Failed;

fn open_resource() -> Resource {
  print("open connection");
  { name: "orders" }
}

fn close_resource(resource: Resource) -> () {
  print("close ${resource.name}");
}

fn fail_after_open() -> Int
  with { scope: Scope }
  raises UseError
{
  let resource = acquire(open_resource(), close_resource);
  print("using ${resource.name}");
  raise UseError::Failed
}

pub fn main() {
  let result = scoped(fail_after_open)! catch {
    UseError::Failed => -1,
  };

  print("result = ${Int::to_string(result)}");
}
```

The lifetime is established here:

```khora
let resource = acquire(open_resource(), close_resource);
```

`acquire` returns the resource immediately, but also registers `close_resource(resource)` with the active `Scope`. The body does not need a later `close_resource(...)` line that can be skipped by non-local control flow.

`scoped` supplies the `scope` capability required by `fail_after_open`:

```khora
scoped(fail_after_open)
```

When `fail_after_open` raises, the scope unwinds, the registered release runs, and only then does `UseError::Failed` continue outward to the `catch` in `main`.

## Cancellation uses the same lifetime rule

Cancellation does not require a second cleanup mechanism. If a fiber is cancelled at a cancellation point while it owns this scope, unwinding releases the scope and runs the same registered finalizers before the fiber is finished.

That is the key production rule: **do not rely on the line after the work to release a resource**. Put release behavior into the resource lifetime itself.

The same pattern underlies cancellation-safe transactions, pooled connections, sockets, files, permits, and tracing spans.

See [Resources and regions](/docs/guide/resources-and-regions/) for `Region`, `Scope`, `scoped`, and `acquire`, and [Fibers and nurseries](/docs/guide/fibers-and-nurseries/) for cancellation behavior.
