# D9 — Imperative constructs

**Status:** planned, not designed in detail.

`docs/vision.md` says Khora should contend whenever a team is choosing between
Rust, Go and TypeScript. Most of those developers are not functional
programmers. They will not learn to express a loop as a fold in order to try the
language — they will close the tab.

The grammar in `docs/project.md` §1.2 has no loops, no early `return`, no
assignment, and no `if`. Everything is recursion and `match`. That is coherent
for an FP-only language and disqualifying for the stated audience.

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

## What is missing today

Confirmed against `crates/khora-syntax` — none of the following parse:

| Construct | Notes |
| --- | --- |
| `if` / `else if` / `else` | **Not in the language at all.** `if` currently exists only as a `match` guard. Even ML-family languages have a conditional expression; this is the most glaring gap. |
| Assignment | `let mut` parses, but there is no way to assign. `mut` is currently meaningless. |
| `for x in xs { … }` | Needs an iteration protocol. |
| `while cond { … }` | |
| `loop { … }` with `break value` | |
| `break` / `continue` | Labelled forms to be decided. |
| Early `return` | |
| Compound assignment (`+=`, `-=`) | Sugar; lowest priority. |

## Sequencing

Two of these are cheap and unblock everything else; the rest depend on other
phases.

**Phase 1, alongside the other front-end work.** `if`/`else`, assignment,
`while`, `loop`/`break`/`continue`, and early `return` are all self-contained
grammar and lowering work. `if` in particular should not wait — it is a
one-evening change that removes a daily papercut.

**Phase 3, after typeclasses.** Generic `for x in xs` needs an iteration
protocol, which needs typeclasses (decision A4). A concrete `for` over `List`
could land earlier as a special case, but shipping the special case first risks
baking in a shape the general protocol then has to match. Prefer waiting, unless
`for` over `List` proves badly missed in practice.

## Interactions to get right

- **Non-local control flow through handlers.** `break`, `continue` and `return`
  crossing a handler boundary must unwind correctly and run finalizers, exactly
  as an error does. In an algebraic effect system these are naturally *also*
  effects; whether to implement them that way or as compiler primitives is part
  of D1.
- **Loop bodies and effect rows.** A loop body's effects join the enclosing
  function's row. Nothing special is needed, but inference has to thread it.
- **FBIP.** A `for` over a uniquely owned list should reuse cells in place
  (Phase 6). The loop form should not be designed in a way that blocks it.
- **Exhaustiveness.** `if` without `else` is only well typed when the branch has
  type `()`, matching the rule `match` already follows.
- **Purity is still the default.** A function with no `with` and no `raises`
  clause is pure regardless of how much local mutation its body uses. That
  property is what makes allowing mutation safe, and it should be stated in the
  language reference rather than left implicit.

## Open questions

- Is `for` sugar over an `Iterator` typeclass, or a language primitive with a
  desugaring to `next`?
- Are `break`/`continue` labelled? Rust's `'label: loop` conflicts with row
  variable syntax (`'r`), so labels need a different spelling.
- Does `loop { }` with `break value` earn its place, or does `while` plus
  recursion cover enough?
- Is assignment an expression (yielding `()`) or a statement?
