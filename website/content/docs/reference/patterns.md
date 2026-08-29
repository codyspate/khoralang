---
title: Patterns
sidebar:
  order: 6
---

Patterns appear in `match` and `catch` arms, local destructuring, `for` bindings, and other positions that bind or inspect a value.

## Wildcard

```khora
_
```

The wildcard matches a value without binding it.

## Binding identifier

```khora
value
```

A bare identifier normally binds the matched value. A bare identifier that resolves to a nullary constructor is treated as that constructor rather than as a new binding.

## Literal patterns

```khora
0
3.14
"ready"
true
false
```

Integer, floating-point, string, and boolean literals can be used as literal patterns.

## Nullary constructor path

```khora
Option::None
Status::Ready
```

A qualified path selects the named constructor.

## Constructor payload pattern

```khora
Option::Some(value)
Result::Err(error)
Message::Move(x, y)
```

The patterns inside parentheses correspond to the constructor's payload positions.

## Record pattern

Shorthand field binding:

```khora
User { id, name }
```

Explicit nested pattern:

```khora
User {
  id: user_id,
  name: "admin",
}
```

General shape:

```text
Path {
  field,
  field: Pattern,
  ...
}
```

A record pattern begins with a path and may bind fields by shorthand or supply another pattern after `:`.

## Tuple pattern

```khora
(left, right)
(x, y, z)
```

Tuple patterns may nest other patterns:

```khora
(Result::Ok(value), _)
```

## Patterns in `match`

```khora
match result {
  Result::Ok(value) => use_value(value),
  Result::Err(error) => handle(error),
}
```

The compiler checks exhaustiveness and unreachable arms.

## Match guards

A guard belongs to the arm after the pattern:

```khora
match score {
  value if value >= 90 => "high",
  value if value >= 50 => "medium",
  _ => "low",
}
```

General form:

```text
Pattern if BoolExpr => Expr
```

The guard runs only after the pattern itself matches.

## Patterns in `let`

```khora
let (left, right) = pair;
let User { id, name } = user;
```

A pattern used directly by `let` must be valid for the value's type without requiring a missing-case branch. Refutable alternatives belong in `match`.

## Patterns in `for`

```khora
for entry in Dict::entries(table) {
  use_entry(entry.key, entry.value);
}
```

The pattern binds each yielded item, under the same rules as `let`: valid for the item's type, and irrefutable. `Dict::entries` yields a `Pair`, which is a record — a tuple pattern such as `(key, value)` is only valid where the item's type is a tuple, and is refused otherwise.

## Patterns in `catch`

```khora
load_user(id)! catch {
  UserError::NotFound(missing_id) => fallback(missing_id),
  UserError::Unavailable(reason) => offline(reason),
}
```

`catch` reuses the pattern syntax but adds failure-row semantics: exhaustively handling a failure type removes that type from the failures that can leave the expression.

## Nesting

Patterns compose recursively:

```khora
match value {
  Envelope {
    payload: Result::Ok(User { id, name }),
  } => use_user(id, name),
  _ => fallback(),
}
```

Use nesting when it makes the shape clearer; split deeply nested business decisions into smaller matches when a single arm becomes difficult to read.

See [Control flow](./control-flow/) for `match` result rules and [Failures](./failures/) for `catch` semantics.