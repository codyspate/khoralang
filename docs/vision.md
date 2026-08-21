# Vision

Why Khora exists. Read this before `roadmap.md` — when a sequencing or design
call is ambiguous, this document breaks the tie.

## The thesis

Rust has the better developer experience. Effect (TypeScript) has the better
functional programming model. Nothing has both.

Khora aims to **meet Rust's developer experience and improve on it**, and to
**meet Effect's functional capabilities and improve on them**.

The bar for success: Khora should be a serious candidate any time a team is
choosing between **Rust, Go, and TypeScript/Node/Bun** for a new project.

## The gap we are aiming at

Effect proves that typed effects, capability-based dependency injection and
structured concurrency are what application code actually wants. But it is
implemented in TypeScript, which imposes three costs that are not inherent to
the ideas:

1. **The abstractions are simulated — twice over.** TypeScript has no
   higher-kinded types, so Effect encodes them with `TypeLambda`/`Kind`
   defunctionalisation; that is the main source of its unreadable types and
   hostile error messages. TypeScript also has no effect handlers, so
   `Effect.gen` and `yield*` exist to fake direct-style code. Both are
   workarounds for missing language features, not properties of the model.
2. **The runtime is a VM.** Every abstraction is paid for at runtime, in a
   garbage-collected interpreter.
3. **The tooling is the npm ecosystem.** Slow, fragmented, and split between
   tools that are fast-but-incomplete and complete-but-slow.

Rust has none of those problems and cannot express the ideas. It has no
higher-kinded types, therefore no `Monad`, no generic `traverse`, and no way to
abstract over effectful containers. Dependency injection is ad hoc.

Khora's premise is that **all three of Effect's costs are TypeScript's, not the
model's** — and that the model is exactly what Rust cannot reach.

## Where we improve on each

**Against Effect:**

| Axis | Effect | Khora |
| --- | --- | --- |
| Abstraction | HKT simulated via defunctionalisation | Native `* -> *` and kind inference |
| Sequencing | `Effect.gen`/`yield*` faking direct style | Real algebraic effects and handlers |
| Runtime | GC'd VM | Native static binary, Perceus RC, no tracing GC |
| Tooling | npm tool sprawl | One static binary: compiler, package manager, fmt, lint, test, LSP |
| Errors | Notoriously hard to read | A first-class, tested concern (see below) |

**Against Rust:**

| Axis | Rust | Khora |
| --- | --- | --- |
| Abstraction | No HKT; GATs are a partial workaround | Native HKT and typeclasses |
| Effects | Untracked | Typed effect and capability rows |
| Dependency injection | Ad hoc, per-framework | `Layer` and capability rows in the type system |
| Errors as data | `Result` plus the `?` operator | Typed, open, composable error channels |

Rust's developer experience — cargo, diagnostics, rust-analyzer, clippy — is not
something we improve on by accident. It is the floor, and matching it is a
requirement, not a stretch goal.

## Non-negotiables

These follow from the thesis. Trading any of them away means we are building a
different language.

1. **Higher-kinded types are core, not optional.** They are what Rust
   structurally cannot express, and they carry `Traversable`, `Stream`, generic
   combinators and user abstractions. Note that direct-style effects
   deliberately reduce the need for `Monad` specifically — there is no monadic
   plumbing left to abstract over — so HKT is justified by containers and
   typeclasses, not by the effect system.
2. **Direct-style algebraic effects.** Effects are rows on the signature and are
   discharged by handlers, not threaded through combinator chains. This is the
   known improvement over monadic effect systems, and it matches the Perceus and
   scoped-row substrate the compiler is already built on — both of which come
   from Koka, which pairs them with exactly this model.
3. **Structured concurrency with interruption.** Effect's headline safety
   property. Fibers, cancellation that runs finalizers, `Scope`-bound resource
   lifetimes, and `Schedule` policies.
4. **Developer experience is the product, not the polish.** Diagnostics quality,
   compile speed and LSP latency are requirements tested from the first working
   compiler, not a phase at the end. We are explicitly competing with the best
   toolchain in the industry.
5. **Zero VM, zero tracing GC.** Native static binaries. The abstractions must
   be cheap enough that nobody chooses Go or Rust over Khora for performance.
6. **An ecosystem on day one.** A new language with no libraries loses to Go and
   Node regardless of merit. First-class Rust interop is how we refuse that
   fight — crates.io is native, LLVM-based, and the closest ecosystem to reach.

## The tie-breaker

The non-negotiables say what Khora must do. This says how to settle everything
else.

**Where a design decision could reasonably go either way, choose the option more
familiar to a developer who uses Go, Rust or TypeScript.**

Those three are the competitive set named at the top of this document, and most
of that audience are not functional programmers. When two spellings, two
resolution rules or two defaults are genuinely close on the merits, the one they
already know wins. This is not deference to fashion. Unfamiliarity is a cost paid
at every use site by every reader, forever; "I prefer the other one" costs
nothing and buys nothing. The rule exists so that decisions of this kind are
settled by argument rather than by taste.

It is a **tie-breaker, not an override.** It applies only where the options are
close. Where Khora is deliberately doing something none of the three can do —
effect rows, higher-kinded types, handlers — the non-negotiables decide and this
rule is silent. Those features are unfamiliar by construction, and that is the
whole reason for building the language.

It has already settled several calls:

- `::` for compile-time paths and `.` for runtime projection, rather than the
  specification's universal dot (`docs/errata.md`, entry 13).
- No uniform function call syntax: `x.f()` finds a field of `x` or an item
  declared against `x`'s type, and nothing else
  (`docs/design/associated-items.md`).
- `if`, `while`, assignment and early `return`, rather than expressing every
  loop as a fold (`docs/design/imperative.md`).
- `!` on calls that can abort, because `?` and `try` have taught this audience to
  expect a mark where control leaves (`docs/design/effects.md`).

## Non-goals

- **Not a systems-programming replacement for Rust.** Reference counting has a
  cost, and we are not targeting kernels, drivers or hard real-time.
- **Not a garbage-collected language.** Ever.
- **Not source-compatible with anything.** Effect users should find Khora
  familiar, but we are not bound to TypeScript's spellings where we can do
  better.
- **Not a research language.** Every type-system feature has to earn its place
  by making application code better, not by being novel.

## How we will know it is working

Concrete, falsifiable checks, in rough order of when they become answerable:

- A generic `traverse` written once works over `Option`, `List` and `Stream`.
  (Rust cannot express this; Effect needs `@effect/typeclass` to fake it.)
- The reference risk-analysis function reads as straight-line code with a `with`
  clause, with no `flat_map` and no nested closures.
- A cancelled fiber runs every finalizer in scope, verified by test.
- Cold `khora build` on the reference application beats `cargo build` on an
  equivalent Rust program.
- LSP hover and completion respond in under 15 ms on a real workspace.
- A missing capability names the absent label and the function that needed it,
  in one screen, without printing a simulated type constructor.
- An HTTP server with real TLS and JSON can be written using Rust crates through
  the interop boundary.
