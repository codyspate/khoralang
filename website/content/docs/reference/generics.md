---
title: Generics
sidebar:
  order: 6
---

Khora supports parametric generics, const generics, trait bounds, and higher-kinded types.

Generic definitions are type-checked against their declared constraints. At native code generation time, whole-program monomorphization produces concrete instances for the types the program uses.

A generic parameter should remain unconstrained when the implementation does not need behavior from it. Add trait bounds only for operations the body actually performs.

Higher-kinded parameters allow abstractions over type constructors rather than only concrete inhabited types. The public reference will include the exact syntax and kind-checking rules before the production release; until then, compiler-checked examples and the implemented grammar are authoritative.

Monomorphization is also why Khora does not promise a stable native Khora ABI between separately compiled binary packages. Source packages and the C FFI boundary are the compatibility mechanisms.
