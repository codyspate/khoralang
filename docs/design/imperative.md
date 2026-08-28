# D9 — Imperative constructs

**Status:** implemented, except generic `for`, which waits on typeclasses.

`docs/vision.md` says Khora should contend whenever a team is choosing between
Rust, Go and TypeScript. Most of those developers are not functional
programmers. They will not learn to express a loop as a fold in order to try the
language — they will close the tab.

The grammar in `docs/project.md` §1.2 had no loops, no early `return`, no
assignment, and no `if`. Everything was recursion and `match` — coherent for an
FP-only language and disqualifying for the stated audience.

## The principle

**Mutation is allowed where it is provably local. The effect system governs what
is observable.**

This is not a compromise of purity, it is what the memory model already buys.
Perceus reference counting with in-place reuse (FBIP) means mutating a uniquely
owned value is not just permitted but *free* — it compiles to a store, not a
copy. A `let mut` local that never escapes is invisible to every caller, so
nothing in the type system is weakened by allowing it.

What stays governed is effect: anything observable from outside the function —
I/O, shared state, failure — goes in the `with` or `raises` row and is checked.
So a developer gets the syntax they already know, and the guarantees come from
the row system rather than from withholding familiar constructs.

Decision A8 (direct-style effects) is the other half of this. Under a monadic
`Effect`, an effectful loop *cannot* be a loop — it has to become
`Effect.for_each` or a fold. Direct style is what makes an ordinary `for` over a
database call possible at all.

## Status

| Construct | State |
| --- | --- |
| `if` / `else if` / `else` | **Done.** Expression form; the condition is parsed with record literals suppressed so `{` opens the branch. |
| Assignment | **Done.** An expression of type `()`, right-associative and loosest-binding, so `x = a \|> b` assigns the whole pipeline. |
| `while cond { … }` | **Done.** |
| `loop { … }` with `break value` | **Done.** |
| `break` / `continue` | **Done.** Unlabeled. |
| Early `return` | **Done.** With or without a value. |
| `for x in xs { … }` | **Phase 3.** Needs the `Iterator` typeclass. |
| Compound assignment (`+=`, `-=`) | Not done. Sugar; lowest priority. |

Implementing these turned up one rule that was missing and is easy to overlook:
**a block-like expression standing in statement position needs no `;`**, the
same rule Rust uses. Without it, `if c { .. }` in the middle of a block is read
as the block's tail expression and every statement after it is orphaned. That
now applies to `if`, `match`, `while`, `loop`, a bare block, and a `with` block.

## Why `for` waits

Generic `for x in xs` needs an iteration protocol, which needs typeclasses
(decision A4, Phase 3). A concrete `for` over `List` could land now as a special
case, but shipping the special case first risks baking in a shape the general
protocol then has to match. Worth revisiting only if `for` over `List` proves
badly missed before Phase 3.

## Interactions to get right

- **Non-local control flow through handlers.** `break`, `continue` and `return`
  crossing a handler boundary must unwind correctly and run finalizers, exactly
  as an error does. In an algebraic effect system these are naturally *also*
  effects; whether to implement them that way or as compiler primitives is part
  of D1.
- **Loop bodies and effect rows.** A loop body's effects join the enclosing
  function's row. Nothing special is needed, but inference has to thread it.
- **FBIP.** A `for` over a uniquely owned list should reuse cells in place
  (Phase 9). The loop form should not be designed in a way that blocks it.
- **Exhaustiveness.** `if` without `else` is only well typed when the branch has
  type `()`, matching the rule `match` already follows.
- **Purity is still the default.** A function with no `with` and no `raises`
  clause is pure regardless of how much local mutation its body uses. That
  property is what makes allowing mutation safe, and it should be stated in the
  language reference rather than left implicit.

## Open questions

- Is `for` sugar over an `Iterator` typeclass, or a language primitive with a
  desugaring to `next`?
- Are `break`/`continue` labeled? Rust's `'label: loop` conflicts with row
  variable syntax (`'er`), so labels need a different spelling.
- Does `loop { }` with `break value` earn its place, or does `while` plus
  recursion cover enough?
- Compound assignment (`+=`, `-=`) — worth the extra grammar, or not?

**Settled while implementing:** assignment is an expression of type `()`, as in
Rust, which keeps it in the precedence table rather than adding a statement
form. `break`/`continue` are unlabeled for now.
