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

The wildcard matches a value without binding it. Prefer an explicit arm per
variant where the cases mean different things: a wildcard earns its place when
the remaining cases genuinely share a behavior, and costs something when they
do not, because it is the one arm a newly added variant will silently fall
into.

## Binding identifier

```khora
value
```

**A bare identifier binds the matched value.** `value => ..` matches anything and names it `value`.

**A bare identifier that is the name of one of the value's cases is an error.** Over a `Color`, `Red => ..` would bind every color, not match `Color::Red`, so the compiler refuses it and names the pattern to write. This applies in every pattern (`match`, `catch`, `let`, `for`, and nested inside others) and to cases with a payload as well: `NotFound => ..` over an `FsError` is refused too. **Write a constructor qualified** (`Color::Red`, `FsError::NotFound(path)`), and bind with a name that is not a case.

The check is against the value's type, not against what the file imports, so it applies to a value whose type the file never names. If a case is added to a type later, a binding that shares its name stops compiling. It does not silently start matching only that case.

**A capitalized bare name is an error too**, when it is no case of the
value's type. A name in a pattern that starts with a capital letter must be a
case, so `Gren => ..` over a `Color` is refused, and the message offers the
nearest case, `Color::Green`, when one is within two edits (or a third of the
name's length, if that is more). A `const` written as a pattern,
`FAVORITE => ..`, would bind every value rather than compare against the
constant; compare with a guard instead:

```khora
match n {
  n if n == FAVORITE => "lucky",
  _ => "ordinary",
}
```

Bind with a lower-case name.

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

The compiler checks exhaustiveness and unreachable arms. Adding a variant to a
type therefore turns every exhaustive match on it into a compile error, which
is the list of places that need a decision about the new case.

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
  print("${entry.key}: ${entry.value}");
}
```

The pattern binds each yielded item, under the same rules as `let`: valid for the item's type, and irrefutable. `Dict::entries` yields a `Pair`, which is a record — a tuple pattern such as `(key, value)` is only valid where the item's type is a tuple, and is refused otherwise.

`for` needs `Step` and `Iterator` in scope, and `Pair` too if the body reads the item's fields: `import std::core::{Dict, Iterator, Pair, Step, print};`. See [Control flow](./control-flow/#for).

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