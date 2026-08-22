# What may cross into a fiber

**An open question, not a decision.** The most important one the language has
right now, because the two things it is proudest of do not compose.

## The state of it

`docs/design/memory.md` §5a says a value may cross into a fiber only if it
cannot be written, because two fibers writing one value is a race and atomic
refcounts (D10) protect the count rather than the fields. That rule is right.

Its implementation says something wider:

```rust
Type::Fn { .. } => false,
```

A closure is refused because **what it captured is not in its type**. Nothing at
the type level can see whether a closure holds a `mut` record, so the
conservative answer is no. Also right, taken alone.

Together they say something nobody intended:

> **No capability can ever cross into a fiber.**

An effect *is* a record of function types — that is the whole of
`docs/design/effects.md`'s shape decision — so every handler holds closures, so
every handler is unshareable, so a fiber can never be spawned from a function
that has one.

```khora
export fn forked<'r>(id: Int) -> () with { 'r | ledger: Ledger } {
  let f = Fiber::spawn(fn () => print(report(id)));   // refused
  Fiber::join(f);
}
```

This is not a corner. It is *the* shape of a concurrent server: a request
arrives, a fiber handles it, and the handler needs the database. `std::net::http`
serves one connection at a time for exactly this reason, and `Fiber::spawn` sits
unused three lines away.

Found by the agent writing the HTTP server; the diagnostic now states the real
reason rather than "can be written", which sent a reader looking for a `mut`
that was not there.

## Why it is not a one-line fix

Making `Type::Fn` shareable is unsound. A closure may capture a `mut` record,
and then two fibers write it:

```khora
let tally = { mut count: 0 };
let bump = fn () => { tally.count = tally.count + 1; };
Fiber::spawn(bump);
Fiber::spawn(bump);   // a race, and nothing said so
```

So the question is how to tell that closure from a handler that captured
nothing but other functions.

## The options

**A. Check the captures at the spawn.** The checker already records what each
lambda captured, and `check_spawnable` already walks that list. Extend it: a
captured *closure* is shareable if the checker can see what *it* captured and
those are shareable.

Cheap, and it does not solve the case above: `ledger` in `forked` arrived as an
evidence parameter, so its captures were decided in another function and are not
visible here. It would unblock a handler built and spawned in the same body,
which is the smaller half.

**B. Put shareability in the function type.** `Type::Fn` gains a bit: this
closure captures nothing unshareable. A lambda's bit is computed from its
captures; a written function type declares one; an effect's operations declare
theirs, and a handler that captures something unshareable fails to satisfy an
effect whose operations are declared shareable.

Sound, complete, and checkable — and it is a new thing in every function type,
which is the kind of change that shows up in error messages for years. It is
also the honest one: shareability *is* a property of the value, and the type is
where properties of values go.

**C. Make it a capability question rather than a type question.** A handler is
built by `with { .. }`, and that is a place a rule could live: an effect could
be declared *shareable*, and installing a handler for it would then require the
handler's captures to be shareable — checked once, where the handler is written,
rather than at every spawn.

Narrower than B and aimed exactly at the case that matters. It does nothing for
an ordinary closure crossing a fiber, which A does.

**D. Say no, and mean it.** Keep the rule, and make a fiber take its
capabilities as *arguments* rather than captures — the thunk is
`() -> ()`, so this would mean a spawn form that passes them explicitly and a
handler built inside the fiber. Honest, and it moves the problem rather than
solving it: something still has to hand the new fiber a `Ledger`.

## What I would do

**B, with A as the thing that makes B's default bearable.** The bit belongs in
the type because that is what it is a property of, and inference can fill it in
for every lambda so that almost nobody writes it. A and C are each half of B
arrived at cheaply, and half of a soundness rule is the kind of thing that is
still there in three years.

But it is a change to every function type in the language, and the language has
had one full night of use. It should be decided deliberately, with the HTTP
server as the case to satisfy, and not at the end of a long session.

## Until then

`std::net::http` serves one connection at a time and says why. That is the only
place in the standard library the limitation bites today, and it bites
everything anybody writes on top of it.
