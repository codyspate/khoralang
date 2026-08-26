# The `std` surface

**Status: audited once. One decision is open and it is a language question.**
Roadmap 13.11.

`docs/design/compatibility.md` lists this among the things 1.0 requires and
does not have: *every public item in `std` is a promise at 1.0, and the set has
never been reviewed with that in mind — several exist because a reference
application needed them at the time.* This is the review.

## What the surface is

390 items, counting types, traits, effects, functions and constants, across
eighteen files. Fields and variants are not counted separately, nor are the
methods of a trait impl — `impl Show for Decimal { fn show }` is the trait's
`show`, and there are fourteen of those.

## Finding 1 — a quarter of it said nothing

**94 of 390 carried no `///`.** Among them: `Eq`, `Show`, `Option`, `Result`,
`List`, `Map`, `Fibers`, `Iterator`, `Functor`, `Traversable` — which is to
say, most of what a person meets in the first hour. The generated reference
listed them with a signature and a blank space where the sentence goes.

All 94 are written now, and `khora-doc/tests/std_surface.rs` fails on the next
one. That test is the point rather than the 94: an item nobody could be
bothered to describe in one line is an item nobody has *decided* to promise,
and it should not reach 1.0 by default. The fix for a failure is to write the
line or to stop exporting the thing, which is exactly the decision this audit
exists to force.

**One was misfiled rather than missing.** The paragraph explaining `Shared` —
what it is for, why mutation is replacement, why a `Map` cannot go in one — sat
above `Changed` and ran into its doc comment, so `Changed` carried both and
`Shared` carried none. Two contiguous `///` blocks are one block, and nothing
had noticed.

## Finding 2 — `export` means nothing inside an `impl`, and that is the open one

**This is a language-surface question and this document does not settle it.**

Today, every method of an exported type is reachable from every module that can
name the type. The keyword is accepted on a method and read by nothing:

```khora
module lib;
export type Counter = { n: Int };
impl Counter {
  fn secret(self) -> Int { self.n * 2 }   // no `export`
}
```

```khora
module main;
import lib::{Counter};
fn main() -> Int { Counter::secret({ n: 21 }) }   // compiles
```

The two halves of this repository disagree about it in the way you would
expect of a keyword that does nothing. In `std`, **317 methods carry no
`export` and 46 do**. In `packages/postgres`, written later, every public
method carries it. Both are correct today and one of them will be wrong the
moment the keyword means something.

The consequence for 1.0 is not stylistic. These are all promises right now:

| | why it exists |
| --- | --- |
| `Map::rehash`, `Map::grow`, `Map::slot` | `grow`'s internals, split out for readability |
| `List::take_first` | `split`'s accumulator |
| `Chain::find`, `Chain::holds`, `Chain::without` | one bucket of a `Map`, which is itself the implementation of `Map` |
| `Dict::node`, `Dict::gather` | balancing and traversal helpers |
| `Method::rank` | an integer per variant, so `same` has something to compare |

None of those is a thing to promise for a decade. Each is documented now, and
several of the new lines say "public only because the caller is", which is an
honest description of an accident rather than a design.

**The options, as they look from here.**

1. **Make the keyword mean what it says.** A method without `export` is
   visible only inside its declaring module. Matches `export` on a top-level
   `fn`, needs no new syntax, and `packages/postgres` is already written for
   it. It is a breaking change for `std` — 317 methods would have to be
   triaged, and the ones a reference application actually calls would gain the
   keyword. That triage is the real content of this item, and it is a day's
   work rather than an afternoon's.

2. **Say that an exported type exports its methods**, and remove the keyword
   from the grammar inside an `impl`. Smallest change, breaks nothing, and
   makes `packages/postgres` wrong in a way the formatter could fix
   mechanically. It also means `std` can never have a private helper on a
   public type, which is what forced every one of the rows above.

3. **Leave it and write it down.** Not a real option before 1.0 — the whole
   point of the compatibility document is that the surface is known — but it
   is what is true today and worth naming as the default nobody chose.

Option 1 is the one that fits the vision: `docs/design/compatibility.md`'s
argument is that a promise should be deliberate, and a keyword that is
sometimes written and never read is the opposite. It is also the expensive one,
and the expense is the audit itself rather than the language change.

## Finding 3 — three names for the same idea

Constructors are spelled `new`, `of`, `empty` and `root`, and the choice is not
random but it is not stated anywhere either:

- `new` — `Map::new`, `Dict::new`, `Vector::new`, `Router::new`, `Prompt::new`.
  An empty one of something that grows.
- `of` — `Method::of`, `Request::of`, `Offset::of_minutes`, `I32::of`.
  Conversion from something else.
- `empty` — `Array::empty`, `Params::empty`. An empty one of something that
  does *not* grow.
- `root` — `Scope::root`, `Region::root`. The outermost one, of which there is
  one.

That is a coherent rule and nothing writes it down, so the next module will
guess. It belongs in a style note rather than in the compiler.

## What was not done

**No item was removed.** Removing from the surface is the other half of the
audit and it depends on Finding 2: if a method without `export` becomes
private, most of the rows in that table stop being public without anybody
deleting anything, and the ones that remain are a much shorter list to argue
about.

**Packages were not audited.** `packages/postgres` is not `std` and does not
carry `std`'s promise. The same test could be pointed at it.
