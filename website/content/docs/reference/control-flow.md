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

Patterns are checked for exhaustiveness and reachability. See [Patterns](./patterns/).

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

`for` walks a `List`, a `Range`, a `Vector`, an `Array`, a `Map`, a `Dict`, and
anything else that implements `Iterator` or `Iterable`:

| Walked | Yields | In this order |
|---|---|---|
| `List<A>` | each element | first to last |
| `Range::Of(from, to)` | each `Int` from `from` up to, not including, `to` | ascending |
| `Vector<A>`, `Array<A>` | each element | index order |
| `Dict<K, V>` | a `Pair<K, V>` per entry | smallest key first, as `Dict::entries` |
| `Map<K, V>` | a `Pair<K, V>` per entry | the order `Map::entries` reports, entry for entry; it follows no rule and can differ between runs |

A loop over a `Vector`, `Array`, `Map` or `Dict` walks it where it is. It does not
copy it into a `List` first, so it allocates the same small amount however long the
collection is.

The left side is a pattern. Khora has a tuple literal — see
[Tuple and unit expressions](/docs/reference/expressions/) — but no standard
iterator yields one: a map yields a `Pair` record, so the loop binds it by name
and reads its fields, or takes it apart with a record pattern.

```khora
for entry in table {
  print("${entry.key}: ${entry.value}");
}

for Pair { key, value } in table {
  print("${key}: ${value}");
}
```

**`Pair<K, V>` has the fields `key` and `value`**, and `String::split_once`
answers a `Split` with the fields `head` and `rest`. Both are ordinary records:
they are read with `.`, and written as record literals — `let p: Pair<String,
Int> = { key: "a", value: 1 };`. Reading `entry.key` needs `Pair` imported.

A tuple pattern — `for (key, value) in ...` — is only valid against a value
whose type really is a tuple, and is refused against a `Pair`:

```
error: this pattern takes a value apart into 2 pieces, but `Pair<String, Int>`
       is not a tuple
```

`for` desugars to `Iterator::next`, so both `Iterator` and `Step` must be in scope in the module that writes the loop, whatever it walks:

```khora
import std::core::{Iterator, Step};
```

### Changing the collection inside the loop

The loop reads each element when it reaches it, from the collection itself.
What it sees of changes its own body makes:

- **`Vector`**: the loop visits index 0, 1, 2 and on while the index is below
  both the length the loop began with and the length at that step, and reads
  each element when it reaches it. So an element pushed onto the end is not
  visited and the loop ends, and a `pop` or `clear` ends the loop at the new
  end. A `pop` or `clear` followed by pushes puts new elements at indices the
  loop has not reached yet, and those are visited: popping and pushing on each
  turn of `[1, 2, 3, 4, 5]` visits `1, 2, 3, 4, 104`, and a `clear` then ten
  pushes on the first turn visits `1, 901, 902, 903, 904`. An element changed
  with `Vector::set` before the loop reaches it is visited with its new value.
  What is visited never depends on the vector's capacity.
- **`Array`**: an element changed with `Array::set` before the loop reaches it
  is visited with its new value.
- **`Map`**: every entry that is in the map for the whole loop is visited exactly
  once, even if the body inserts enough to grow the table. An entry inserted or
  removed during the loop may or may not be visited; an entry whose value is
  replaced is visited once, with the old value or the new.
- **`Dict`**: a `Dict` is never changed in place, so the loop walks the
  dictionary it started with. `d = d.insert(..)` in the body makes a new one.

None of these stops the program, runs forever, or visits one element twice.

### Something that is not walkable

```
error: a `for` walks an `Iterator` or an `Iterable`, and `Int` is neither.
       `List`, `Range`, `Vector`, `Array`, `Map` and `Dict` are, and a type of
       your own is once it implements `Iterator` (it is its own cursor) or
       `Iterable` (it hands out a cursor)
```

A type of your own is walkable once it implements one of those two traits. See
[Traits](/docs/reference/traits/) for `Iterable`.

A tuple is taken apart by pattern and never by position: there is no `pair.0`.
Destructure it in the `for`, in a `let`, or in a match arm.

```khora
let point = (3, 4);
let (x, y) = point;
```

Reach for a transform such as `List::map` when the result is another
collection, and for `for` when the point is the body run once per element.

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

A function normally returns its body's final expression; `return` is the
explicit early-exit form. Prefer the final expression where it stays clear, and
`return` where an early exit is what makes the control flow readable.

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

`raise` is not `return`. It leaves through the function's typed failure
channel, which is a second exit the signature declares separately; `!`
propagates that channel and `catch` handles it before it reaches the caller.
See [Failures](./failures/).

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

See [Capabilities](./capabilities/) for capability rows and overrides.