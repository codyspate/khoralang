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

## FFI and unsafe boundaries

Khora still needs explicit rules around foreign code, thread-affine state, and operations the compiler cannot prove safe. The production toolchain documents those boundaries rather than pretending automatic memory management eliminates systems-level invariants.

Khora is not intended to replace Rust for every low-level kernel/embedded use case. Its target is reliable native application and service software where ownership complexity is often a larger development cost than a benefit.
