# Unions, and what `+` is for

## The decision

**`+` is conjunction. `|` is disjunction.**

```khora
fn describe<T: Eq + Show>(a: T, b: T) -> String   // T implements Eq *and* Show
fn width(x: Int | String) -> Int                  // x is an Int *or* a String
```

`+` for trait bounds already works and is Rust's rule. `|` for a union of
concrete types is not implemented; this document says what it would mean and
what it costs, so the decision is on record before anybody starts.

## What is true today

| Written | Today |
| --- | --- |
| `<T: Eq + Show>` | works, and costs nothing at run time |
| `raises A + B` | works — a `raises` clause takes a *row*, and `+` builds one |
| `with { a: A, b: B }` | works — same, for capabilities |
| `Result<Int, A + B>` | **refused**, as of the change this document accompanies |
| `Int \| String` anywhere | not parsed |

`Result<Int, A + B>` used to be accepted and mean nothing. `type_of_syntax` had
an arm per type form and `_ => Type::Unknown` beneath them, and `Unknown`
unifies with everything — so the annotation switched off the checking of that
position rather than constraining it. Errata 64.

## Why `raises A + B` is spelled with `+` and probably should not be

A `raises` row is a disjunction: the function raises `A` *or* `B`, never both
at once. By the rule above it should be `raises A | B`.

It is `+` because rows came first and nothing else used the symbol. That is a
reason and not a justification, and the two now disagree in a way a reader can
notice: the same `+` means "and" in a bound and "or" in a row, three lines
apart in the same signature.

Changing it is a breaking change to every `raises` clause in `std`, the
examples, the packages, the tests and the documentation. It should happen *with*
`|` landing rather than before it, so a program is edited once, and both
spellings should be accepted for one release with the old one warning.

## What a union of concrete types costs

Not a small feature, and the cost is concentrated in one place a reader would
not guess.

**Subtyping, in a unifier that has none.** Khora's inference is
Hindley–Milner: unification decides that two types are *equal*. A union needs
`A` to be acceptable where `A | B` is wanted, which is subtyping — a different
relation with different algorithms, and it touches every call site, every
`let`, and every generic instantiation. This is the expensive part and it is
not localised.

**Exhaustiveness.** `match` on an `A | B` has to know its arms cover both, and
`usefulness.rs` reasons about constructors of one type rather than a set of
types.

**A runtime representation.** A value of type `A | B` needs a tag saying which
it is, so it is a boxed pair of a discriminant and a payload — the same shape a
variant already has. That part is cheap, because the machinery exists.

**Perceus.** Drop glue per arm, and the reuse analysis has to know that an
`A | B` holding an `A` can be reused as an `A`.

**`attempt`.** The function that turns a failure into a value takes one error
type today, precisely because `Result<A, E>` needs one `E`. With unions it
becomes `attempt<A, 'er, 'ef>(..) -> Result<A, <the row as a union>>`, which is
the payoff: the reason to want unions at all is that a two-type failure row
currently has nowhere to go.

## What is deliberately not in this

**Existentials.** `SomeType<(A: B + C) | D>` — "either some type implementing
B and C, or a D" — is a third feature wearing bound syntax. A bound constrains
a parameter *being introduced*; in an argument position nothing introduces `A`,
so the only available meaning is "some type, chosen elsewhere", which is
`dyn Trait`.

That is the one thing whole-program monomorphization cannot erase. Every trait
call in Khora is direct because the concrete type is known at the use site; an
existential needs a vtable, a boxed representation and drop glue that dispatches
— the first indirect call in the language, introduced through a corner of union
syntax rather than on its own evidence.

If a program turns up that cannot be written without one, that is when to
decide, and it should be decided as "does Khora have dynamic dispatch" rather
than as "what may appear inside a `|`".

## Order of work, if it is taken up

1. `|` parses in a type position, and is refused with a message that says it is
   not implemented. (Refusing beats `Unknown`, which is where this started.)
2. Subtyping in the unifier, with `A <: A | B` and nothing else. The rest of
   the feature is unreachable until this exists and is the reason to schedule
   it alone.
3. Exhaustiveness for a union scrutinee.
4. Representation, drop glue, and reuse.
5. `attempt` over a row, and `raises A | B` accepted alongside `raises A + B`.
6. One release later, `+` in a row warns.
