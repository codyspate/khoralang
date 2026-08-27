---
title: Collections and strings
sidebar:
  order: 14
---

Khora's standard library provides collection types and Unicode strings while the compiler/runtime optimize ownership and reuse where they can prove it is safe. At source level, ordinary collection code remains value-oriented.

## List literals

Square brackets create a `List`:

```khora
let numbers = [1, 2, 3, 4];
let names = ["Ada", "Grace", "Linus"];
let empty: List<Int> = [];
```

A trailing comma is allowed in multiline literals:

```khora
let names = [
  "Ada",
  "Grace",
  "Linus",
];
```

Use higher-order operations when the result is another collection:

```khora
let doubled = numbers
  |> List::map(fn value => value * 2);
```

An effectful or fallible function can be mapped directly because function types carry their own capability and failure rows:

```khora
let users = ids
  |> List::map(fn id => load_user(id)!);
```

That form propagates the first `UserError`. To process every element and retain each failure as data, convert the failure channel with `attempt`:

```khora
let results = ids
  |> List::map(fn id =>
    attempt(fn () => load_user(id)!)
  );
```

See [Typed failure with raises](/docs/guide/errors-and-raises/#collect-failures-as-values-with-attempt).

## `for` over a collection

Use `for` when the body executed for each item is more important than producing another collection:

```khora
for name in names {
  print(name);
}
```

The left side is a pattern, so destructuring works there too.

## Quoted strings

Double quotes create a `String`:

```khora
let language = "Khora";
```

Standard escapes use `\`:

```khora
let two_lines = "first\nsecond";
let quoted = "say \"hello\"";
```

## String interpolation

`${...}` evaluates a Khora expression and inserts its string representation into the surrounding string:

```khora
let count = 3;
let message = "processed ${Int::to_string(count)} item(s)";
```

Interpolation works in both quoted and backtick strings.

Use interpolation for human-readable composition. Use structured encoders such as JSON when another program will consume the output.

## Multiline backtick strings

Use backticks for multiline text whose layout matters:

```khora
const SCHEMA: String = `
  create table if not exists entries (
    id serial primary key,
    memo text not null
  )
`;
```

When the opening delimiter is followed by a newline, the common indentation prefix of non-blank lines is removed. That lets the literal line up with surrounding source without baking that indentation into the string.

A backtick string otherwise behaves like an ordinary `String`: escapes still work, `${...}` still interpolates, and a literal backtick is escaped with `\``.

If content begins on the same line as the opening backtick, indentation stripping does not apply:

```khora
let raw = `first
  second`;
```

## Choosing a collection

`List<A>` is a linked sequence suited to ordered transformation and iteration. `Vector<A>` is a growable contiguous sequence suited to accumulation and indexing. `Map<K, V>` is a mutable hash table; `Dict<K, V>` is a persistent ordered map.

The exact operations for each type live in the [standard-library API reference](/docs/stdlib/api/core/). The language syntax is the same whichever collection type you use: normal calls, methods, pipelines, lambdas, `for`, and pattern matching.

## Sharing collections

Mutating containers such as `Vector` and `Map` stay fiber-local. When several fibers need coordinated evolving state, use an explicit `Shared` boundary or an external capability rather than hidden shared mutation. See [Shared state](/docs/guide/shared-state/).