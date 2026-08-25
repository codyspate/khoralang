---
title: Typed failure with raises
sidebar:
  order: 6
---

Khora does not use unchecked exceptions for ordinary recoverable failures. A function that can fail declares that possibility in its type with `raises`.

```khora
fn load_user(id: Id) -> User raises DbError
```

Callers can propagate a declared failure with `!`:

```khora
let user = load_user(id)!;
```

The caller's own failure row must then account for `DbError`, or the failure must be handled before leaving the current scope.

Use `catch` or an effect handler when the current layer has enough context to make a decision. Do not catch merely to convert a typed failure into an unstructured string.

## Failure is part of the API

A function's `raises` row is documentation the compiler checks. It answers a question that many languages leave to prose: what normal failure conditions must a caller be prepared to handle?

Keep failure types meaningful at the abstraction boundary. A database package may expose detailed driver errors internally while an application repository converts them into a smaller domain-specific failure type.

## Traps are different

Bounds violations, arithmetic overflow, and similar traps represent bugs or violated invariants rather than routine business failure. They are intentionally distinct from `raises`; callers should not be forced to model programming errors as ordinary recoverable outcomes.
