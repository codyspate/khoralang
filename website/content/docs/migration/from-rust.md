---
title: From Rust
sidebar:
  order: 3
---

Khora shares Rust's goal of native, predictable software without requiring a tracing GC, but it deliberately does not expose Rust's ownership proof system as the ordinary programmer interface.

## Memory

Khora source is written in a functional style with automatic memory management. The compiler/runtime use reference counting and ownership/reuse analysis to remove unnecessary reference-count operations and reuse uniquely owned storage where safe.

The programmer does not annotate borrows or lifetimes merely to express ordinary data flow.

## Failure and authority

Rust commonly uses `Result<T, E>` and explicit dependency values. Khora lifts recoverable failure into `raises` rows and external authority into `with` capability rows so both remain visible in function types without dominating every value-level composition.

## Concurrency

Khora uses structured fibers and direct-style I/O. Application functions do not split into synchronous and `async fn` forms, and suspension is a runtime property rather than a different source-level function kind.

## Visibility

`pub` works the way it does in Rust, in both of the places you would expect it.
A declaration without it belongs to its module, and a method without it belongs
to the module that declares the type:

```khora
pub type Counter = { n: Int };

impl Counter {
  pub fn doubled(self) -> Int { Counter::secret(self) }
  fn secret(self) -> Int { self.n * 2 }
}
```

One difference. A method of a *trait* implementation needs no `pub`: it is
reachable wherever the trait is, and what makes it public is the trait rather
than the impl. Writing the keyword on one method of an `impl Show for T` would
suggest the others were hidden.

Khora's module path separator is `::` as in Rust, but the declaration is
`module a::b;` at the top of a file and the import is `import a::b::{X};` — the
file is the module, so there is no `mod` tree to keep in step with the
directory layout.

## FFI: `extern fn`, and no `unsafe` keyword

There is no stable Khora-to-Khora ABI, for the same reason Rust has none: the
whole program is monomorphized. The interoperability boundary is the C ABI, and
it is spelled the way Rust spells it minus the block. An `extern fn` with no
body is a symbol the linker must find; a `pub extern fn` with a body is a Khora
function C can call:

```khora
extern fn monotonic_ticks() -> Int;
```

What replaces `unsafe` is the manifest. `extern` in `[permissions]` names the
**packages** that may declare `extern fn` at all — `std` always may — so the
audit is one table rather than a search for a keyword, and a dependency that
starts calling into C cannot do it quietly. [FFI](/docs/reference/ffi/) has the
ABI-safe types, the rules for borrowing a buffer for the duration of one call,
what a blocking foreign call costs a fiber, and what a trap does at an exported
boundary. [Capabilities](/docs/reference/capabilities/) has the permission
table.

Khora is not intended to replace Rust for every low-level kernel or embedded
use case. Its target is native application and service software where ownership
complexity costs more than it returns — and the honest version of that trade is
on [Limitations](/docs/limitations/), which is the page to read before choosing
between them.
