---
title: Values and functions
sidebar:
  order: 1
---

Khora is expression-oriented and statically typed. Type inference handles ordinary local code, while annotations remain useful at module boundaries and anywhere they make an API clearer.

```khora
fn double(n: Int) -> Int {
  n * 2
}
```

A function's final expression is its result. Pure functions are ordinary Khora code; operations that can fail or require external authority are represented in the function's type rather than hidden behind exceptions or global state.

Bindings use `let`:

```khora
let total = subtotal + tax;
```

Khora favors immutable values. Mutation and sharing exist, but the language makes them explicit so most code can be reasoned about as transformations from input values to output values.

## Function types matter

A signature communicates more than argument and return types. As you progress through the Guide you will see signatures carry typed failure and capabilities as well:

```khora
fn load_user(id: Id) -> User
  with { db: Db }
  raises DbError
```

Read that as: given an `Id`, this function produces a `User`, requires database authority, and may fail with `DbError`.

That information is checked by the compiler and is a central part of Khora's programming model.
