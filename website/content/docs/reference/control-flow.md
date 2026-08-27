---
title: Control flow
sidebar:
  order: 5
---

Khora's control-flow forms are expressions or block-like expressions. This page records their accepted shapes and result rules.

## Blocks

```khora
{
  let value = compute();
  value + 1
}
```

General shape:

```text
{
  Statement*
  Expr?
}
```

The final expression, when present without a semicolon, is the block's value. A block with no final value produces `()`.

## `if`

```khora
if condition {
  when_true
} else {
  when_false
}
```

`else if` is nested `if` syntax:

```khora
if first {
  a
} else if second {
  b
} else {
  c
}
```

The condition must be `Bool`. When an `if` is used as a value, its branches must agree on a result type. An `if` without `else` must produce `()`.

## `match`

```khora
match value {
  Pattern => Expr,
  Pattern if guard => Expr,
}
```

An arm body may be a block:

```khora
match value {
  Result::Ok(item) => {
    log(item);
    item
  },
  Result::Err(error) => fallback(error),
}
```

Patterns are checked for exhaustiveness and reachability. See [Patterns](./patterns.md).

## Match guards

```khora
match score {
  value if value >= 90 => "high",
  value if value >= 50 => "medium",
  _ => "low",
}
```

The guard runs only after its pattern matches and must produce `Bool`.

## `while`

```khora
while condition {
  body;
}
```

The condition must be `Bool`.

## `for`

```khora
for pattern in iterable {
  body;
}
```

The left side is a pattern:

```khora
for (key, value) in entries {
  print(key);
}
```

## `loop`

```khora
loop {
  body;
}
```

A loop exits through `break`, `return`, `raise`, failure propagation, cancellation, or other non-local control flow.

## `break`

Without a value:

```khora
break;
```

With a value:

```khora
break result;
```

A `loop` with value-carrying `break` expressions produces the common type of those values:

```khora
let found = loop {
  let candidate = next();
  if candidate.valid {
    break candidate;
  }
};
```

## `continue`

```khora
continue;
```

`continue` skips the remainder of the current loop iteration.

## `return`

Return a value:

```khora
return value;
```

Return unit:

```khora
return;
```

A function normally returns its body's final expression; `return` is the explicit early-exit form.

## Typed failure control flow

Explicit raise:

```khora
raise UserError::NotFound(id)
```

Propagate a fallible call:

```khora
load_user(id)!
```

Handle a typed failure:

```khora
load_user(id)! catch {
  UserError::NotFound(_) => User::guest(),
  UserError::Unavailable(reason) => User::offline(reason),
}
```

`raise`, `!`, and `catch` participate in typed failure rows rather than ordinary function return. See [Failures](./failures.md).

## Capability installation as control scope

Postfix installation serves one expression:

```khora
load_user(id)! with {
  store: test_store,
}
```

Block installation serves a lexical region:

```khora
with Production {
  run_server()!
}
```

See [Capabilities](./capabilities.md) for context rows and overrides.