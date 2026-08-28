# How a capability reaches the code that uses it

**Decided and implemented.**

## Where it started

`with` is a block of `let`s. A capability is an ordinary binding, and the two
ways code reaches one are the two ways code reaches any binding:

- A **named function** declares what it needs in its `with` clause, and those
  labels become extra parameters. A call site supplies them from whatever is
  visible there. This is the whole of `docs/design/effect-runtime.md` §2 —
  there is no handler stack and no dynamic lookup.
- A **lambda** *captures* the bindings its body reads, capabilities included,
  the same way it captures anything else — so its requirement row was always
  empty: whatever it needed, it already had.

Both are sound and neither is surprising on its own. Together they cost
something.

## What that cost

A function value carries its requirement row, so a higher-order function can
take a callback that needs a capability and install it. That worked, end to
end:

```khora
fn shout(n: Int) -> () with { logging: Logging } { logging.note(n) }

fn with_logging(body: (Int) -> () with { logging: Logging }) -> () {
  with { logging: handler for Logging { note: .. } } { body(1) }
}

with_logging(shout)      // prints 1
```

The same thing eta-expanded did not, until this:

```khora
with_logging(fn n => shout(n))
//           ^ `logging: Logging` is required here but not provided
```

**Eta-expansion changed meaning**, which it must not. `f` and `fn x => f(x)`
are the same function everywhere else in the language, and a reader has no way
to predict that wrapping one in a lambda is what broke the program.

The shape it costs is not exotic. It is every library that hands something to a
callback and takes it back afterwards:

```khora
nursery(fn () => ...)          transaction(fn tx => ...)
scoped(fn () => ...)           with_connection(fn c => ...)
with_span(fn () => ...)        with_temporary_file(fn f => ...)
```

Each was writable with a named function, which is why `std::core::nursery` and
`scoped` worked and were tested. But "define a top-level function to use this
API" is a real tax, and it was the one place in Khora where a lambda was not
simply a function.

## The rule

> A lambda resolves a capability lexically if it can, and **requires** it if it
> cannot. What it requires is in its type, and its caller supplies it — exactly
> as for a named function.

So:

- `fn () => report(n)` written inside a function that has `ledger` captures it,
  and its row stays empty. Nothing changes for the common case, and `List::map`
  does not have to become row-polymorphic to accept a callback that logs.
- `nursery(fn () => serve()!)` cannot resolve `nursery` lexically, so the
  lambda's type becomes `() -> () with { nursery: Nursery }`, and the `body()!`
  inside `nursery` supplies it from the handler it just installed.

Resolve-if-you-can-require-if-you-cannot is the same rule a reader already
applies to a name: it means the nearest binding, and if there is none it is a
parameter. Eta-expansion stops changing anything, because the expansion now has
the row the named function had.

### Why not the alternative

Making a lambda's row *always* explicit — capture nothing, require everything —
is more uniform and worse. Every higher-order function in `std` would have to
be polymorphic in its callback's requirements, `List::map` included, so that a
callback which logs can be passed to it. The cost lands on every signature in
the library to buy a property most callbacks do not need. Capture is the right
default; requirement is the right fallback.

## How it is built

Two pieces, not three. The call side was already there: `evidence_from_row`
supplies a closure's requirements from its type and `invoke_closure_at` builds
the matching function type, which is why `with_logging(shout)` ran before any
of this. HIR needed nothing either — see the limit below.

1. **The checker.** A lambda's `requires` starts as a variable, and
   `Checker::absorb_requires` solves it after the body: every `Requires` demand
   raised inside, label by label, is either resolvable lexically — a binding
   supplies it, or an enclosing `with` block installs it — or it is not, and
   what is not becomes the closure's own row. Absorbed rather than merely
   copied: the demand is struck off, so the enclosing function is not charged
   for a capability it neither has nor was asked for. The mirror of
   `absorb_raises`, which does the same for the other row.

   Closed, not open, and sorted by label. Closed because these are exactly what
   the body needs, the same promise a written `with` clause makes. Sorted
   because two places have to agree on the order and only one of them should be
   deciding it — errata 33 was that disagreement.

2. **Codegen.** `declare_closure` appends a parameter per label after the
   written ones, and `emit_closure` puts them in `incoming`, which is where
   `evidence_from_row` already looks for a capability that a `with 'ef` clause
   forwarded and no binding names. They are passed owned like every other
   argument, and released by the closure's outermost scope, because there is no
   local for the reference-counting plan to hang them on.

## The limit

A lambda can **require** a capability it never mentions. It cannot **mention**
one that is not in scope:

```khora
nursery(fn () => spawn_one()!)              // works: a call carries the row
nursery(fn () => nursery.adopt(f))          // does not: `nursery` is a bare name
```

A bare name is resolved by ordinary lookup, and at that point there is nothing
to find — the second line reads `nursery` as the top-level function and fails
somewhere confusing. Making it resolve would mean inventing a binding for any
unresolved name inside a lambda, which turns a typo into a capability
requirement and loses "cannot find `x` in this scope" for every closure in the
language. Not worth it for the case that is one helper function away, and the
helper is where the name belongs anyway.
