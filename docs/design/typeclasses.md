# D6 — Typeclasses: spelling, coherence, and what ships in `std`

**Status: decided.** Blocks phase 3. Settles decision D6 in `docs/roadmap.md`;
A4 already settled *whether* Khora has typeclasses and higher kinds, and this
settles how much.

---

## 1. They are spelled `trait` and `impl`

```khora
pub trait Eq {
  fn eq(self, other: Self) -> Bool;
}

impl Eq for Int {
  fn eq(self, other: Int) -> Bool { self == other }
}

fn all_same<T: Eq>(a: T, b: T, c: T) -> Bool {
  a.eq(b) && b.eq(c)
}
```

Haskell spells this `class`/`instance`. Khora does not, under the tie-breaker in
`docs/vision.md`: a developer arriving from Rust, Go or TypeScript reads
`trait`/`impl` immediately, and `class` means something else in two of those
three languages — a nominal record with inheritance, which is precisely what
this is not. The concept is Rust's trait. It gets Rust's word.

`Self` is the implementing type. Bounds are `+`-separated: `T: Eq + Ord`.
Supertraits are written as a bound on the trait: `trait Ord: Eq`.

## 2. Higher kinds fall out of applying `Self`

A trait implemented for a type *constructor* refers to `Self<A>`:

```khora
pub trait Functor {
  fn map<A, B>(self: Self<A>, f: (A) -> B) -> Self<B>;
}

impl Functor for Option {
  fn map<A, B>(self: Option<A>, f: (A) -> B) -> Option<B> {
    match self {
      Option::Some(v) => Option::Some(f(v)),
      Option::None => Option::None,
    }
  }
}
```

There is no separate higher-kinded syntax, no `F<_>` parameter, and nothing to
learn beyond "`Self` can take arguments when the trait uses it that way". The
kind of `Self` is *inferred* from the trait body — `Functor` uses `Self<A>`, so
`Self : * -> *`, so `impl Functor for Int` is a kind error and says so. This is
the whole reason A4 is tractable without the ceremony Scala needs: Khora infers
what Scala makes you declare.

Scala's `F[_]` and Haskell's explicit kind signatures were both considered. Both
put a second, unfamiliar notation next to the generics a reader already knows,
to state something the compiler can work out.

## 3. Associated types

```khora
pub trait Iterator {
  type Item;
  fn next(self) -> Option<Self::Item>;
}
```

Rust's spelling, and the reason `::` was kept for paths (`docs/errata.md` entry
13). `Iterator` needs one: the element type is a function of the iterator type,
not a free parameter the caller picks. D3's `Schema::Spec` is the same shape and
is unblocked by this.

## 4. Coherence

Three rules, all Rust's, all chosen because a resolution failure has to be
explainable in one sentence:

- **One impl per trait per type.** Overlapping impls are rejected at the point
  the second one is declared, naming the first. No specialisation, no
  most-specific-match. If two impls could apply, the program is wrong, not
  ambiguous.
- **The orphan rule.** `impl Trait for Type` is allowed only where the trait or
  the type is declared in the same package. Otherwise two packages could each
  supply `impl Show for Int`, and which one a third package got would depend on
  its dependency graph.
- **Resolution is nominal.** An impl is found because it exists and names the
  trait, never because a type happens to have methods of the right shape. Go's
  interfaces and TypeScript's are structural, so this is the one place the
  tie-breaker points two ways — but structural resolution cannot distinguish
  `Monoid` under `+` from `Monoid` under `*` for `Int`, and cannot state the
  orphan rule at all. Nominal is what the analogous feature (Rust's traits)
  does, and it is what makes the other two rules expressible.

## 5. Dispatch is static

Monomorphisation already exists (`crates/khora-types/src/mono.rs`), and it runs
after inference has solved every type argument. A bound `T: Eq` at an
instantiation where `T = Int` therefore resolves to `impl Eq for Int` at compile
time, and the call becomes a direct call to that impl's function. No dictionary
is passed, no vtable is built, and abstraction costs nothing at runtime — the
promise in `docs/vision.md`.

Dynamic dispatch (`dyn Trait`) is deliberately absent. It needs a boxed
representation and a vtable layout, it interacts with Perceus, and nothing in
phases 3 through 6 requires it. When something does, it gets its own decision.

## 6. What ships in `std`

Small on purpose. Every trait here is either required by a phase 3 exit
criterion or is something a program cannot reasonably avoid.

| Trait | Why it is in the list |
| --- | --- |
| `Eq`, `Ord` | Comparison over user types. `Ord: Eq`. |
| `Show` | Printing a user type without writing a formatter by hand. |
| `Hash` | Required before there is a hash map, which there will be. |
| `Default` | The value a container starts at; keeps `Monoid` honest. |
| `Iterator` | Exit criterion: `for` iterates a user-defined type. |
| `Functor`, `Applicative` | A4's justification is containers. `Applicative` is what `traverse` needs to sequence effects. |
| `Foldable`, `Traversable` | Exit criterion: one `traverse` over `Option`, `List` and a user type. |
| `Monoid` | `Foldable::fold` needs it, and it is the smallest useful abstraction there is. |

Deliberately **not** in v1, each for a stated reason:

- **Operator traits** (`Add`, `Sub`, …). `+` is built in for `Int` and `String`.
  Overloading it is a large coherence surface for a small gain, and no exit
  criterion asks for it.
- **`Clone`.** Perceus manages sharing; there is nothing for a user to call.
- **`Monad`.** Under A8 there is no monadic plumbing to abstract over — that is
  exactly what the direct-style decision bought. `Applicative` earns its place
  through `traverse`; `Monad` would not earn its place at all.
- **`From`/`Into`.** Conversion is worth having, but it wants coherence rules
  around blanket impls that this document deliberately does not take on.

`std/ai.kh` already writes `D: Device` and `T: Scalar` as bounds. Both become
traits under this decision, with no change to the signatures that use them.

## 7. What this leaves open

- **Default method bodies** (`fn ne(self, other: Self) -> Bool { !self.eq(other) }`)
  are familiar and cheap, but they interact with monomorphisation in a way worth
  implementing after the base case works, not alongside it.
- **Blanket impls** (`impl<T: Show> Show for List<T>`) are the natural next step
  and the reason `From`/`Into` is deferred. They need the overlap check to reason
  about impls that are not ground.
- **Where clauses.** `+`-separated bounds cover phase 3. A `where` clause is
  presentation, and can follow.
