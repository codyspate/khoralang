# What may cross into a fiber

**Decided.** Fibers are operating-system threads today (`khora-rt`), so every
rule here is about a data race that can happen now, not one that might later.

> A value may be held by two fibers when this compiler can see that nothing can
> write it. Where it cannot see — a closure's captures, a type with no body, a
> type the caller chooses — the answer is *no* until somebody writes down why
> not, at the one place where the thing being asserted is visible.

## The problem it solves

`docs/design/memory.md` §5a says a value may cross only if it cannot be written:
two fibers writing one value is a race, and atomic refcounts (D10) protect the
count rather than the fields. Right, and easy to check for a record — it has a
`mut` field or it does not.

A closure is the hard case, because **what it captured is not in its type**.
`(Request) -> Response` says nothing about whether the thing behind it holds a
counter somebody else is incrementing. The conservative answer is to refuse
every function type, and taken alone that is right too.

Together the two said something nobody intended:

> No capability can ever cross into a fiber.

An effect *is* a record of function types — the whole of
`docs/design/effects.md`'s shape decision — so every handler is a record of
closures, so every handler was unshareable, so a fiber could not be spawned from
any function holding one. The two features this language is proudest of did not
compose, and an HTTP server answered one caller at a time.

## What was rejected

**A shareability bit in the function type, inferred.** Sound, and it *colors*:
a `Router` holds a handler, so `Router` carries the handler's bit, and so does
every container of a function above it. Coloring is the thing the rows exist to
avoid, and buying concurrency with it would trade this language's best property
for its second-best.

**Refusing any closure that captures something writable, everywhere.** Tried,
and the whole corpus passed — which is exactly how a bad rule looks from inside
a small codebase written in one idiom. It makes every closure shareable by
construction, at the price of making

```khora
items.each(fn item => { total.sum = total.sum + item; })
```

illegal in a program that never spawns anything. Rust allows that; a language
whose thesis is to beat Rust's ergonomics cannot forbid it to make concurrency
easier. Backed out.

## What is decided

### An effect is shareable, and the handler pays for it

`handler for Ledger { .. }` is the one place a capability comes into existence,
and its operations are written right there — so the captures are on the screen
and can be checked. Answered once, where it is answerable, instead of at every
spawn where it is not.

The check has teeth only if it cannot be dodged, so an operation must be a
closure **written at that literal** or a named function. A binding holding one:

```khora
let leak = fn () => bump(tally);
handler for Counting { tick: leak }     // refused
```

was written elsewhere and took its captures with it. Refused for want of
anything to look at, rather than waved through.

The cost is real and stated: a handler may not capture something writable, so a
test double that counts its calls in a `mut` field is refused. It captures a
`Shared<Int>` instead, which is shareable — `docs/design/shared.md`.

### A type with no body has to say so

`pub type Array<A>;` has no visible fields, and answering "shareable" because
none can be seen was wrong in the direction that matters. `Array::set` writes.
`Ptr` points at memory this language did not allocate. A runtime handle may need
a lock of its own. All three looked safe until this rule existed, and two fibers
writing one array compiled and raced.

So a declared type with no body is unshareable until `impl Share for T`.

`Share` is a marker: no methods, and implementing it asserts rather than
provides. It is therefore **not an ordinary impl**, and may be written in one
place only: the module that declares the type, and only for a type with no body.

The orphan half is not decoration. Without it the marker is forgeable by
anyone — declare a trait of your own spelled `Share`, write
`impl<A> Share for Array<A>`, and an array becomes something two fibers may
hold. That compiled, and raced. The author of a type is the only one who knows
what the compiler cannot, so they are the only one who may say.

For anything this compiler *can* see into, the answer is derived and an impl is
refused outright, because the only thing one could add is a lie about a record
with a `mut` field.

Nobody writes it for a record, a variant or a tuple: those are shareable exactly
when their contents are, and a `Share` bound is satisfied by looking rather than
by finding an impl. Derived where derivable, asserted only where it must be.

Declared today, each with its reason: `Fibers` (it takes a lock — see below),
`Fiber` (every operation is a message to the runtime), and `SharedFn`.

**`Region` is deliberately not on the list, and neither is `Scope`.** A
finalizer's captures need not be `Share` — `acquire` of a connection with `mut`
fields is what one is for — so the fiber that runs a finalizer has to be the one
that deferred it, and that holds only if the region cannot cross. `Region` is
opaque with no impl, which refuses it; `Scope` is an effect, and effects are
otherwise shareable (see below), so `TypeMap::shareable_with` answers false for
it by name (`khora_types::REGION_TYPE`) and `check_handler_is_shareable` exempts
a `Scope` handler, which never crosses and so may capture its region. A fiber's
error row is walked too (`check_raises_stay_home`): `Fiber`'s `A: Share` covers
the answer and nothing covers the error, and asking the whole row for `Share`
would refuse a `mut` record in an error, which is marked at the handover and
correct. Every refusal names the rewrite: `Fiber::spawn(fn () => scoped(work))`.
A lambda works as well, `scoped(fn () => work())`, inside a function with a
`scope` of its own too: the capability `scoped`'s parameter hands the lambda
shadows the enclosing binding for what the body requires without naming it
(`Checker::handed_nearer`). A body that names `scope` still gets the
enclosing binding, which is the lexical rule, and across a spawn is refused.

The route no type can close is `Region::root()`, reachable by name from any
fiber. The runtime records the opening fiber in each region and traps a defer
or a release from any other, and traps `khora_region_root` from a spawned
fiber; `khora test` and `khora bench` give each block a root region of its own.
The release check is what catches a route the checker misses in every build: a
capture whose type is settled only after the spawn is checked (on TODO-0.4) can
hand a child the last reference with no defer for the defer check to see. With
every defer and release on one fiber, a finalizer's captures never cross, so
`khora_region_release` does not mark them.

The cost is a pattern: a child that acquires into its parent's longer scope.
Structured concurrency's "released by the scope that outlives it" is
expressible only for a `Share` resource, acquired by the parent and handed in.
The alternatives — `acquire<A: Share>` (refuses 9 std sites and every package),
keeping the mark for life (leaves the field race), or making a region's release
wait for every fiber that deferred into it (a join at every block end) — are
weighed in the design round `finalizer-sharing`.

**The assertion has to travel as far as the type does**, and getting that wrong
is invisible in the module that wrote it. An impl arrives in another file two
ways: with its trait, or with its type. Neither fires for a type nobody named —
and reaching through a field to answer "is this shareable" deliberately looks at
types the file cannot name, which is what `TypeMap::reachable` exists for.

So a body arrived and its impl did not, and for an opaque type the impl *is* the
answer. `postgres::pool`'s `Pool` holds a `Channel`, `Channel` is opaque with an
`impl Share` beside it, and a `Pool` could not be handed to a fiber from any file
that had not also imported `Channel` — while a file that had imported it for
unrelated reasons was fine. "Add an unused import and your program compiles" is
the shape that makes this worth a rule rather than a fix: the same type, the same
question, two answers.

`Share` impls now travel with every name an imported type *mentions*, opaque ones
included. Only `Share`: every other trait is about resolving something the
program wrote, and importing those would put methods within reach of a file that
cannot name the type they are on.

### A type the caller chooses has to be required

```khora
fn launder<A>(v: A) -> Fiber { Fiber::spawn(fn () => sink(v)) }
```

handed a caller's mutable record to another fiber with nothing to say about it.
`A` is shareable exactly when the signature wrote `A: Share` — the same bound
Rust spells `Send`, checked the same way, and the only place in this design
where a signature has to carry anything.

### `SharedFn` reifies the proof

The router is the case none of the above reaches. Its handlers arrive as a
parameter of `Router::get`, so by the time the `Route` record is built the
closure was written somewhere else. The handler cannot borrow the `handler for`
trick, and the whole router was stuck on one fiber.

```khora
pub type SharedFn<A, B, 'er>;
impl<A, B, 'er> Share for SharedFn<A, B, 'er> {}
```

`SharedFn::of` takes a closure written at the call — checked exactly as a
handler operation is — and returns something that has forgotten it was ever a
closure. A `Route` holding one is shareable in the ordinary structural way, with
nothing special said anywhere about routers:

```khora
Router::new()
  |> Router::post("/analyze/:id", SharedFn::of(fn request => handle(request)!))
  |> Router::listen(8080)!
```

The wrapper does not exist at runtime: `of` returns its argument and `call` is
an ordinary closure call. The whole of what it does happened in the checker.

The cost is the visible wrapper at the mount site. That is the honest price, and
it is paid only by the APIs that actually forward a closure across a fiber
rather than by every container of a function in the language.

### A `_` arm on `catch`

Not a sharing rule, but the server needed it and nothing else could express it.
A supervisor recovers from work whose failures are the *caller's* choice, so
there is no constructor to name:

```khora
Router::answer_on(router, transport) catch { _ => respond_500() }
```

`_` subtracts the whole row, tail included. Every neighbor has the form
(`catch_unwind`, `recover`, `catchAll`); this one is checked rather than
dynamic, and it costs what it should — the arm learns nothing about what went
wrong, because there is no name to learn it under.

Two things it must not take. A **cancellation** travels the same channel and is
in nobody's row, so a `_` that stopped one would break every nursery; it keeps
the propagate path by an explicit case, and so does a test failure. And what
the arm *did* take has to be released with no static type to take drop glue
from — `Backend::emit_error_releaser` switches on the error id instead, in a
function emitted once at the end when every id is known. Errata 45 is what
happens without either.

## What the runtime owes

An `impl Share` is a promise the runtime has to keep, so:

- The nursery's child list is behind a `Mutex`, because a shareable nursery
  may be adopted into from two fibers at once. `Region`'s finalizer list is
  behind one too, although only its owner fiber ever defers: it guards against
  a runtime bug, uncontended, on a path touched when a resource is acquired.
- A fiber handle's join slot is behind a `Mutex`: two fibers may hold one handle
  and both call `join`, and "take it if it is there" has to happen once.
- `khora_fibers_wait` drains in **rounds** until a round finds nothing. A child
  may adopt a fiber of its own while the parent is waiting — that is what a
  shareable nursery is for — and a single pass would return with a grandchild
  still running, which is precisely the promise a nursery makes. The lock is
  never held across a join, or that adoption would deadlock against it.

## What `Share` is not yet

**A boundary.** The compiler recognizes `Share`, `Fiber`, `SharedFn` and the
rest by their bare names, so a file that declares its own `Array` gets the
array intrinsics, and a compiler-special concept can be entered by spelling.
The orphan rule closes the reachable forgery; calling any of this a safety
property needs compiler-known identity — a sealed `Share` that is *the* one
from `std::core` rather than any trait spelled that way, and the same for the
handful of types the backend treats specially. One change, for the whole set.

## What is still open

- ~~**`Shared<A>`**~~, for the cases the rules above refuse on purpose. Done,
  and it is a cell rather than a lock over a mutable record:
  `docs/design/shared.md`.
- **A move-in spawn.** Captures are copied and both fibers keep theirs, so this
  is `Sync`, not `Send`. A consuming spawn could transfer an otherwise mutable
  value safely, and would take the pressure off `Shared<A>`.
- ~~**`Map` cannot cross**~~, because it mutates its buckets in place. Still
  true, and no longer a gap: `Dict` is the ordered persistent map, shareable
  with nothing to declare, and is what a `Shared` table is made of.
- **A lambda has no evidence parameters.** A higher-order function that
  installs a capability for its callback takes a named function, not a lambda,
  so eta-expansion changes meaning. `docs/design/capability-passing.md`.
