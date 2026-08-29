---
title: Control flow
sidebar:
  order: 2
---

Khora is expression-oriented: blocks, `if`, `match`, and loops participate in ordinary expression-oriented code rather than living in a separate statement language.

## Blocks produce their final expression

A block evaluates statements in order and then produces its final expression:

```khora
let total = {
  let subtotal = 40;
  let tax = 2;
  subtotal + tax
};
```

The final expression has no semicolon. Adding a semicolon turns it into a statement, so the block no longer returns that value.

## `if` and `else`

Use `if` for conditional control flow:

```khora
let label = if score >= 90 {
  "excellent"
} else if score >= 70 {
  "good"
} else {
  "needs work"
};
```

When an `if` is used as a value, its branches must agree on the result type. An `if` without `else` is useful for side-effecting or mutating work and must produce `()`:

```khora
if should_log {
  print("starting request");
}
```

## `match`

Use `match` when behavior depends on the shape of a value:

```khora
let message = match result {
  Result::Ok(value) => "value: ${value}",
  Result::Err(error) => "failed: ${error}",
};
```

Match arms can use guards:

```khora
let category = match score {
  value if value >= 90 => "high",
  value if value >= 50 => "medium",
  _ => "low",
};
```

The compiler checks pattern exhaustiveness and unreachable arms. See [Pattern matching](/docs/guide/pattern-matching/) for all pattern forms.

## `while`

Use `while` when repetition is controlled by a condition:

```khora
let mut attempts = 0;

while attempts < 3 {
  attempts = attempts + 1;
}
```

## `for ... in ...`

Use `for` to consume values from an iterable source:

```khora
for user in users {
  print(user.name);
}
```

The left side is a pattern, so destructuring is allowed when the iterator's item shape is known. `Dict::entries` yields a `Pair`, which is a record, so its halves are reached by name:

```khora
for entry in Dict::entries(table) {
  print("${entry.key}: ${entry.value}");
}
```

Khora has no tuple literal, so `for (key, value) in ...` only works where the item's type genuinely is a tuple. Against anything else it is refused, naming the type it found.

`for` is desugared to the `Iterator` trait's `next`, which hands back the next item and the iterator that follows it. Both `Iterator` and `Step` have to be in scope where the loop is written:

```khora
import std::core::{Iterator, List, Step};
```

Use collection transforms such as `List::map` when the result is another collection; use `for` when the important part is the body executed for each element.

## `loop`

Use `loop` for repetition whose exit is expressed inside the body:

```khora
let mut next = 0;

loop {
  if next >= limit {
    break;
  }

  next = next + 1;
}
```

A `loop` can also produce a value through `break value`:

```khora
let answer = loop {
  let candidate = next_candidate();

  if candidate.is_valid() {
    break candidate;
  }
};
```

All value-carrying `break` paths in the same loop must agree on a type.

## `continue`

`continue` skips the rest of the current iteration:

```khora
for item in items {
  if item.should_skip() {
    continue;
  }

  process(item);
}
```

## `return`

A function normally returns its block's final expression. Use `return` for an explicit early exit:

```khora
fn classify(value: Int) -> String {
  if value < 0 {
    return "invalid";
  }

  "valid"
}
```

`return` may omit a value in a function returning `()`:

```khora
fn maybe_print(enabled: Bool) -> () {
  if !enabled {
    return;
  }

  print("enabled");
}
```

## Failure is separate control flow

`raise` is not the same thing as `return`: it leaves through the function's typed failure channel. `!` propagates that channel and `catch` can handle it before it reaches the caller.

```khora
let user = load_user(id)! catch {
  DbError::Rejected(_) => User::guest(),
  DbError::Disconnected(_) => User::offline(),
  DbError::RolledBack(_) => User::offline(),
};
```

See [Typed failure with raises](/docs/guide/errors-and-raises/) for the full failure model.