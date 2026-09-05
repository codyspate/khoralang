# The `std` surface

**Status: audited, and the open question is answered.** Roadmap 13.11.

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

## Finding 2 — `export` meant nothing inside an `impl`. It does now

**Resolved: option 1 below, taken 2026-08-26.** A method without `export` may
only be called by the module that declares it. What follows is the state that
was found and the argument for the change; what was done is at the end.

Before, every method of an exported type was reachable from every module that
could name the type. The keyword was accepted on a method and read by nothing:

```khora
module lib;
pub type Counter = { n: Int };
impl Counter {
  fn secret(self) -> Int { self.n * 2 }   // no `export`
}
```

```khora
module main;
import lib::{Counter};
fn main() -> Int { Counter::secret({ n: 21 }) }   // compiles
```

The two halves of this repository disagreed about it in the way you would
expect of a keyword that does nothing. In `std`, **317 methods carried no
`export` and 46 did**. In `packages/postgres`, written later, every public
method carried it. Both were correct, which is how you can tell the keyword
meant nothing.

The consequence for 1.0 was not stylistic. These were all promises:

| | why it exists |
| --- | --- |
| `Map::rehash`, `Map::grow`, `Map::slot` | `grow`'s internals, split out for readability |
| `List::take_first` | `split`'s accumulator |
| `Chain::find`, `Chain::holds`, `Chain::without` | one bucket of a `Map`, which is itself the implementation of `Map` |
| `Dict::node`, `Dict::gather` | balancing and traversal helpers |
| `Method::rank` | an integer per variant, so `same` has something to compare |

None of those is a thing to promise for a decade. Each is documented, and
several of those lines said "public only because the caller is", which was an
honest description of an accident rather than a design. All of them are private
now.

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
sometimes written and never read is the opposite.

## What was done

**Option 1.** `InherentImpl` records which of its methods carry the keyword;
`import_inherent` marks the copy `foreign`, and a foreign impl answers for only
its exported methods. A hidden method's *signature* does not cross either, so
it is not merely unreachable — its type is not there to be read.

The refusal names the fix, because it is one word in a file the reader may not
have thought to open, and it names the other fix too:

    `Map::rehash` is not exported, so only the module that declares it may
    call it. Write `pub fn rehash` there if it is part of the type's
    interface — otherwise this call belongs inside that module.

**A trait impl is not filtered.** `impl Show for Decimal { fn show }` is
reachable wherever `Show` is; what makes it public is the trait, and writing
the keyword on one method of an impl would suggest the others were hidden.

**The triage cost far less than the 317 suggested**, because "317 methods to
decide about" was the wrong frame. Measured three ways:

| | |
| --- | --- |
| 92 | called from another file already — public, no judgement needed |
| 35 | called only inside the file that declares them — the candidates |
| 184 | called nowhere in this tree — API waiting for a user, and *not* evidence of anything |

That third bucket is the trap. `Option::is_some` and `Int::wrapping_add` are
uncalled here because `std`'s callers are programs that do not exist yet, and
hiding them on that evidence would have been the worst possible reading of it.

So: **241 methods gained the keyword and 24 did not**, chosen from the middle
bucket. The direction of the default is deliberate — a method wrongly hidden is
caught by the compiler the first time anything calls it, and a method wrongly
exported is a promise nobody noticed making.

The 24, each a helper of a public function on the same type: `List::take_first`
and `merge` (`sort` and `split`); `Chain::find`, `without`, `holds`;
`Dict::node`, `balance`, `gather`, `least`; `Map::slot`, `grow`, `rehash`;
`String::fold` and `compare_bytes` (`Hash` and `Ord`); `Int::digits` and
`digit`; `Decimal::align`; `Method::rank`; `Response::extra`;
`Router::serve_once`, `serve_connection`, `secure_and_serve`, `dispatch`;
`Prompt::describing`.

**One thing this exposed.** `Chain` is now an exported type with no public
methods at all, which is the reference saying out loud that it is `Map`'s
implementation. It stays exported because `Map`'s `buckets` field has type
`Array<Chain<K, V>>` and a public field's type has to be nameable — which is a
question about whether `Map`'s fields should be public, not about `Chain`.

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

**No type, trait or free function was removed.** Twenty-four methods stopped
being public, which is Finding 2's doing rather than a separate pass. Whether
`std` should export fewer *types* — `Chain` and `Halves` are the obvious
candidates — is a question this did not open.

**Packages were not audited.** `packages/postgres` is not `std` and does not
carry `std`'s promise. The same test could be pointed at it.

## Since: `std::ai` is gone

**2026-09-05.** The paragraph above says no type, trait or free function was
removed. A module's worth has been now, so the 390 this document opens with was
counted before it: **21 public items left `std`.**

`std::ai` was two modules wearing one name, and they deserved opposite answers.

**The tensor half is deleted** -- `Device`, `Scalar`, `Tuple`, `F32`, `Tensor`,
`Tensor::zeros`, `Embedding`, `matmul`, `embed`, `cosine_similarity`. Ten items,
of which five were declarations with no body. No `.kh` file in the repository
called any of them, and the module's own doc examples imported `Model` and
`Embedder` -- two types that have never existed. A shape-checked tensor is a
good idea; this was the idea rather than the thing, and 1.0 would have promised
it for a decade. The const-generic machinery it was meant to demonstrate is
covered by `khora-types/tests/const_generics.rs`, which defines its own
`Matrix` and never imported this.

**The inference half moved to `packages/ai`** -- `Message`, `Prompt` with its
six builders, `ModelError`, `LLMService`, `extract`. Eleven items, all with
bodies and one real caller in `examples/risk_analyzer`, which still builds and
still serves a request. It is a vocabulary for an interface no two providers
agree on, and that is the argument against `std` rather than against the code:
`role` is a `String` precisely because the set of roles keeps moving, and a
package can follow that where a compatibility promise cannot.

**What this changes about the audit.** Finding 1 asked whether every item was
documented, and the answer made 94 items get a sentence. The better question is
the one this pass asked -- whether an item would survive a stranger asking what
it is for -- and it is not one a test can fail on.
