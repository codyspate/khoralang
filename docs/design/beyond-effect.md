# Where Khora can pass Effect, not just match it

**Status: a list of opportunities, none of them built.** `docs/vision.md` sets
the bar as "meet Effect's functional capabilities and improve on them". Meeting
is most of the work and it is legible: the mapping in
`docs/design/std-admission.md` says which Effect module has a Khora answer.
Passing is a different question, and it has a single test.

> **What can Khora do because effects are in the language and the compiler owns
> the whole program, that Effect cannot do because it is a library on a VM?**

Anything that fails that test is a feature request. Anything that passes is a
reason to exist.

## Already past it

Worth stating, because two of these are built and neither is being sold.

**Inlay hints show the rows.** `khora-lsp`'s `hints.rs` renders the capability
row and the error row at the call site:

```khora
let answer = charge(account, amount);   // with { db: Db, clock: Clock } raises DbError
```

Every other language's inlay hints show inferred types. Khora infers two more,
and they are the ones a reader is missing. The same information in Effect lives
inside a nested generic and is the source of its worst error messages.

**Effect rows are monomorphised whole-program.** `effect-runtime.md`: a row
variable is concrete at every call site. Effect builds a data structure and
interprets it; Khora resolves it statically. "Effects are close to free in a
release build" is a claim Effect can never make.

**A static binary with no VM.** 3.6 MB, 8.4 MB resident serving load, against
Node's floor. Measured, in `bench/README.md`.

## 1. Build `Stream` fusible, because it is only designed once

Effect's `Stream` is a runtime interpreter: each `map` or `filter` stage is an
allocation and a dispatch. With whole-program monomorphisation and Perceus
reuse, `map |> filter |> fold` can lower to **one loop with no intermediate
allocation**, the way Rust's iterators do.

Rust has the codegen and not the ergonomics. Effect has the ergonomics and not
the codegen. Nothing has both, and fusion is an architectural property -- it
comes from designing for it, not from optimising later. A `Stream` built as a
closure of closures inherits Effect's ceiling permanently.

`Stream` is also named in non-negotiable #1 as one of the things higher-kinded
types are *for*, and it does not exist. This is the largest gap against the
thesis and the best showcase for the effect system.

## 2. Record every effect, and replay it

The idea with the widest gap between "Khora can" and "Effect cannot".

Every interaction with the outside world goes through a handler, and
`derive(Encode)` can serialise an operation's arguments and result. So a build
can wrap every handler, record `(operation, arguments, result)` in order, and a
replay handler can feed that log back and reproduce the run exactly.

**Effect structurally cannot.** A TypeScript program can always reach a raw
`fetch` or `fs.readFileSync`, so the log has holes and the replay diverges.
Khora's capability system is what closes them: if the manifest did not grant it,
the code cannot do it, so the recording is total.

"Download the effect log from the failing production request and replay it on a
laptop" is a debugging story no mainstream language has. It needs no new
machinery -- it composes effects, `Encode` and capabilities, all of which exist.

The open questions are size (a log is unbounded), redaction (`Redacted` and
`Shape::Secret` already exist and are the right hook), and what happens when
the program changes between record and replay.

## 3. Deterministic concurrency testing

Khora owns its scheduler; `khora-rt/src/scheduler.rs` has no seeding or replay
hook. A seeded scheduler that explores interleavings -- Loom, Shuttle, madsim --
would make `khora test` a race detector with reproducible failures.

Effect cannot: V8 owns the event loop. For a language whose non-negotiable #3 is
structured concurrency, "the language where a concurrency bug reproduces on
demand" is a flagship claim. `tests/load.rs` and `tests/net_cancel.rs` are
already the shape that would exercise it.

## 4. The supply-chain guarantee, which is built and not sold

The manifest caps what a package may reach, the `extern` list is auditable, and
`[workspace.permissions]` is a ceiling a member cannot raise. So **a transitive
dependency cannot open a socket unless the manifest grants it**, checked when it
compiles.

Effect's `R` channel documents what a program needs; nothing stops a dependency
doing something else. This is the strongest safety property Khora has, and
`docs/positioning.md` leads with throughput and memory instead.

What is missing is not the mechanism but the surface: a `khora audit` that
prints the effective capability set of a whole dependency tree, so the guarantee
is something a reviewer can read rather than something a compiler enforces
silently. For an audience that has watched npm for the last two years, this may
matter more than performance does.

## 5. Property tests derived from `Schema`

Effect has `Arbitrary`. `Shape` already carries everything a generator needs,
and `khora test` already exists, so `derive(Decode)` could yield round-trip
testing with shrinking for free. Cheap, and it compounds with replay: a
generated log is a generated program run.

## 6. `--trace-effects`

Handlers are values, so a development build can wrap every one and print each
capability use in order. "Run it and see everything it touched" falls out of the
design almost free, and it is the feature a newcomer would use to understand
somebody else's program.

## What not to copy

Effect's shape is partly a record of TypeScript's limits, and importing those
would be importing a workaround for a constraint Khora does not have.

- **HKT defunctionalisation** -- Khora has native `* -> *`.
- **Branded types** -- newtypes already wrap nominally.
- **`Schema`'s `R` parameter** -- effect rows are on functions already; putting
  requirements in the schema type duplicates the effect system with worse
  ergonomics.
- **The typeclass hierarchy.** Effect deliberately left fp-ts's
  `Functor`/`Applicative`/`Traversable` encoding behind because it was hostile
  in TypeScript. Khora exports all three and **nothing in the repository uses
  any of them** -- they appear only in compiler test files. Keep higher-kinded
  types as a language capability; do not promise the hierarchy at 1.0 until a
  real program needs it. `join_all` is what an Effect developer actually reaches
  for.

## Order

1. `Stream`, designed for fusion -- the largest gap, and expensive to get wrong.
2. `Schema` transformation (`via`) -- small, and both replay and property tests
   need a schema that can describe a wire form differing from the domain form.
3. A stability tier, per `std-admission.md` -- makes everything else cheaper to
   land.
4. Record and replay.
5. Deterministic scheduling; `khora audit`; generators; `--trace-effects`.

In practice (2) comes first: it is a day's work and it unblocks two of the
others.
