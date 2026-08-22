# How a capability reaches the code that uses it

**Decided, not yet implemented.** The rule below is what the language should
do; `docs/roadmap.md` carries it as work.

## How it works today

`with` is a block of `let`s. A capability is an ordinary binding, and the two
ways code reaches one are the two ways code reaches any binding:

- A **named function** declares what it needs in its `with` clause, and those
  labels become extra parameters. A call site supplies them from whatever is
  visible there. This is the whole of `docs/design/effect-runtime.md` §2 —
  there is no handler stack and no dynamic lookup.
- A **lambda** *captures* the bindings its body reads, capabilities included,
  the same way it captures anything else. Its requirement row is therefore
  always empty: whatever it needed, it already has.

Both are sound and neither is surprising on its own.

## What that costs

A function value carries its requirement row, so a higher-order function can
take a callback that needs a capability and install it. That works, end to
end, today:

```khora
fn shout(n: Int) -> () with { logging: Logging } { logging.note(n) }

fn with_logging(body: (Int) -> () with { logging: Logging }) -> () {
  with { logging: handler for Logging { note: .. } } { body(1) }
}

with_logging(shout)      // prints 1
```

The same thing eta-expanded does not:

```khora
with_logging(fn n => shout(n))
//           ^ `logging: Logging` is required here but not provided
```

**Eta-expansion changes meaning**, which it must not. `f` and `fn x => f(x)`
are the same function everywhere else in the language, and a reader has no way
to predict that wrapping one in a lambda is what broke the program.

The shape it costs is not exotic. It is every library that hands something to a
callback and takes it back afterwards:

```khora
nursery(fn () => ...)          transaction(fn tx => ...)
scoped(fn () => ...)           with_connection(fn c => ...)
with_span(fn () => ...)        with_temporary_file(fn f => ...)
```

Each is writable with a named function today, which is why `std::core::nursery`
and `scoped` work and are tested. But "define a top-level function to use this
API" is a real tax, and it is the one place in Khora where a lambda is not
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

## What it takes

The call side is already built and exercised — `evidence_from_row` supplies a
closure's requirements from its type, which is what makes `with_logging(shout)`
run. Three pieces are missing, all on the definition side:

1. **HIR.** When a call inside a lambda names a capability label that no
   visible binding supplies, record it as an evidence parameter of the enclosing
   *lambda* rather than reporting it. It stops at the lambda boundary and does
   not propagate to the enclosing function, which is what keeps `nursery`'s
   caller from having to declare a nursery it does not have.
2. **Types.** A lambda's `requires` is those labels instead of the hardcoded
   empty row.
3. **Codegen.** The lifted body takes the labels as parameters in sorted order —
   the same convention `khora_hir::body`'s `evidence` already uses for a named
   function.

The risk is in the ordering agreement between the three, which is exactly the
kind of disagreement errata 33 was about. It wants its own change with its own
tests, not a corner of another one.
