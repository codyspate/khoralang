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

### `std/config_native.kh` — typed configuration

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

### `Clock.sleep` — `std/clock_native.kh`

**An operation on the clock, not an intrinsic**, and that is the whole design.
Waiting is the one thing a program does that a test cannot afford to actually
do, and putting it on the capability means a fake clock is
`handler for Clock { sleep: fn ms => .. }` and nothing else — no test runtime,
no special mode, no rule about which fiber may advance time.

Effect's `TestClock` is the alternative made concrete: a structure of pending
deadlines with an `adjust` that completes them in order, needed *only because*
`Effect.sleep` is baked into the fiber runtime and reaches the clock through a
fiber-local. Its documentation has to open with a footgun — fork the sleeping
effect, or the test fiber blocks and can never call `adjust`. Khora's test for
the same thing is four lines and does not change how the sleeping code is
written. `Random::seeded` already made this argument about the other
unrepeatable input.

The runtime part was almost nothing, because `scheduler::sleep_until` already
existed for the reactor's own deadlines and had no export. So a sleeping fiber
gives its worker back for the whole wait — ten thousand sleeping fibers are ten
thousand entries in a heap rather than ten thousand stacks — and off a
scheduler the thread blocks, which is what `main` does before anything is
spawned. `sleep_until` answers false to say which happened, so the distinction
is read rather than guessed.

This is 3.1's dependency, and the rest of 3.1 is still open.

### A fiber that answers — `std/core.kh`

`Fiber::spawn` took `() -> ()` and `join` gave back `()`, so the only way a
fiber could say anything was a `Channel` or a `Shared`. The runtime noticed:
`fiber.rs` printed *"a fiber ended with an error nobody was waiting for"* to
stderr and freed the object, with a comment saying this was a path that *"should
not survive nurseries — there the error goes to a parent who knows exactly what
it is."* That parent path was never built. This is it.

`Fiber<A, 'er>`, `join(self) -> A raises 'er`, and the child's failure comes
back out of the join with its type intact.

**The row is on the handle, and that is the design.** An erased `Fiber<A>` was
written first and it worked; it was also unpleasant in a way that only showed
up when the first test was written against it. Every join needed a `!` and an
enclosing `raises` — *including on a fiber that provably cannot fail* — and
since the caller's row could not name the child's error type, `catch { _ => .. }`
was the only arm that compiled.

That a row can be a *type* parameter at all was the surprise. `Slot<A, 'er>`
carries one and behaves: `take` on an infallible slot needs no `!`, and a
fallible one raises by name. Nothing in `std` had used it.

**What it costs is that a nursery adopts `Fiber<(), {}>`.** An effect operation
cannot be generic in a row any more than in a type, so `Nursery::adopt` has to
name one shape — verified, not assumed: `adopt: (Slot<(), 'er>) -> ()` is
refused with *"`'er` is a type the caller chooses"*. The honest shape is the one
with nothing left to say. A child that can still fail has nowhere to fail *to*,
because nobody is going to join it, so requiring an empty row moves that
decision to the `adopt` site where somebody can write what a failure means.

Three things wait, and they are not the same wait:

| | waits | takes the answer | on a cancelled child |
| --- | --- | --- | --- |
| letting the binding go | yes | no | nothing |
| `Fiber::wait` | yes | no | nothing |
| `Fiber::join` | yes | **yes** | **unwinds the joiner** |

`Fiber::wait` was not in the plan. It arrived because four existing tests used
`join` purely for ordering, and the last column is why they could not keep
using it.

**That last cell is the one rule this changed rather than added.** A
cancellation stops a fiber and not its parent — `fibers.md` says so and four
tests pin it, and that still holds. But a *joiner* has asked for an answer that
will never exist, and there is no `A` to invent, so the ask fails the way the
child did. A parent that did not ask is untouched.

`Fiber::detach` is 3.1's third call, shipped as recommended: cancel, let go, do
not wait. Without it a `timeout` over a body with an uninterruptible tail is a
lie, and one finalizer that never returns holds its nursery, its parent, and
`main`.

### Two compiler gaps this turned up

Neither is caused by the above; both were found by leaning on parts of the
checker nothing in `std` had used before.

**A row in a type-argument position is not checked against an annotation.**
`Slot<Int, {Boom}>` is accepted where `Slot<Int, {Other}>` is declared, and
`Fiber<(), {DbError}>` is accepted by `adopt`, whose parameter says
`Fiber<(), {}>`. Rows unify *openly*, which is right in `raises` position —
that is subsumption, and `demand_is_carried` depends on it — and wrong for an
invariant argument, where the declared row should be rigid. Inference is
correct, which is the path real code takes; it is annotations that are not
enforced. So `adopt`'s empty row is documentation until this is fixed.

**Tuple inference through a nested lambda gives up.** A `map2` whose inner
lambda builds a tuple and whose outer one destructures it reports *"the type of
this expression was never worked out, and nothing else was reported"* — the
message that asks to be reported. Nesting records instead works.

### `Schedule`, `retry`, `repeat` — `std/resilience_native.kh`

`vision.md` names retry policies as a non-negotiable. What was checked in was
`Schedule = { attempts: Int }` with a `retry` that counted and never waited,
used by nothing — a loop, not a policy.

The existing decision was right and its docstring said why: *"a plain
description rather than a stream of instants: a schedule with no clock in it
can be read, compared and tested."* Kept, and widened to `Times`, `Spaced`,
`Exponential`, `Fibonacci`, `Jittered`, `Union`, `Intersect`, `AndThen`,
`UpTo`, built up from pieces so that the combination nobody anticipated is
spellable.

**Not a closure.** Effect's `Micro` — the 5 kB subset that drops `Layer`,
`Ref`, `Queue`, `Deferred` and `Stream`, and keeps `Schedule` — represents one
as `(attempt, elapsed) => Option<number>`. In Khora that is a record of
closures, which `sharing.md` refuses across a fiber and which would need
`SharedFn` the way `Router` does. An ADT is structurally `Share`, derives `Eq`
and `Show`, and prints in a log line. Khora's constraints produce the better
design here rather than a compromise. The price is arbitrary predicates, which
is why `retry_while` takes one as a separate parameter instead.

**Instants, not delays**, which is what makes `Union`, `Intersect` and `UpTo`
real comparisons — the sooner of two delays is not the sooner of two instants
once the sides have been running for different lengths of time. It also lets
the two honest readings of "again later" coexist as ordinary cases rather than
special ones:

- `Spaced` is a **grid**. The third attempt begins at `3 * millis` whatever the
  body cost, so a run that falls behind does not pile up delays.
- `Exponential` and `Fibonacci` are **backoffs**, measured from the failure
  that just happened. "Wait twice as long as last time" is a statement about
  the other end, not about the calendar — and a backoff anchored to the start
  would shorten every wait by however long the failing call took, which is
  exactly backwards: the call that took thirty seconds to time out is the one
  to be most patient after.

That distinction was a real bug on the way through, caught by a test asserting
on the *sequence of waits* rather than on elapsed time. Which is the other
thing worth recording: **every test in `resilience.rs` runs in microseconds and
none of them waits**, because the fake clock records what it was asked for and
returns. A timing assertion could not have told 100+200+400 from 300+400. That
is `Clock.sleep`-as-a-capability paying for itself the first time it was used.

### Where they went, and where `Clock` went

Two moves, and the second is the one that made the first possible.

**`Clock` is now `std::clock`.** It lived in `std::env` because both answer
"what did the outside world hand this process", and that grouping cost
something the moment anything else wanted a clock: `env_native.kh` is
native-only for `getenv` and `argv`, so the clock was native-only too — not
because clocks are unportable, but because of the file it was in. A Worker has
`Date.now`. One small file with one reason to carry `_native` makes a
`clock_wasm.kh` an afternoon rather than an untangling, and it stops
`import std::env` being the line you write to get a clock.

**`Schedule` and its drivers are `std::resilience`.** A `retry` that waits
needs a `Clock` in its row, and `std::clock` imports `std::core`, so there is
no version of the delaying driver that can live where the old one did.

Moving the *description* along with the drivers rather than leaving it in
`core` is the part I got wrong first time. The argument for keeping it was
"vocabulary belongs in core" — but `Decimal`, `DateTime`, `Json` and `Row` are
all vocabulary that crosses package boundaries and none of them is in `core`
either. `packages/postgres` imports `std::decimal::{Decimal}` and thinks
nothing of it. What `core` actually holds is what the *language* leans on: the
traits `derive` writes, the types codegen has intrinsics for. Nothing in the
compiler knows what a schedule is.

**Named `resilience` rather than `retry`** because a circuit breaker, a rate
limiter, a bulkhead and a hedged request are all the same subject and none of
them is a retry. A module called `std::retry` could not hold them.

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

Ordered by what should be built first. **The numbering is not renumbered when
an item is built** -- it is cited from commit messages, and a §3.4 that quietly
becomes a §3.3 makes those wrong. A built item leaves a pointer and moves its
body to section 1.

### 3.1 A fiber that returns a value

**Built** — see section 1. What is still open is the layer above it: `race`,
`par2`, `par_map` and `timeout`. All four are ordinary Khora now that a fiber
can carry a result and `Clock.sleep` exists, and `timeout` is `race` against a
sleep, so it is one item rather than four.

Also still open, and narrowed rather than solved: **what a nursery says when
several children fail.** The flat list is the right shape, but it does not
belong on `Fibers` — at that level every child's error type differs and the
handles are bare, so there is nothing typed to put in a list. It belongs where
the error type is a parameter, which is `par_map` answering
`List<Result<A, E>>`.

### 3.2 `Schedule` as a widened ADT

**Built** -- see section 1. It needed one decision this section did not
anticipate (where the driver lives, given that `core` must not import a clock),
which is recorded there.

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
