---
title: Values and functions
sidebar:
  order: 1
---

Khora is expression-oriented and statically typed. Most local types are inferred, while public APIs and important boundaries can state their types explicitly.

## Local bindings with `let`

Use `let` to bind a value:

```khora
let name = "Khora";
let retries: Int = 3;
let enabled = true;
```

Bindings are immutable by default. The annotation after `:` is optional when the compiler can infer the type.

Use destructuring when the shape is already known:

```khora
let (left, right) = pair;
```

More pattern forms are covered in [Pattern matching](./pattern-matching.md).

## Explicit mutation with `let mut`

When a local value really needs to change, opt in with `mut` and assign with `=`:

```khora
let mut attempts = 0;

while attempts < 3 {
  attempts = attempts + 1;
}
```

A normal `let` binding cannot be assigned to later. Shared mutable state uses a separate `Shared` boundary; see [Shared state](./shared-state.md).

## Module constants with `const`

Use `const` for a named module-level value whose initializer can be treated as a constant expression:

```khora
const DEFAULT_PORT: Int = 8080;
const SERVICE_NAME = "orders";
```

A constant is different from a local `let`: it is declared at module scope and can be referenced by other declarations in that module.

Add `pub` when callers in other modules should be able to use it:

```khora
pub const DEFAULT_PAGE_SIZE: Int = 50;
```

Use `const` for genuine constants such as limits, protocol values, and fixed configuration defaults. Use `let` for values computed as part of running a function.

## Visibility with `pub`

Top-level declarations are private to their module unless they are marked `pub`:

```khora
fn normalize(raw: String) -> String {
  raw |> String::trim
}

pub fn parse_name(raw: String) -> String {
  normalize(raw)
}
```

The same `pub` modifier is used for public types, constants, effects, traits, contexts, and other exported declarations.

## Functions

A function declaration starts with `fn`, followed by the name, parameters, optional return type, optional capability/failure clauses, and body:

```khora
fn double(n: Int) -> Int {
  n * 2
}
```

A block evaluates to its final expression, so no `return` is needed for the common case:

```khora
fn greeting(name: String) -> String {
  let normalized = name |> String::trim;
  "Hello, ${normalized}"
}
```

Parameters may omit annotations when another type context determines them, although named functions normally benefit from explicit parameter and return types at their boundary.

A function that produces no meaningful value can return unit `()`:

```khora
fn record_success() -> () {
  ()
}
```

## Early `return`

Use `return` when control should leave before the block's final expression:

```khora
fn label(score: Int) -> String {
  if score < 0 {
    return "invalid";
  }

  if score >= 90 {
    "excellent"
  } else {
    "standard"
  }
}
```

Prefer the final-expression style when it stays clear; use `return` when an early exit makes the control flow easier to read.

## Functions are values

Functions can be passed to higher-order APIs directly:

```khora
fn normalize(value: String) -> String {
  value |> String::trim |> String::lower
}

let names = raw_names |> List::map(normalize);
```

Function types use `->`:

```khora
fn apply_twice<A>(value: A, f: A -> A) -> A {
  value |> f |> f
}
```

Function types can also carry `with` and `raises` rows when the function value requires capabilities or may fail.

## Lambdas

Use `fn ... => ...` for an anonymous function. A single parameter can be written without parentheses:

```khora
let doubled = values |> List::map(fn value => value * 2);
```

Use parentheses for multiple parameters or when annotating them:

```khora
let add = fn (left: Int, right: Int) => left + right;
```

A lambda can have a block body:

```khora
let describe = fn value => {
  let doubled = value * 2;
  "${Int::to_string(value)} -> ${Int::to_string(doubled)}"
};
```

For unary transformation lambdas written as a pipeline, Khora also provides `||>`; see [Pipelines](./pipelines.md#the-flow-operator).

## Capabilities and failures are part of a function type

A signature may state required authority with `with` and recoverable failure with `raises`:

```khora
pub fn load_user(id: Id) -> User
  with { db: Db }
  raises DbError
{
  db.load_user(id)!
}
```

Read that as: given an `Id`, the function produces a `User`, requires the `db` capability, and may fail with `DbError`.

Public signatures make those requirements explicit. Private helpers can often let the compiler infer them. Continue with [Typed failure with raises](./errors-and-raises.md) and [Effects and capabilities](./effects-and-capabilities.md) for the complete syntax.