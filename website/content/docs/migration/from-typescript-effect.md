---
title: From TypeScript + Effect
sidebar:
  order: 1
---

If you know Effect, Khora's motivation should feel familiar: failures and capabilities belong in types, resource lifetimes should be structured, and concurrent work should have ownership.

The major difference is where that model lives. Khora builds those ideas into a native language/runtime rather than expressing them as a TypeScript library over the JavaScript/Node execution model.

## Where the familiar pieces live

| Effect | Khora |
| --- | --- |
| `Effect<A, E, R>` | a function's return type, `raises` row and `with` row |
| `Effect.either` | [`attempt`](/docs/reference/failures/#attempt) |
| `Redacted` | `std::core::Redacted` — same idea, and `Show`/`Encode` make it a compile error rather than a convention: a record holding one derives `Decode` and refuses `Encode`, where Effect's `Schema.Redacted` round-trips the secret on its encode side by design |
| `Config` | [`std::config::read`](/docs/cookbook/configuration/) over a `Schema<A>`, which `derive(Decode)` writes from the type; see below |
| `Schedule`, `retry`, `repeat` | [`std::resilience`](/docs/cookbook/retrying/) |
| `Clock.sleep`, `TestClock` | `std::clock::Clock` — `sleep` is an operation on the capability, so a fake clock is a handler and needs no fork-or-deadlock caveat |
| `Queue` with `dropping`/`sliding` | [`Channel::dropping` / `Channel::sliding`](/docs/reference/sharing/#what-a-full-channel-does) |
| `Fiber.join` | `Fiber::join`, which re-raises the child's failure with its type |
| `Effect.forkScoped` | `nursery.adopt(Fiber::spawn(..))` |
| `Ref` | `Shared<A>` |
| `Layer` | nothing — see below |

Some things are deliberately absent. `Effect.gen`, `pipe`, dual APIs, branded types and `Match` are TypeScript workarounds; Khora has methods, nominal types, `match`, and `let`.

## Direct style

`Effect.gen` and `yield*` disappear. A function calls effectful operations
directly, inside the capability and failure context its own signature declares
— there is no `Effect<A, E, R>` value to construct and compose:

```typescript
// Effect
const loadUser = (id: Id) =>
  Effect.gen(function* () {
    const db = yield* Database;
    const row = yield* db.query(id);
    return User.from(row);
  });
```

```khora
// Khora
fn load_user(id: Id) -> User with { db: Db } raises UserError {
  User::of(db.query(id)!)
}
```

`raises` is the typed failure dimension, `with` is environmental authority, and
postfix `!` is where control can leave — the marks `yield*` was carrying. A
*handler* is what supplies a capability for a scope; it is the value a `with`
block binds.

## The rows are named the other way round

`Effect<A, E, R>` calls them Errors and Requirements. Khora writes them as two separate rows, and the conventional names are two letters each so you cannot read them backwards:

```khora
fn call<A, 'ef, 'er>(body: () -> A with 'ef raises 'er) -> A
  with 'ef
  raises 'er
```

`'ef` is the capability row — Effect's `R`. `'er` is the failure row — Effect's `E`. A single-letter `'e` was ambiguous in exactly the direction that hurts somebody arriving from Effect, which is why it is not spelled that way.

## No `Layer`, and that is the point

There is no `Layer`. A capability is built by an ordinary binding in the `with`
block, once, in the order written — so there is nothing to memoize, and no way
to get two structurally identical instances of the same dependency:

```khora
with {
  config: env_config(),
  scope: Scope::root(),
  db: postgres_db()!,
  store: sql_store(),
} {
  run_server()!
}
```

Built once, in the order written, by the ordinary rules of a binding. There is nothing to memoize because there is nothing being rebuilt.

## No `Config<A>` description type either

`std::config::read(schema)` reads the whole configuration at once. It walks a `Schema<A>` — the same one that reads a JSON body — for the variables it needs, and answers `Validated`, so every bad key is reported in one pass and each is spelled as the variable it came from.

There is no description type to defer the read, because the `Env` handler is already what a test swaps. The schema is there for reuse rather than for deferral.

## Interruption is not only at effect boundaries

In Effect, a tight loop inside one synchronous step cannot be interrupted. Khora separates scheduler safepoints from cancellation points and emits a safepoint at every loop back-edge of a function that can raise, so a spinning loop in a fallible function is still cancellable. A function with no `raises` row has no cancellation point at all: the row is the channel a cancellation travels on.

## Pipelines

Khora's `|>` is call-oriented. `value |> f(a)` means `f(value, a)`, and a single `_` placeholder can select another argument position. It is not limited to piping into unary functions.
## Visibility is `pub`, not `export`

The one place the surface will read as *less* familiar than you expect.
`import` is spelled the way you already spell it, but what marks a declaration
public is `pub`:

```khora
pub type Entry = { id: Int, memo: String };

pub fn total(entries: List<Entry>) -> Int
```

It also appears on methods, which is why it is not `export`: nobody imports
`Map::get`, they reach it by having a `Map`. A declaration without `pub` is
private to its file, which is closer to a module with no `export` than to
TypeScript's default of exporting whatever is written at the top level.

