---
title: From TypeScript + Effect
sidebar:
  order: 1
---

If you know Effect, Khora's motivation should feel familiar: failures and capabilities belong in types, resource lifetimes should be structured, and concurrent work should have ownership.

The major difference is where that model lives. Khora builds those ideas into a native language/runtime rather than expressing them as a TypeScript library over the JavaScript/Node execution model.

## Visibility is `pub`, not `export`

The one place the surface will read as *less* familiar than you expect.
`import` is spelled the way you already spell it, but what marks a declaration
public is `pub`:

```khora
pub type Entry = { id: Int, memo: String };
pub fn total(entries: List<Entry>) -> Int { .. }
```

It also appears on methods, which is why it is not `export`: nobody imports
`Map::get`, they reach it by having a `Map`. A declaration without `pub` is
private to its file, which is closer to a module with no `export` than to
TypeScript's default of exporting whatever is written at the top level.

## Direct style

Khora code calls effectful operations directly inside the capability/failure context declared by the function. There is no separate `Effect<A, E, R>` value that application code must construct and compose.

`raises E` corresponds roughly to the typed failure dimension. A `with { ... }` capability row corresponds to environmental authority. Handlers provide capabilities for a scope.

## The rows are named the other way round

`Effect<A, E, R>` calls them Errors and Requirements. Khora writes them as two separate rows, and the conventional names are two letters each so you cannot read them backwards:

```khora
fn call<A, 'ef, 'er>(body: () -> A with 'ef raises 'er) -> A
  with 'ef
  raises 'er
```

`'ef` is the capability row — Effect's `R`. `'er` is the failure row — Effect's `E`. A single-letter `'e` was ambiguous in exactly the direction that hurts somebody arriving from Effect, which is why it is not spelled that way.

## Where the familiar pieces live

| Effect | Khora |
| --- | --- |
| `Effect<A, E, R>` | a function's return type, `raises` row and `with` row |
| `Effect.either` | [`attempt`](/docs/guide/errors-and-raises/#collect-failures-as-values-with-attempt) |
| `Redacted` | `std::core::Redacted` — same idea, and `Show`/`ToJson` make it a compile error rather than a convention |
| `Config` | [`std::config`](/docs/cookbook/configuration/) — but no `Config<A>` description type; see below |
| `Schedule`, `retry`, `repeat` | [`std::resilience`](/docs/cookbook/retrying/) |
| `Clock.sleep`, `TestClock` | `std::clock::Clock` — `sleep` is an operation on the capability, so a fake clock is a handler and needs no fork-or-deadlock caveat |
| `Queue` with `dropping`/`sliding` | [`Channel::dropping` / `Channel::sliding`](/docs/reference/sharing/#what-a-full-channel-does) |
| `Fiber.join` | `Fiber::join`, which re-raises the child's failure with its type |
| `Effect.forkScoped` | `nursery.adopt(Fiber::spawn(..))` |
| `Ref` | `Shared<A>` |
| `Layer` | nothing — see below |

Some things are deliberately absent. `Effect.gen`, `pipe`, dual APIs, branded types and `Match` are TypeScript workarounds; Khora has methods, nominal types, `match`, and `let`.

## No `Layer`, and that is the point

Effect v3's layer memoization could silently build an overlapping layer twice, and v4 shipped what its own guide calls a safety net for the footgun. The residual issue survives, because the memo key is object identity: calling a layer factory twice gives you two structurally identical layers built separately.

In Khora that is a `let` binding:

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

The Effect version of typed configuration is a *description* — a value denoting "read `PORT` as an integer", interpreted later by a provider so a test can swap it. The whole description layer exists to defer the read.

Khora's provider is the `Env` handler and it was already swappable, so `std::config`'s readers are plain functions whose row says they read from something. What the description bought — typed parsing, composition, error accumulation — comes from the return type instead: every reader answers `Validated`, and `map2` reports every bad key in one pass.

## Interruption is not only at effect boundaries

In Effect, a tight loop inside one synchronous step cannot be interrupted. Khora separates scheduler safepoints from cancellation points and emits a safepoint at every loop back-edge, so a spinning loop is still cancellable. It looks like an implementation detail and it is not.

## Pipelines

Khora's `|>` is call-oriented. `value |> f(a)` means `f(value, a)`, and a single `_` placeholder can select another argument position. It is not limited to piping into unary functions.

## Fibers and scopes

Khora fibers and nurseries provide structured concurrency in the language/runtime. Cancellation and finalization remain central concepts, but source code stays ordinary direct-style Khora.

## Memory/runtime

Khora compiles to native code and does not use a tracing GC or JavaScript VM. Automatic memory management is implemented through reference counting plus compiler ownership/reuse analysis rather than through a source-level borrow checker.
