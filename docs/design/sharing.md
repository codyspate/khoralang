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

**A shareability bit in the function type, inferred.** Sound, and it *colours*:
a `Router` holds a handler, so `Router` carries the handler's bit, and so does
every container of a function above it. Colouring is the thing the rows exist to
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
test double that counts its calls in a `mut` field is refused. `Shared<A>` is
what that is waiting on.

### A type with no body has to say so

`export type Array<A>;` has no visible fields, and answering "shareable" because
none can be seen was wrong in the direction that matters. `Array::set` writes.
`Ptr` points at memory this language did not allocate. A runtime handle may need
a lock of its own. All three looked safe until this rule existed, and two fibers
writing one array compiled and raced.

So a declared type with no body is unshareable until `impl Share for T`.

`Share` is a marker: no methods, and implementing it asserts rather than
provides. It is therefore **not an ordinary impl**, and may only be written
where there is nothing to check — a type with no body. For anything this
compiler can see into, the answer is derived and an impl is refused, because the
only thing it could add is a lie about a record with a `mut` field.

Nobody writes it for a record, a variant or a tuple: those are shareable exactly
when their contents are, and a `Share` bound is satisfied by looking rather than
by finding an impl. Derived where derivable, asserted only where it must be.

Declared today, each with its reason: `Fibers` and `Region` (both take a lock —
see below), `Fiber` (every operation is a message to the runtime), and
`SharedFn`.

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
export type SharedFn<A, B, 'e>;
impl<A, B, 'e> Share for SharedFn<A, B, 'e> {}
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
Router::answer_on(router, connection) catch { _ => respond_500() }
```

`_` subtracts the whole row, tail included. Every neighbour has the form
(`catch_unwind`, `recover`, `catchAll`); this one is checked rather than
dynamic, and it costs what it should — the arm learns nothing about what went
wrong, because there is no name to learn it under.

## What the runtime owes

An `impl Share` is a promise the runtime has to keep, so:

- `Region`'s finalizer list and the nursery's child list are behind a `Mutex`.
  A fiber that acquires a resource wants it released by the scope that outlives
  it, which is the point of handing a `Scope` across.
- A fiber handle's join slot is behind a `Mutex`: two fibers may hold one handle
  and both call `join`, and "take it if it is there" has to happen once.
- `khora_fibers_wait` drains in **rounds** until a round finds nothing. A child
  may adopt a fiber of its own while the parent is waiting — that is what a
  shareable nursery is for — and a single pass would return with a grandchild
  still running, which is precisely the promise a nursery makes. The lock is
  never held across a join, or that adoption would deadlock against it.

## What is still open

- **`Shared<A>`**, for the cases the rules above refuse on purpose: a stateful
  test double, a cache, a counter behind a lock. Its API has to release under a
  raise and under cancellation, which is what makes it a design rather than a
  type.
- **A move-in spawn.** Captures are copied and both fibers keep theirs, so this
  is `Sync`, not `Send`. A consuming spawn could transfer an otherwise mutable
  value safely, and would take the pressure off `Shared<A>`.
- **`Map` cannot cross**, because it mutates its buckets in place. Correct
  today; a persistent map would simply be shareable, with nothing to declare.
- **A lambda has no evidence parameters**, so `nursery(fn () => ...)` — a
  higher-order function that installs a capability *for* its callback — cannot
  work: a capability is a lexical binding, and the thunk was written before the
  binding existed. `Router::listen` writes the `with` block out instead.
