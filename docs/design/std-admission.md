# What belongs in `std`

**Status: decided, and the first removal is done.** Companion to
`docs/design/std-surface.md`, which audited what `std` *contains*. This says what
it may contain.

`docs/design/compatibility.md` makes every public item in `std` a promise for
the life of a major version. `std-surface.md` found that the set had never been
reviewed against that, because several items existed "because a reference
application needed them at the time". A review needs a rule, and the rule that
was in use -- **does this move on somebody else's schedule?**, from
`ecosystem.md` -- measures the wrong risk.

It is not wrong. It correctly put Postgres and OTLP in packages, and it caught
the LLM vocabulary in `std::ai`, where `role` is a `String` precisely because
providers keep adding to the set. But it is tuned for *external* cadence, and
the dominant risk for a language this young is **its own design churn**. The
thing most likely to force a breaking change in `std::net::http` is not the HTTP
RFC. It is Khora learning that `Router` was the wrong shape.

A rule blind to that licensed a 2,343-line HTTP framework and a tensor library
into a permanent promise. One of those has already been withdrawn.

## The floor: what is not a choice

Two mechanical tests, no judgement required. `std` is the part of the ecosystem
**version-locked to the compiler**, so an item must be in it if either holds.

**The compiler names it.** Remove it and ordinary syntax stops working:
`List` (literals), `Option` and `Result` (patterns and `!`), `String`, `Int`,
`Float`, `Bool`, `Eq`, `Ord`, `Show`, `Hash` (derive targets), `Iterator`
(`for`), `Fibers`, `SharedFn`, `Scope`, `Array` (the FFI surface), and
`Decode`, `Encode`, `Schema`, `Fields`, `Raw` -- `khora-hir`'s `DERIVABLE` and
`bring_derive_companions` name all five, and `khora-types` tells a reader to
"import it from `std::schema`".

**The runtime implements it.** It ships inside `khora_rt` whether or not `std`
exposes it, so excluding it means shipping dead symbols. Measured by counting
`extern fn`: `fs` 15, `net::socket` 13, `decimal` 13, `net::tls` 10, `trace` 5,
`env` 5, `random` 4, `process` 4, `core` 4, `clock` 3, `log` 1.

Everything else is a choice. As of the audit that is about 6,700 lines --
`net::http`, `schema`, `json`, `time`, `db`, `resilience`, `permissions`,
`config` all bind nothing.

## The test for everything else

> **If two independent packages both needed this, would they have to *agree* on
> it, or could each bring its own?**

Agreement is the only thing a shared library is for. Everything else is
convenience bought with a decade of compatibility.

| Must agree -- `std` | Each can bring its own -- package |
| --- | --- |
| `Request`, `Response`, `Method`, `Status`, headers | `Router`, layers, `listen`, a client |
| `Db`, `Row`, `Cell` | a driver, a pool |
| `Tracer`, `Span` | an exporter |
| `Decode`, `Encode`, `Schema`, `Shape`, `Rule` | -- |
| effect declarations: `Clock`, `FsRead`, `Log` | retry drivers, backoff policy |

**Effects are the strongest `std` citizens in this language.** An effect
declaration is a protocol, so it is interop by construction. Data types are the
weakest, because each one freezes a shape.

**A combinator that is reflected in the AST is not convenience.** `at_least`,
`min_items` and `one_of` look like sugar over `refine` and are not: `Rule` has a
named variant for each, and `Shape::to_json_schema` renders them into `minimum`,
`minItems` and `enum`. A package cannot add a `Rule` variant, so moving the
constructors would split one decision across two promises. They stay. This
document originally said otherwise and was wrong.

## The audience decides the floor, not just the ceiling

`docs/vision.md` says who this is for: people who want Effect's model with
Rust's developer experience. That is a checklist a migrating developer already
carries, and `std` is measured against it. Cutting `Config` or `Schedule` to
packages would not read as discipline; it would read as "Khora does not have
that".

| Effect | Khora | |
| --- | --- | --- |
| `Effect` | the language: effect rows | better -- no `Effect.gen`, no `yield*` |
| `Context`/`Tag`/`Layer` | handlers and `context` | better -- no wiring |
| `Option`/`Either` | `Option`/`Result` | equal |
| `Fiber`/`Scope` | `Fiber`, `Scope`, `Nursery` | equal |
| `Queue` | `Channel` | equal |
| `Ref` | `Shared` | equal |
| `Schedule` | `std::resilience` | equal |
| `Config` | `std::config` | equal |
| `Logger`/`Tracer` | `std::log`, `std::trace` | equal |
| `Schema` | `std::schema` | **short a transformation** |
| `Metric` | -- | missing |
| `Duration` | split across `clock` and `time` | scattered |
| **`Stream`** | -- | **missing, and named in non-negotiable #1** |
| `Deferred`, `Semaphore`, `Cache` | -- | missing |

The rows marked *better* are the argument for the language existing. Effect's
worst ergonomic taxes -- `Effect.gen`, `yield*`, `Layer` wiring, the `R` channel
-- are all things Khora deletes by having effects in the language rather than on
top of it.

**And the ceiling is lower than Effect's**, because the parts of Effect that
exist only to work around TypeScript are not needed: HKT defunctionalisation,
branded types (Khora's newtypes wrap nominally), and `Schema`'s `R` parameter,
which would duplicate the effect rows the language already has.

## What this does not decide, and should

**`std` has no way to say "not settled yet".** There is no `unstable`, no
`preview`, no `experimental` marker anywhere in the front end. At 1.0 every item
freezes at once, on a library whose oldest line is weeks old. That makes every
question here binary -- promise it forever, or exile it -- when the honest
answer for much of this library is "probably right, ask again in a year".

Two shapes, neither built:

1. **`std::preview::*`**, a namespace items graduate out of, carrying no
   compatibility promise. Cheap; no package machinery needed.
2. **First-party packages bundled with the toolchain**, versioned separately and
   outside the 1.0 promise -- Go's `golang.org/x/*`. Better, and it needs
   resolution without a registry round-trip, which `khora-pkg` does not have.

Either would let `Router`, the retry drivers and the `schema` combinators ship,
be used, and be *fixed*. This is worth more than any individual cut, including
`std::ai`, and it is the recommended next structural change.
