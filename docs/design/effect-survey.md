# What Effect-TS has that Khora should, and what it has that Khora should not

**A review list, not a decision.** Sections 1 and 2 record what was built and
why; section 3 is what is proposed and still open; section 4 is what was
rejected and the reason, so that nobody has to survey it twice.

The survey read Effect's v3 documentation *and* its v4 source, and the second
half turned out to matter more than the first: several of Effect's most
elaborate mechanisms have been deleted or flattened in v4, and where Khora had
already declined to build them, that is a stronger argument than any
first-principles one. Those are noted where they apply.

Judged throughout against `docs/design/ecosystem.md`'s rule — *is there a middle
layer, a thing that fails only in production and that every library would answer
differently?* — and against `docs/vision.md`'s non-negotiables.

---

## 1. Built

### `Redacted<A>` — `std/core.kh`

A wrapper whose `Show` prints `<redacted>` and which has **no `ToJson`**.

Both halves are deliberate and point in opposite directions. *With* `Show`, a
record holding a secret still derives `Show`, so the config a service prints at
start-up stays printable and the password in it does not appear. *Without*
`ToJson`, a record holding a secret does **not** derive `ToJson`, and the build
stops — which is the right place to stop, because a type that serialises a
secret is a bug and one that serialises `"<redacted>"` is a payload that fails
to round-trip somewhere further away.

**No `Eq`.** Comparing two secrets is a real thing to want, and a derived `Eq`
compares byte by byte and stops at the first difference — a timing side channel
written by somebody who was not writing one. Anybody who needs the comparison
writes `expose` and says so.

Interpolation closed itself off for free: `"${key}"` wants a `String` and does
not call `Show`, so it does not compile.

This is where a nominal type earns its keep. Effect's `Redacted` is a
`toString` override, which any structured logger walks past because it reads
fields rather than calling it. Khora's is a missing impl, which is a compile
error. Twenty lines, strictly stronger.

### `Validated<A, E>` — `std/core.kh`

Error accumulation. `map2` runs its function only if both sides succeeded and
otherwise carries **every** error from both; `and_then` fails fast, because its
second step is written in terms of the first's value and there is no second
answer to collect when there is no first value.

`std::core`'s own docstring on `Applicative::map2` has been pointing at this
since it was written — *"`Option` gives up if either is `None`, and a validation
type would collect both failures."*

**Not an `Applicative` instance.** The instance needs `Validated<_, E>`, the
error parameter fixed and the value parameter free, and Khora has no partial
application of a type constructor. The methods are the same functions under
their own names; the instance can arrive later without changing a call site.

### `List` gained `Show` and `Eq` — `std/core.kh`

Fallout, and the reason generalises past `Validated`: `derive(Show)` walks
fields, so a record holding a `List` could not derive one. The container people
reach for by default was the one that made a struct unprintable.

`show` gives `[a, b, c]`, which is how the literal is written.

### `std/config.kh` — typed configuration

`string`, `integer`, `boolean`, `secret`, `or_default`, `report`, and a
three-case `ConfigError`. Every reader answers `Validated`, so `map2` reports
every missing key in one pass rather than one restart per key.

**And deliberately no `Config<A>` description type.** Effect's is a twelve-node
AST interpreted by a swappable `ConfigProvider`, and the entire reason for the
description layer is that the read must be deferred so the provider can be
swapped. *Khora's provider is the `Env` handler, and it is already swappable* —
a test writes `with { env: handler for Env { .. } }` and these functions read
from it, because the row says they read from something. The AST, its
interpreter, and a second spelling of every reader buy nothing that the return
type does not already buy. The tests install a fake `Env` and never touch a
real variable, which is the argument made checkable.

Two details worth keeping:

- **`or_default` fires on `Missing` and never on `Malformed`.** `PORT=eighty`
  quietly becoming `8080` is the bug this module exists to stop, and it is the
  one a fallback written the obvious way introduces. Only possible because the
  error is structured.
- **`ConfigError::Denied` is its own case**, because a variable the manifest
  refuses sends a reader to `khora.toml` and a variable nobody set sends them to
  a deployment script. Same reasoning as `IoError::Denied` and `EnvError::Denied`.

Nothing here raises: reading configuration is the one place where "tell me
everything that is wrong" is the whole job, and a `raises` stops at the first.

### `Channel::dropping`, `Channel::sliding`, `Channel::poll`

Blocking is the right default — a queue nobody drains is a producer that should
slow down — and it is wrong in exactly one place, which every service has: the
path that must not stall. `docs/design/channels.md` has the table.

**The asymmetry in the answer is the design.** `dropping` answers `false`, so
the loss is visible and countable; `sliding` answers `true`, so it is not,
because the reason to slide is that the newest value is the only one worth
having and nobody was going to act on the loss. Choosing between them is
choosing whether the loss is somebody's business.

It is a property of the channel rather than of the send: a queue is lossy or it
is not, and two senders disagreeing about which is not a state a queue can be
in.

---

## 2. Corrections to documents, found on the way

- **`docs/vision.md`** claimed Khora's dependency-injection advantage over Rust
  was *"`Layer` and capability rows in the type system"*. `effects.md` rejects
  `Layer`, and section 4 below is why that rejection was right. Now reads
  "capability rows".
- **`std/core.kh`'s `nursery` and `scoped`** both said *"Pass a named function,
  not a lambda"*, and explained that a lambda's requirement row is always empty.
  `docs/design/capability-passing.md` is marked *Decided and implemented* and
  fixes exactly that. Checked against the compiler: `nursery(fn () => serve())`
  compiles and runs. The docstrings were stale and now say so.

---

## 3. Proposed, and open

Ordered by what should be built first. Nothing here is started.

### 3.1 A fiber that returns a value, and `sleep`

**This is not a comparison finding; it is a hole.** `Fiber::spawn` takes
`() -> ()` and `join` gives back `()`, so the only way a fiber can communicate a
result is a `Channel` or a `Shared`. There is no `khora_sleep`, no
`Clock.sleep`, no `timeout`, no `race`, and no bounded parallel map.
`positioning.md` says Khora should be a candidate wherever a team considers Go;
a language in which a database call cannot be timed out is not that.

Proposed: `Fiber<A>` with `join(self) -> A`, plus `race`, `par2`, `par_map` and
`timeout`. The runtime is most of the way there — the join slot exists and is
mutex-guarded, and needs to carry a word instead of nothing. A fiber result must
be `Share`, the bound `spawn` already needs.

**Three calls to make.**

1. **What does `join` do with the body's error?** Re-raising matches
   `Fiber.join`; reifying it as a `Result` matches `Fiber.await`. Effect ships
   both because they are different — the second lets a supervisor inspect an
   outcome. *Recommendation: `join` first, the reifying form when a supervisor
   needs it.*
2. **What does a nursery report when three children fail and two are
   cancelled?** Khora's tagged return carries exactly one error and
   `bounded_nursery` has no answer. Effect v3 modelled this as a
   `Sequential`/`Parallel` tree; **v4 flattened it to a list of
   `Fail | Die | Interrupt`**. *Recommendation: the flat list. Settle it
   together with 3.3, since both are "what does the unwinding path know".*
3. **The escape valve, which Khora lacks entirely.** The nursery always
   cancels-then-waits and `Region::defer` runs finalizers with cancellation held
   off. That is correct, and it is also how a program hangs: one finalizer that
   never returns hangs the nursery, which hangs its parent, up to `main`.
   `scheduler.md` promises both *"cancellation: bounded latency"* and *"nursery
   exit: every child stopped or joined"*, and those are in tension.
   *Recommendation: ship `Fiber::detach` — signal and do not await — with the
   rest. Without it, `timeout` over an uninterruptible body is a lie.*

**`sleep` must be an operation on `Clock`, not an intrinsic.** Effect's
`TestClock` is real machinery — a structure of pending deadlines with an
`adjust` that completes them in order — and it exists *only because*
`Effect.sleep` is baked into the fiber runtime and reaches the clock through a
fiber-local. Its documentation has to lead with a footgun: fork the sleeping
effect or the test fiber blocks and can never call `adjust`. If `sleep` is a
`Clock` operation, **deterministic test time costs nothing** — a fake clock is
`handler for Clock { sleep: fn ms => .. }` and no runtime support is needed. The
capability *is* the seam, exactly as `Random::seeded` already establishes.

### 3.2 `Schedule` as a widened ADT

`std/core.kh`'s `Schedule` is `{ attempts: Int }` and is used by nothing.
`vision.md` names retry policies as a non-negotiable.

The existing decision is right and its docstring says why: *"a plain description
rather than a stream of instants: a schedule with no clock in it can be read,
compared and tested."* Keep it; widen the description to `Times`, `Spaced`,
`Exponential`, `Fibonacci`, `Jittered`, `Union`, `Intersect`, `AndThen`, `UpTo`.

**Do not copy the representation.** Effect's `Micro` — the 5 kB subset that
drops `Layer`, `Ref`, `Queue`, `Deferred` and `Stream` — keeps schedules as a
closure. In Khora a closure-based schedule is a record of closures, which
`sharing.md` refuses across a fiber and which would need `SharedFn` the way
`Router` does. An ADT is structurally `Share`, derives `Eq` and `Show`, and
prints in a log line. Khora's constraints produce the better design here.

Two semantics worth copying, both non-obvious:

- **A decision is an absolute interval, not a relative delay** — anchored to the
  original start. That is what makes a fixed schedule drift-free and
  non-piling when it falls behind, and what makes `Intersect` a real interval
  intersection rather than a max of delays.
- **The schedule never sleeps; the driver does.** The driver reads the clock,
  steps the schedule, and sleeps only if the next instant is still ahead.

Jitter draws from `Random`, so it is seedable, and the row says so.

Roughly 200 lines of `std`, no grammar and no type-system work — **once 3.1
exists**. That `Micro` keeps `Schedule` while dropping `Layer`, `Ref`, `Queue`,
`Deferred` and `Stream` is Effect's own ranking of what is irreducible, and it
agrees with this list.

### 3.3 Finalizers that know how the scope ended

`Region::defer` hands the finalizer nothing. `std::db::transaction` works around
this correctly — registering the rollback before the body and marking the
transaction settled on commit — but that is a boolean threaded by hand, and
every future `acquire` that must behave differently on the failing path will
thread its own.

Proposed: an `Outcome` of `Completed | Failed(error) | Cancelled`, a
`defer_with`, and an `acquire_with`. `defer` stays as it is, so nothing existing
changes.

**The call: how does the outcome reach the region?** `khora_region_release` runs
as drop glue and has no idea why. But codegen **already distinguishes** the
paths — `leave_scope` at the end of a block, `unwind_to` on a return or a raise
— and already knows the error id, and already knows `u32::MAX` means
cancellation. *Recommendation: one extra runtime call on the unwinding paths
before the release, plus one field on the region.*

Cheapest real capability on this list, and it retires a hand-threaded boolean.

### 3.4 `Stream`

`vision.md` names it twice and nothing exists. The concrete production gap is
that `std::net::http`'s `Request.body` is a `String`.

Khora's pieces are unusually well placed. `Iterator`'s
`fn next(self) -> Step<Self, Self::Item>` is *already* a pull-based stream in
successor-passing form; `Channel` is the bounded queue a `buffer` needs;
`Fibers` is the nursery; `Region`/`acquire` is resource safety.

**Effect v4 independently arrived at the shape to build**, which is the
strongest confirmation available: it deleted the seven-parameter `Channel` and
its bespoke executor, and a v4 channel is now a function from an upstream pull
to a downstream pull, with pipelining as composition. Three further v4 changes
land in Khora's favour:

- **`Chunk` was dropped** in favour of a plain non-empty array. The rope existed
  to make immutable JS array concatenation cheap; the *chunking* was essential,
  the *representation* was not. Take the batch, use a slice, skip the rope.
- **The non-empty refinement is load-bearing** — it makes "a pull returned
  nothing" unrepresentable, killing a class of spin loops.
- **End-of-stream moved into the error channel as a distinguished variant.**
  Khora already has that mechanism: `effect-runtime.md` §6 reserves
  `which = u32::MAX` for cancellation *because it is an id no error type can be
  assigned*. An end marker is the same trick one id further down — one control
  path instead of two.

**The call, and it is worth a spike before anything else: how does an effect row
attach to a trait method?** `khora-types/src/traits.rs` already parses `with` and
`raises` on a trait method signature, but no `std` trait uses a row *variable*
and `Functor::map` has no row at all. Half a day of spiking decides whether
`Stream` is library work or type-system work.

Also worth taking from Eio rather than Effect: the split between a **byte**
source/sink and an **element** stream. HTTP bodies want the first; a windowed
reconciliation feed wants the second. Effect conflates them behind `Chunk`.

**One hazard to design out from the start.** Every Effect fan-out gives each
branch a bounded queue, and an abandoned branch fills its queue and blocks the
distributor — a silent deadlock, with no warning in the source or the docs. A
fan-out that owns a nursery can make an abandoned branch a detectable error
instead of a hang.

### 3.5 `Schema`

One value denoting both a decoder and an encoder, from which JSON Schema,
property-test generators, structural equivalence and pretty-printing are
derived. Effect's error tree is genuinely good: the refinement and
transformation discriminants tell you *which layer* rejected a value, which
distinguishes "that wasn't a string" from "that string failed my rule".

**The call: compile-time or runtime?** Effect's schemas are runtime values
buildable from runtime data, which forces an interpreter, boxed inputs, and a
recursive case that is a *lazily cyclic closure graph*. `memory.md` D11 says a
cycle leaks in Khora and the thing that breaks one does not exist. *Do not port
that shape.*

*Recommendation: `derive(Schema)`, expanded source-to-source exactly as
`derive(ToJson)` already is, producing monomorphized `decode`, `encode`,
`json_schema` and `arbitrary`.* The whole value proposition survives;
monomorphization replaces interpretation; no AST is ever allocated. What is lost
is runtime-constructed schemas, which nothing in `positioning.md` asks for.
`derive`'s own doc says its six members are derivable *because every one is
structural* — `Schema` is structural too, so this fits the existing rule rather
than bending it.

Two things to take whatever the shape: the error tree with its layer
discriminants, and the rule that **JSON Schema and generators describe the
*encoded* side and stop at the first transformation**. That rule produces
documented footguns in Effect and cannot be avoided — a bidirectional schema
genuinely has two shapes — so it is better stated than rediscovered.

One Effect decision explicitly not to copy: schemas that require services and
decode asynchronously, which make the synchronous entry point runtime-partial.
In Khora an effectful schema carries a row and the synchronous entry point
simply does not exist on it. Better for free.

### 3.6 Metrics, and the carrier underneath them

`docs/design/observability.md` already argues that metrics may not belong in the
first cut, because traces and logs share a propagation problem and metrics do
not — theirs is aggregation and temporality, which is closer to a package's
business. *Recommendation: keep that answer.*

The one thing worth taking is the observation that Effect's ambient metric
labels are a fiber-local, the same mechanism carrying log annotations and the
current span. Roadmap 13.4 — trace propagation across spawn, steal, wake,
blocking hand-off and cancellation — is still designed rather than built, and
needs exactly one such carrier.

**Build it closed and internal**: a fixed struct on the fiber, not a
user-facing keyed map. `scheduler.md` §12 already rules out a fiber-local
storage API, and rightly — a user-visible fiber-local is a hidden input, which
is the thing capabilities exist to abolish. Build it once for tracing and metric
labels cost nothing extra.

---

## 4. Rejected, with the reason

So that nobody surveys this twice.

| | Why not |
| --- | --- |
| **`Layer` and layer memoization** | `effects.md` already rejects it, and Effect's own v4 migration proves the rejection right: in v3 *"two `Effect.provide` calls with overlapping layers would silently build those layers twice"*, and v4's fix is described in their guide as *"a safety net to avoid the footguns"*. The residual footgun survives — the memo key is object identity, so calling a layer factory twice yields two structurally identical layers built separately. That is Khora's `let` binding, arrived at the long way round. **Worth citing in the documentation**: it is the clearest "direct style is not just prettier" argument available, now backed by release notes rather than assertion |
| **`Ref`** | `Shared<A>` already is one. Worth knowing *why* Effect's has no CAS and no lock: its atomicity comes entirely from the update function being typed pure and synchronous, so it runs in one non-yielding interpreter step. That does not port to real threads — but the API property does. **Because the change function is pure, retrying it is free**, which is what makes a compare-and-swap loop legal. `khora_shared_update` takes a mutex today; for word-sized values it could be CAS. Invisible in every type, and available only because `update` cannot fail — the property `std/core.kh` already argues for on cancellation grounds |
| **`SynchronizedRef`** | An effectful update under a lock, which `std/core.kh` refuses on purpose. Effect's reasoning is sharper than ours: an effectful update **cannot** use CAS, because retrying re-runs the effect and that is not idempotent. So the lock is mandatory rather than chosen, it is non-re-entrant, and it makes even pure writes take it. Keep refusing |
| **`SubscriptionRef`** | Revisit after `Stream`, not before. One design point if it happens: read-current and subscribe must happen under the same lock, or you get a missed update or a doubled initial value |
| **Batching / `RequestResolver`** | v3 required the runtime to reify "I am blocked" and restructure the effect tree, which direct style cannot do. Two corrections to that verdict: v3 batching is **opt-in** (the gate is `batching === "inherit"`, so the docs' own "disabling batching" example is a no-op), and **v4 reversed the design** to a time-windowed collector with a delay and a batch key — which needs no reification and *is* buildable in Khora as a collecting handler over a `Shared`. So "cannot be built" was wrong; **"a package, by whoever needs it, once `sleep` exists"** is right. Keep the N+1 answer explicit (`load_many(keys)`) and document why, because people will ask |
| **`Cause` as a tree** | v3's `Sequential`/`Parallel` topology was flattened in v4. Khora's `{ which, payload }` with `u32::MAX` for cancellation already gives the distinction that survived. Take the flat list under 3.1(2); leave the tree |
| **`Exit` as a separate type** | Subsumed by 3.3's `Outcome` and 3.1(2)'s failure list |
| **`Effect.gen`, `pipe`, dual APIs, branded types, `Match`, `Effect.Do`** | Every one is a TypeScript workaround. `dual` dispatches on `arguments.length` at runtime; branded types are a phantom intersection because TS is structural; `Match` builds a runtime matcher because TS has no expression-level match; `Do`/`bind` accumulates a growing record to avoid callback nesting. Khora has methods, nominal types, `match`, and `let` |
| **`Either`/`Option` interop** | Khora has `Result` and `Option`; `attempt` is `Effect.either` |
| **`Chunk`** | A depth-balanced rope solving a JS immutable-array-concat problem Khora does not have. Effect dropped it from `Stream` in v4 |
| **`Runtime` / `ManagedRuntime` / `RuntimeFlags`** | `Runtime<R>` is a root fiber's initial context, flags and fiber-locals; Khora's root fiber is `main` with an empty row and a root region, discharged at compile time. `ManagedRuntime` exists because `runPromise(provide(program, AppLayer))` rebuilds the graph per call; `with Production { .. }` wraps `main` once. `runSync`'s "cannot be resolved synchronously" exception is a workaround for JS having no way to block a thread |
| **`FiberRef` as a user-facing feature** | A user-visible fiber-local is a hidden input, which capabilities exist to abolish; `scheduler.md` §12 already rules out the API. Adopt the *mechanism* once, closed and internal, for tracing — 3.6 |
| **STM** | Merged into Effect v4's core. Its value is composing independent transactions, which needs retry-on-conflict or continuation capture; `effect-runtime.md` §3 rules out the latter for a reason that does not bend — multi-shot capture needs stack maps, which is precise-GC machinery |
| **`Cache` / `ScopedCache`** | Genuinely useful, genuinely portable (`Dict` + `Shared` + a one-shot cell), and genuinely a **package**: there is no middle layer. If someone builds it, copy `ScopedCache`'s **refcounted borrow** — `get` requires a scope, so eviction waits for active borrowers rather than ripping a resource out from under a user. Note also that Effect's TTL is lazy (no sweeper, so expired entries hold capacity until touched) and that it **caches failures** for the TTL, which surprises people |
| **Durable execution (`@effect/workflow`)** | Alpha, and its own docs name the use case as *"a payment that has to be reconciled with the payment provider"* — `positioning.md`'s target verbatim. It is a distributed-systems product, not a language feature. A package, and it wants `Schema` and `Stream` first |
| **A `Duration` newtype** | Khora spells time as `Int` milliseconds consistently. A wrapper buys little and needs conversions at every boundary |
| **`Effect.cached` / `cachedFunction` / `once`** | Memoisation helpers; `cachedFunction` is unbounded with no eviction. Package-level |

---

## 5. What Effect does worse, and why that is worth writing down

Not gloating — each of these is a trap Khora can still walk into, and several
are things to say out loud in the documentation because they are real
advantages nobody would otherwise notice.

- **`Layer` memoization was a footgun Effect had to patch**, and the residual
  reference-identity issue is still documented as a caution. Khora's `let`-bound
  `with` block has none of it by construction.
- **`Cause` was over-modelled and got flattened** — six variants including a
  recursive tree became three in a flat array. Resist growing the error channel
  past `{ which, payload }` plus a list.
- **The library tax is visible in their own numbers.** A minimal program went
  from ~70 kB to ~20 kB in v4 after *"the core fiber runtime has been rewritten
  from scratch"*; `Micro` exists at 5 kB purely to escape it. Khora pays none of
  this.
- **v3's seven-parameter `Channel` and its bespoke executor were not
  essential** — Effect deleted them. Do not build the elaborate version of
  anything Effect has since simplified.
- **Effectful, service-requiring decoding was a mistake**: it makes the
  synchronous decode path runtime-partial. A row instead, so the sync entry
  point does not exist on an effectful schema.
- **`TestClock` needs a fork-or-deadlock caveat**, because the clock is ambient
  rather than a capability. Eio's mock clock instead advances when nothing is
  runnable, which is better semantics and available to Khora for the same
  reason — 3.1.
- **Fan-out deadlocks silently** when a branch is abandoned. Nurseries let Khora
  make that an error instead of a hang — 3.4.
- **Bounded pub/sub gives head-of-line blocking**: one slow subscriber throttles
  every publisher. Guaranteed delivery bought with liveness; make the trade
  explicit at the constructor, as `Channel::dropping`/`sliding` now do.
- **Interruption is only observed at effect boundaries** — a tight loop inside
  one synchronous step cannot be interrupted. **Khora already solved this
  better**: `scheduler.md` §1 separates safepoints from cancellation points and
  emits `khora_safepoint` at every loop back-edge, measured under `bench/service`
  and inside the noise floor. Worth saying out loud somewhere, because it is a
  real advantage that looks like an implementation detail.
- **`uninterruptibleMask`'s `restore` is subtle**, and a naive version is a bug:
  it restores the *caller's prior* interruptibility rather than "make
  interruptible", so masks nest without punching a hole through an outer
  uninterruptible region. Relevant if Khora ever grows a user-facing mask beyond
  `Region::defer`'s implicit shield.
