---
title: Memory and resources
sidebar:
  order: 20
---

Khora provides automatic memory management without a tracing garbage collector and without requiring ordinary source code to carry borrow/lifetime annotations.

The runtime uses reference counting; compiler ownership analysis removes unnecessary retain/release work and may reuse uniquely owned storage when that does not change program meaning.

This is an implementation strategy with programmer-visible guarantees:

- values remain valid according to ordinary lexical/type semantics;
- optimization must not make observable behavior depend on whether storage was reused;
- sharing across fibers is explicit and subject to the language's sharing rules;
- external resources are not memory and require structured cleanup.

Resources such as sockets, files, transactions, and foreign handles belong to regions/scopes with finalization rules. Cancellation must unwind through those scopes rather than bypassing cleanup.

Foreign/thread-affine resources may impose stronger rules than ordinary Khora values. The FFI reference documents those constraints where the compiler cannot infer them automatically.
