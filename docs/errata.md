# Specification errata

Findings from implementing the front end against the language specification.
Each entry states what the spec says, why it cannot be implemented as written,
and what `crates/khora-syntax` does instead.

## 1. Generic argument order in `std.effect` and `std.ai` is corrupted

§2.1 defines the core type as `Effect<+A, -R, +E>` — value, capability row,
error channel. The listings in §3 do not follow it. Several signatures have
their type arguments shuffled and their delimiters displaced:

| As published | Intended |
| --- | --- |
| `Effect<A, Never {},>` | `Effect<A, {}, Never>` |
| `Effect<Never, E {},>` | `Effect<Never, {}, E>` |
| `Effect<T, 'r Never T label: { \| },>` | `Effect<T, { label: T \| 'r }, Never>` |
| `Effect<B, + E1 E2 R1 R2 { \| } },>` | `Effect<B, { R1 \| R2 }, E1 + E2>` |
| `Layer<R1, E2 R2,>` | `Layer<R1, R2, E2>` |
| `Tensor<D: Device, Scalar Shape: Tuple, Type:>` | `Tensor<D: Device, Shape: Tuple, T: Scalar>` |
| `matmul<D: Device, Int, K: M: N: Scalar T: const>` | `matmul<D: Device, const M: Int, const K: Int, const N: Int, T: Scalar>` |

The corruption looks mechanical — tokens rotated within each parameter list —
and one signature survived intact: `embed: ... -> Effect<Embedding<Dim, F32>, {}, ModelError>`
in `LLMService`. That surviving line agrees with §2.1, so `A, R, E` is taken as
authoritative and every signature in `std/` is written in that order.

`std/effect.kh` also adds `map_error`, which §4.2 calls three times but §3.1
never declares.

## 2. `->` is used by the grammar the lexical rules forbid

§1.1 says "No `::` or `->` symbol clutter", but `FunctionType` in §1.2 and every
function signature in §3 use `->`. Implemented with `->`; the prohibition is
read as applying to path separators only.

## 3. Capability references (`:label.member`) are undeclared syntax

§4.2 writes `ask(:ledger.get_history)` and `ask(:ai.extract(_, AnalysisReport.spec))`.
No production in §1.2 introduces a leading `:`. It is parsed as
`CapabilityExpr ::= ":" Path`, producing a `CAPABILITY_EXPR` node.

There is a second, deeper problem here that the parser cannot settle. Under the
pipe rule in §1.1, `x |> ask(:ledger.get_history)` desugars to
`ask(x, :ledger.get_history)`, but §3.1 declares `ask` as taking a single
`Label`. Either `ask` is variadic in a way the signature does not show, or the
intended spelling is `x |> :ledger.get_history`. The reference program is
transcribed verbatim, so this is a *type* error waiting for `khora-types`, not a
syntax error.

## 4. `LayerDecl` is referenced but never defined

`TopLevelDecl ::= TypeDecl | FunctionDecl | LayerDecl | LetDecl` in §1.2, yet no
`LayerDecl` production exists, and §4.2 declares layers as ordinary `let`
bindings with a `Layer<...>` annotation. `LayerDecl` is dropped; layers are
`LetDecl`s.

## 5. Opaque type and signature-only declarations have no production

§1.2 requires `TypeDecl` to have `= TypeDef` and `FunctionDecl` to have
`= BlockExpr`. §3 relies on neither: `export type Effect<+A, -R, +E>;` declares an
abstract type, and `export fn succeed<A>(value: A) -> Effect<A, {}, Never>;`
declares a signature with no body. Both right-hand sides are optional in the
implemented grammar.

## 6. Misplaced parenthesis in `VariantType`

Published: `VariantType ::= ( "|" Ident ( "(" RecordFields | TupleFields ")" )? )+`

The alternation straddles the parentheses, so `"(" RecordFields` and
`TupleFields ")"` are the two branches. Corrected to
`"(" ( RecordFields | TupleFields ) ")"`.

## 7. `PlaceholderExpr` is used but never defined

`PipeExpr` refers to `PlaceholderExpr`; no production defines it. Implemented as
a bare `_` in any argument position, yielding `PLACEHOLDER_EXPR`. Binding it to
the piped value is a lowering concern (`khora-hir`), not a syntactic one.

## 8. Row merge and error union need surface syntax

§2.2 specifies `R_combined = R1 ∪ R2` and `E_combined = E1 ∪ E2` but gives no
notation. The residue in the corrupted signatures (`{ | }` and a stray `+`)
suggests two different spellings were intended, and that is what is implemented:
`{ R1 | R2 }` for row merge, `E1 + E2` for error union.

## 9. `{` is ambiguous between a record literal and a block

Both `RecordInit` and `BlockExpr` are `PrimaryExpr` alternatives, so
`match x { ... }` and `f({ a: 1 })` cannot both parse under an LL(1) reading.
Resolved with two rules:

- `{` opens a record literal when it is immediately followed by `}` or by
  `Ident :`; otherwise it opens a block.
- Inside a `match` scrutinee, `{` always opens the arm list. Wrap the scrutinee
  in parentheses to pass a record literal.

## 10. Member functions used by the reference program are never declared

§4.2 calls `Prompt.new`, `Prompt.system`, `Prompt.user`, `Layer.succeed`,
`Layer.merge`, `Tensor.zeros`, `Response.json`, `Router.new`, `Router.post`,
`Router.listen`, `AnalysisReport.spec` and `params.get`. None appear in §3, and
the spec never says whether `Type.member` denotes an associated item, a module
function, or a record projection — even though §1.1 gives all three the same
`.` spelling.

The parser therefore does not commit: `a.b.c` in expression position is a
`FIELD_EXPR` chain, and name resolution in `khora-hir` decides what each link
means. Deciding that is a prerequisite for the type checker.

That last paragraph is superseded by entry 13. With `::` separating compile-time
paths from runtime projection, `Prompt::new` is a path the parser can build
outright and only `params.get` is left for name resolution to classify. The
declarations themselves are still missing from §3.

`std/net/http.kh` is not specified at all; the signatures there are reconstructed
from usage and should be treated as provisional.

## 11. `std.effect` is renamed to `std.core`

Direct-style effects (roadmap A8, `docs/design/effects.md`) removed the `Effect`
type the module was named after, and `effect` became a reserved word — so
`module std.effect;` no longer lexes as a path. The module now holds `Option`,
`Result`, `Never`, `Handler<E>` and the `Scope` effect, and is named
accordingly.

`examples/risk_analyzer/khora.toml` was renamed to match. Manifest dependency
keys stay dotted: they name packages, not source paths, so they are unaffected
by the `::` split in entry 13.

## 12. `Never` and `Label` are used but never declared

`std.core` refers to both without defining them; `Float`, `Int`, `String` and
`List` are likewise assumed. They are declared as opaque types in `std/core.kh`
so the corpus is self-contained; the real definitions belong in a `std.prelude`.

## 13. Universal dot is replaced by `::` for paths and `.` for projection

§1.1 mandates "Universal Dot Notation (`.`): Consistent `.` symbol for
namespaces, static enum constructors, record field access, and method
invocations. No `::` or `->` symbol clutter." That rule is not implemented.
Khora has two separators, split by *when* the thing on the left exists:

- **`::` — compile-time paths.** Module paths, types, associated items and enum
  constructors: `std::core::Option`, `RiskLevel::Critical("freeze")`,
  `Prompt::new()`, `import std::core::{Option};`, `module app::main;`.
- **`.` — runtime projection.** Record fields and method calls: `report.risk`,
  `ledger.get_history(id)`, `req.params.get("id")`.

The specification's argument for one dot is symbol economy, and that is the
wrong thing to economise on. `Foo.bar` is unreadable to a *human*: nothing in it
says whether `Foo` is a module, a type or a variable, so a reader who does not
already know the codebase cannot tell a namespace lookup from a field load, and
neither can a tool. Rust's `::`/`.` split is not clutter — it carries
information. Everything left of `::` is resolved by the compiler and gone before
the program runs; everything left of `.` is a value that exists while it runs.
One character buys that distinction at every use site, and Go, Rust and
TypeScript all draw the same line in one spelling or another, so it is what the
audience in `docs/vision.md` already reads for.

Four consequences, all improvements:

- **D2 largely dissolves.** Four meanings of one operator become two
  syntactically distinct groups, and what is left is one resolution rule per
  group instead of a four-way ordering. `docs/design/associated-items.md` states
  them.
- **The parser builds real paths.** `a::b::c` is a `PATH` node with three
  segments, decided at parse time, rather than a `FIELD_EXPR` chain handed
  wholesale to name resolution (entry 10).
- **Imports and module declarations get an honest separator.** `import a.b.{X};`
  needed the final `.` before `{` special-cased in the grammar because it was
  not a projection at all; `import a::b::{X};` needs no such exception.
- **A regex can color paths.** The VS Code TextMate grammar could not
  distinguish `Effect.map` from `report.risk` from `RiskLevel.Low`, and the
  precise answer was deferred to LSP semantic tokens in phase 8.4. It no longer
  has to be: an identifier followed by `::` is a path segment and can be nothing
  else. Semantic tokens remain the right answer for locals and bare type names;
  they are no longer needed to get paths right.

This is the second override of §1.1's "No `::` or `->` symbol clutter" — `->`
was the first, in entry 2. The clause is now dead in both halves, and the
lexical rule it belonged to should be read as describing an early intention
rather than the language.

## 14. Tensor shapes need tuple types, which nothing else forced

`docs/project.md` §3 writes the shape of a tensor as a tuple: `Tensor<D, (M, K),
T>`. Tuples parsed and lowered to HIR from Phase 1, but the checker typed every
one of them `Unknown`, and `Unknown` unifies with anything so that no error
cascades from an earlier one. The two facts together meant `matmul(a, b)` type
checked for *any* pair of tensors — the shape argument was not being compared at
all, and no test failed, because nothing had ever asked the checker a question
about a tuple.

Const generics on their own would not have fixed this. `Matrix<M, K>` with the
dimensions as direct type arguments checks correctly with only `Type::Const`;
`Tensor<D, (M, K), T>` needs `Type::Tuple` as well, or the const arguments are
buried inside a type the unifier walks straight past. Phase 3's exit criterion
is stated against `matmul`, so both had to land together for it to mean
anything.

Tuples are now real in the type system — width and component types are checked,
they nest, they carry type parameters, and destructuring binds each name at its
own component's type. They still have no runtime representation: `khora build`
reports that tuple literals are not supported yet, which is the same honest
refusal it already gave for list literals, and is unchanged by this entry.

## 15. `khora check` never type checked

The command printed "checked 1 file(s): no syntax errors" for a program with a
type error in it, because it only ever ran the parser. The type checker, the
name resolver and their diagnostics all existed and were tested; nothing wired
them to the command named after the job. A clean exit on a broken program is
the worst failure mode available to a checker, and it survived because every
test for type errors called the library directly.

`check` now reports both, and `build` renders its semantic errors through the
same renderer instead of printing bare `error: message` lines with no span.
`crates/khora-cli/tests/check.rs` runs the real binary, which is the only level
at which the gap was visible.

Two adjacent claims turned out to be false in the same way, and are now
corrected rather than restated:

- The workspace declared `rust-version = "1.80"`. salsa 0.28 requires 1.85, so
  the project had never built on the version it advertised.
- `build`'s doc comment already claimed semantic errors went through `check`'s
  renderer. They did not, until now.

## 16. The codebase is not `cargo fmt` output

Running `cargo fmt --all` rewrites roughly 2,200 lines across 46 files, and the
rewrite goes in both directions: it expands struct-variant declarations the
codebase writes on one line, and collapses signatures the codebase wraps. No
`rustfmt.toml` setting reconciles the two, because the style is hand-maintained
and was never rustfmt's output to begin with.

This is worth knowing before someone reformats the tree inside an unrelated
commit. Adopting `cargo fmt` is a reasonable decision, but it is its own commit
and its own decision, not a side effect of touching a file.

## 17. The orphan rule is decided but cannot yet be checked

`docs/design/typeclasses.md` settles coherence as Rust's: one impl per trait per
type, nominal resolution, and the orphan rule — an impl is allowed only where
the trait or the type is local. The first two are enforced. The third is not,
and enforcing it today would be wrong rather than merely incomplete.

The reason is that `type_map` is per file. A trait is known only if the file
being checked declares it, so `impl Show for Int` in a program that imports
`Show` from `std` would see neither a local trait nor a local type and be
rejected — not because it is an orphan, but because the compiler cannot yet
tell where `Show` came from. The check lands with cross-package trait
resolution, which is also what makes it meaningful: an orphan impl is only a
hazard when two packages can supply one.

Nothing about the decision changes. What is recorded here is that the rule is
currently inert, so that its absence is not mistaken for permission.

## 18. `Self` turned out to be the whole kind system

A4 committed to native higher kinds, and the expectation was a notation for
them: Scala writes `trait Functor[F[_]]`, Haskell allows an explicit kind
signature. Both put a second syntax next to the generics a reader already knows.

Neither is needed. A trait says how it uses `Self`, and that is already the
information a kind would carry:

    trait Eq      { fn eq(self, other: Self) -> Bool; }        // Self : *
    trait Functor { fn map<A, B>(self: Self<A>, ..) -> Self<B>; }  // Self : * -> *

So `impl Functor for Int` is a kind error and `impl Functor for Option` is not,
with nothing declared anywhere. The kind of every named type comes from its
parameter list for free, and const parameters give a different kind than type
parameters do — `Vector<const N: Int>` is `Int -> *`, not `* -> *`, so it cannot
stand in for a `Functor`.

Two consequences worth stating:

- **The commonest mistake has an exact fix.** `impl<A> Functor for Option<A>`
  applies the constructor one step too far. The diagnostic says so and names the
  correct spelling, because the compiler knows both kinds.
- **Default method bodies came free.** They were listed as deliberately deferred
  in the design doc, on the assumption they would complicate monomorphization.
  They did not: recording `Self: ThisTrait` as an ordinary bound on a trait's own
  signatures makes a default body's calls resolve through the machinery that was
  already there. The deferral is withdrawn.

## 19. The reference-counting planner was guessing at types

`khora-perceus` decided what to `dup` and what to `drop` from a private
`type_of` that re-derived an expression's type from its *shape*: a string
literal is a `String`, a constructor call is its ADT, a call to a named function
is that function's declared return type, and everything else is `Unknown`.
`Unknown` is not boxed, so anything it could not recognize was silently treated
as a machine word — no dup, no drop.

For the phase 2 subset this happened to be right often enough that every test
passed. Closures broke it immediately and loudly. A lambda's type is not
derivable from its shape, so a closure-typed local was never counted, and a
boxed value passed to one was released by the callee and then again by the
caller: a double free on the first program that captured a list.

The fix is not a better guess. The checker already computes and zonks the type
of every expression in the body, and `khora_types::checked` publishes it. The
planner now reads that. Two things fell out:

- `bind` no longer takes a type at all — a pattern's bindings look up their own
  types — which deleted the branch that gave every tuple-pattern binding
  `Unknown`.
- Match-arm bindings and `let` initializers get real types where they used to
  get guesses.

The general lesson is worth keeping: a second, weaker implementation of
something the compiler already knows is not a shortcut, it is a divergence
waiting for the first input that tells the two apart.

## 20. Closures were listed as out of scope and then never scheduled

Phase 2 named closures under **Out**, correctly — the vertical slice did not
need them. No later phase picked them up, and phase 3's exit criterion then
asked for `traverse`, whose signature takes a function argument. The gap was
invisible because each phase's own list was internally consistent.

Closures are now implemented, and phase 3's text says where they landed and
why. The process point is that "out of scope for phase N" and "scheduled for
phase M" are different statements, and only the first was being written down.
Anything deferred needs a destination, or it is not deferred, it is dropped.

## 21. The tie-breaker was applied to spelling instead of behavior

`docs/vision.md` said to prefer "the option more familiar to a developer who
uses Go, Rust or TypeScript", and listed "two spellings" among the things it
settles. Read literally that licenses reaching for whichever of the three
languages spells a thing most recognizably — and in practice it meant reaching
for Rust, because Rust is the one of the three with the closest feature set.

That is not what the rule is for. What a construct **does** is what a developer
predicts and what a wrong prediction costs; what it is *called* is learned once.
The two are not the same weight:

- **Novel syntax for familiar behavior is cheap.** `fn x => x + 1` is nobody
  else's lambda spelling, and it does not matter: it behaves like the lambda
  every one of the three languages has.
- **Familiar syntax for novel behavior is expensive.** It mispredicts every
  time, and a familiar word is precisely the thing that stops a reader checking.

The rule is now stated that way, with an explicit note that it is *not* an
instruction to copy Rust.

The conclusions it had already produced mostly survive, because they were
behaviorally motivated even where the stated reason was not:

- **`trait` stands**, but the argument changes completely. It was justified as
  "the concept is Rust's trait, so it gets Rust's word". The real argument is
  that `interface` — much the more familiar word, from Go and TypeScript — is
  **structural** in both, and Khora's resolution is nominal. The familiar word
  would promise the wrong behavior, so it loses to the accurate one.
- **`fn x => body` stands** and needed no defense. It is not Rust's `|x|` or
  TypeScript's `(x) =>`, and that was never a problem.

One thing it does *not* survive, recorded here because it is the kind of defect
the corrected rule is meant to catch:

- **A type could not have a method without a trait.** In Go, TypeScript and
  Rust alike, adding a method to your own type is the ordinary first thing you
  do and needs no abstraction. In Khora `impl User { fn birthday(self) .. }` was
  a syntax error: the only route to `user.birthday()` was to declare a trait and
  implement it. That is a behavioral surprise, on a daily action, for all three
  audiences at once — exactly what the rule protects, and what focusing on
  spelling caused to be missed. Now implemented; see
  `docs/design/keywords.md`.

## 22. Two semantics had been decided by implementation rather than by decision

An outside design review made one central point: the unresolved interactions —
closures, handlers, cancellation, reference counting, threads and foreign code —
matter more than any syntax question. That is right, and it converged
independently on D1, which this roadmap already called its largest unknown.

Roughly a third of the review asked for things already decided and written down:
effects inferred on private functions and required on exported ones
(`docs/design/effects.md`), higher kinds with no notation of their own
(`docs/design/typeclasses.md`), and capabilities as distinct from enforceable
sandbox permissions (D4). Those needed no change.

What it surfaced that was real is narrower and sharper than the list itself, and
it is a specific failure mode rather than a set of gaps: **two semantics had
already been settled by writing code, with no decision recording the choice.**

- **Reference counts are non-atomic.** `khora-rt` says so deliberately, in a
  module comment. A5 promises fibers across cores. Nothing anywhere reconciled
  the two, and every `dup` and `drop` already emitted assumes the
  single-threaded reading. Now D10.
- **Cycles are impossible, and nobody knew.** ADTs build bottom-up, closures
  capture by value, assignment rebinds rather than mutates, and a `let`
  initializer cannot see itself — so the heap graph is a DAG and Perceus is
  currently *complete*. That is a real guarantee that had never been stated, and
  it ends the moment mutable fields or recursive closures land. Now D11, with
  the invariant written down in `docs/design/memory.md` while it is still true.

Two smaller corrections came out of the same pass. A6's rationale implied that
sharing LLVM with Rust buys interoperability; it does not, and the rationale now
says so, deferring the actual cost to D8 where it belongs. And nothing owned
compatibility guarantees — no editions, no ABI policy, no versioning rules —
which is exactly the "deferred without a destination" pattern entry 20 named.
Now D12.

One design answer fell out of writing it up. A recursive closure appears to need
to capture itself, which is a cycle. It does not: a lifted lambda already
receives its own closure object as its first argument, so self-recursion can go
through that parameter with no capture, no refcount traffic and no cycle. The
DAG invariant survives recursive closures entirely. Mutual recursion between two
closures still needs a cycle and is a case for named functions.

The general lesson is the same one entry 19 taught in a different key: a choice
made by an implementation is still a choice, and one nobody wrote down is one
nobody can revisit.

## 23. An impl could contradict its trait, and the compiler blamed itself

`impl_signatures` reads an impl's signature from what the impl *wrote* rather
than deriving it from the trait, and its doc comment said why: "so that a
mismatch between the two is a *diagnosable difference* rather than something
the checker silently papers over."

Nothing ever read it. A trait could promise `-> Bool`, an impl return `Int`,
and the checker accept both:

```khora
export trait Eq { fn eq(self, other: Self) -> Bool; }
impl Eq for Int { fn eq(self, other: Int) -> Int { 1 } }
```

`khora check` reported no errors. `khora build` then produced:

```text
error: the generated module is not valid LLVM IR, which is a compiler bug:
  Branch condition is not 'i1' type!
```

A user error, presented as a compiler bug, at the wrong phase, with no source
span. Found while comparing two designs for `Iterator` — neither had anything
to do with it, which is the usual way.

The check now compares every impl method against the trait's declaration, with
`Self` substituted, associated types projected, and the method's own parameters
renamed positionally so `fn map<X, Y>` may implement `fn map<A, B>`.

The lesson is narrow and specific: a comment explaining why a design leaves a
door open is not the same as a test that something walks through it. Both of
the last three entries — 19, 22 and this one — are the same shape, which is
that an intention recorded in prose is not a property the compiler holds.

## 24. Reference counting was planned once for code compiled many ways

Two bugs, found by writing the first `for` loop over a heap-allocated list and
noticing it leaked one cell per iteration. Neither had anything to do with
`for`; the loop was just the first program that walked a generic container.

**Drop glue was emitted per type name, not per instantiation.** A variant's
field types come from the declaration, so `Boxed<A>`'s field is `A` — a rigid
parameter, and `is_boxed` says a parameter is never boxed. Asking the
declaration whether `Boxed` owns anything therefore always answered no, and
every `Boxed<String>` in every program leaked its contents. Glue is now keyed
by the instantiated type with the arguments substituted in, so `Boxed<String>`
gets a routine and `Boxed<Int>` correctly gets none.

**Reference-counting plans were computed once per source function.** Same root
cause, other side. `khora-perceus` planned a body from the types it was
*written* at, where `A` is rigid and unboxed — so a generic function never
duplicated or released anything held in a type parameter. `plan` now takes the
types as an argument and code generation calls it once per specialization.

The two hid each other. A generic container never released its payload, which
exactly compensated for a generic function never retaining what it stored
there, so programs leaked instead of crashing. Fixing the glue alone turned
`traverse` from correct-and-leaky into a use-after-free that stopped printing
halfway. That is worth remembering: in a reference-counting runtime, two
opposite errors can look like correctness, and fixing one is a regression until
the other is fixed too.

The general shape is the same as entry 19 — something the compiler already
knows, recomputed worse somewhere else. Here it is not a second implementation
but a second *time*: a property was computed at the wrong stage, before
monomorphization had said what the types actually were.

## 25. A constructor was found by its bare name

`Resolution::Variant` carries both the type and the case — `Maybe::Some`
resolves to `{ type_name: "Maybe", name: "Some" }`, correctly. Every consumer
then called `variant_name()`, kept only `"Some"`, and looked it up across the
whole program with first-match-wins.

```khora
export type Option<A> = | Some(value: A) | None;
export type Maybe<A>  = | Some(value: A) | None;
fn f() -> Maybe<Int> { Maybe::Some(1) }
```

> error: this function returns `Maybe<Int>`, but its body has type `Option<Int>`

A type error caught that one only because the two types differ. A tag is an
*index within one type's variant list*, so where the shapes line up the program
compiles with another type's tag and a `match` takes the wrong arm — silently,
at runtime.

Found while checking whether the language was ready for a standard library. It
was the reason it was not: `std` would declare `Option`, `Result`, `Step` and
`Ordering`, and their case names would then shadow those of every program that
imported it.

`variant` and `variant_name` are deleted rather than left available, in both
the type map and the backend, so nothing can reach the ambiguous lookup again.
The replacement `variant_of(type_name, case)` requires both halves.

The shape is familiar by now — the same as entries 19, 22 and 24. The
information was present and correct; a consumer threw half of it away.

## 26. An alias made the type map and the resolver disagree

`import demo::lib::{helper as first}` bound `first` in the file's scope to a
`Resolution::Item` naming `helper` — correctly, since that is what the defining
module calls it. `type_map` then copied the imported signature in under
`first`, because that is what this file calls it.

Both halves were reasonable and together they never met. The checker resolved
`first()` to a resolution naming `helper`, looked up `helper` in a map keyed by
`first`, found nothing, and inferred `Unknown` — which unifies with everything,
so `khora check` reported no errors. Code generation then failed with
"`print` shows `Int`, `Bool` and `String`; showing a `?` needs a typeclass",
naming a phase-3 feature that had nothing to do with it.

Two lessons, and the second is the one worth keeping.

The narrow one: a resolution now carries the name *this file* uses, because
every downstream map is keyed that way. Where the defining module spells it
differently, `FileScope::origin` says so, and only monomorphization asks —
it has to find the body, which lives under the original name.

The general one: **`Unknown` unifying with everything turns a lookup miss into
a clean bill of health.** It exists to stop one error cascading into ten, which
is right, but it also means any hole in name resolution shows up as silence
rather than as a diagnostic. Entry 22 said an intention in prose is not a
property the compiler holds; this is the same shape one level down — a lookup
that quietly returns nothing is indistinguishable from one that succeeded,
unless something downstream happens to need a real answer.

## 27. A callee's type was matched without resolving it

`infer_call` read the callee's type and matched `Type::Fn { .. }` against it
directly. A type variable *solved to* a function is not a `Type::Fn` until it
is followed, so a callee whose type arrived that way fell through to "not a
function" — which, being `Unknown`, unified with everything and reported
nothing.

Latent until recursive closures landed. A lambda's type used to be built after
its body, so it was always a concrete `Type::Fn`; a recursive one has to exist
*before* its body is checked, so its result is a variable the body solves, and
suddenly `let inner = outer(20)` gave `inner` a variable rather than a
function. `khora check` stayed silent and code generation failed with a message
about typeclasses.

The fix is one `shallow` call. The lesson is the same as entry 26, which is now
the third of its kind: **a lookup or a match that quietly produces nothing is
indistinguishable from one that succeeded**, because `Unknown` absorbs the
difference. Anywhere the checker pattern-matches a type's shape, it has to
follow the variables first.

## 28. A mark and the demand it answered were filed under different keys

`f()!` records two facts. The checker records a *demand* — this call reaches a
fallible function, so its row has to be subsumed by the enclosing one — and it
records the `!` *mark* that answers the demand. The demand was keyed by the
callee expression, because that is what carries the signature; the mark was
keyed by the call, because that is what the `!` is attached to in the source.
Every properly marked call therefore looked unmarked.

The failure was loud rather than silent, which is the only reason it cost
minutes instead of a day: correct programs were rejected with "this call can
raise; mark it with `!`" pointing at a call that already had one. Had the
lookup defaulted to *marked* it would have been entry 26 all over again.

The fix records the mark against both expressions. The lesson is narrower than
"resolve before matching" but comes from the same place: **two passes agreeing
about a fact is not enough if they disagree about what to file it under.** A
map whose keys come from one pass and whose lookups come from another needs
the key to be part of the interface, not an implementation detail either side
picked for its own convenience.

## 29. A lifted lambda emitted its enclosing function's calling convention

A `Lower` knew which function it was emitting as `owner`, a symbol it looked
the signature up by. For a lifted lambda that symbol is the function the lambda
was *written inside*, because that is what the closure-site table is keyed by —
so a lambda inside a fallible function asked whether it could raise, was told
yes, and returned `{ i32, i64 }` from a function whose return type was `i64`.

It surfaced the first time a handler was built inside a function that could
fail, which is the shape of every real service constructor. Before that, every
lambda that had ever been lifted happened to live in a function with no
`raises` clause.

The fix is a `raises: bool` set where the `Lower` is built — `true` from the
signature for a real function, `false` for a lambda, since a closure type
carries no error row and a lifted lambda therefore cannot raise at all. With
the last caller gone, the `signature()` helper went too: it existed only to
answer a question about `owner` that `owner` was never the right key for.

Entries 24, 26 and 27 are the same shape from different angles. This one adds
a corner they did not cover: **`owner` was not wrong, the question was.** The
field means "which function's table do I belong to", and it was read as "which
function am I", which is the same string for everything except the case that
matters.

## 30. A row variable in type position read as nothing at all

`fn mount<'r>(handler: Req -> Res with 'r)` was accepted, and so was every
argument passed to it. `type_of_syntax` read a path type by its `Path` child,
and a bare `'r` has no `Path` — it is one token — so the name came out empty,
the empty name mapped to `Unknown`, and `Unknown` unifies with everything.

The row tests passed the whole time. `with { 'e | ledger: Ledger }` reads its
tail through the same function, so the tail was `Unknown` there too; the tests
checked that a caller providing more than was asked for is accepted, and
`Unknown` accepts that along with everything else. They were green for the
wrong reason and would have stayed green if row unification had been deleted.

The fix is four lines: read `row_var()` before looking for a `Path`. What the
entry is really about is the fourth appearance of one shape — 24, 26, 27, and
now this — **`Unknown` is not a type, it is a silence**, and a lookup that
produces it has reported nothing while looking like it reported something. The
three earlier entries were about a lookup that missed. This one is about a
*parse* that missed, which is worse: the checker had the token in hand.

Worth stating as a rule rather than a fifth entry. `Unknown` should be
reachable from exactly two places — a parse error, and an error already
reported — and never from "I did not recognize this shape".
