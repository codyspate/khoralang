---
title: Pattern matching
sidebar:
  order: 4
---

Patterns let Khora inspect algebraic data types and destructure values without unchecked casts or nullable sentinel conventions. They appear in `match`, `let`, `for`, `catch`, and other binding positions.

## Match variants exhaustively

```khora
pub type Status =
  | Pending
  | Complete(value: String)
  | Failed(message: String);

let message = match status {
  Status::Pending => "waiting",
  Status::Complete(value) => value,
  Status::Failed(reason) => "failed: ${reason}",
};
```

The compiler checks exhaustiveness and unreachable arms. If a new variant is added later, exhaustive matches identify the places that need a new decision.

## Wildcard pattern `_`

Use `_` when the value in that position is intentionally ignored:

```khora
let complete = match status {
  Status::Complete(_) => true,
  _ => false,
};
```

Prefer explicit variant arms when each case has meaningful semantics. A wildcard is most useful when the remaining cases genuinely have the same behavior.

## Binding patterns

A bare name binds the matched value:

```khora
let description = match status {
  Status::Complete(value) => value,
  Status::Failed(message) => message,
  other => "not finished",
};
```

Inside constructor patterns, names bind payload values automatically.

## Literal patterns

Literal values can be matched directly:

```khora
let label = match code {
  200 => "ok",
  404 => "not found",
  _ => "other",
};
```

String, numeric, boolean, and other supported literal forms can appear where the pattern grammar accepts them.

## Tuple patterns

Destructure tuples positionally:

```khora
let (width, height) = dimensions;

let description = match point {
  (0, 0) => "origin",
  (x, 0) => "x axis at ${Int::to_string(x)}",
  (x, y) => "point",
};
```

## Constructor patterns

Variant payloads can be positional or named by the type definition. Constructor patterns use the constructor path followed by their payload pattern:

```khora
pub type Message =
  | Text(String)
  | Move(Int, Int)
  | Quit;

match message {
  Message::Text(text) => print(text),
  Message::Move(x, y) => move_to(x, y),
  Message::Quit => stop(),
}
```

## Record patterns

Record-shaped values can bind selected fields by name:

```khora
let User { id, name } = user;
```

Use `field: pattern` when the binding name or nested pattern should differ from the field name:

```khora
let User { id: user_id, name } = user;
```

Patterns can nest when the underlying data nests, but shallow patterns are usually easier to read.

## Match guards

Add `if` after a pattern when the shape alone is not enough:

```khora
let category = match result {
  Result::Ok(value) if value > 100 => "large",
  Result::Ok(_) => "normal",
  Result::Err(error) => "failed",
};
```

A guard is evaluated only after its pattern matches.

## Destructuring in `let`

Use an irrefutable pattern in `let` when the type guarantees the shape:

```khora
let (first, second) = pair;
```

A refutable shape belongs in `match` instead, where all possible alternatives can be handled.

## Patterns in `for`

The binding side of `for` is also a pattern:

```khora
for (key, value) in entries {
  print("${key}: ${value}");
}
```

## Patterns in `catch`

`catch` uses the same pattern vocabulary to select typed failures:

```khora
let user = load_user(id)! catch {
  DbError::NotFound(_) => User::guest(),
  DbError::Unavailable(reason) => User::offline(reason),
};
```

Unlike an ordinary `match`, `catch` also changes the surrounding failure row: a fully handled failure type no longer propagates. See [Typed failure with raises](./errors-and-raises/).