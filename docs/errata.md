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
`= BlockExpr`. §3 relies on neither: `pub type Effect<+A, -R, +E>;` declares an
abstract type, and `pub fn succeed<A>(value: A) -> Effect<A, {}, Never>;`
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

`std/net/http_native.kh` is not specified at all; the signatures there are reconstructed
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
  precise answer was deferred to LSP semantic tokens in phase 10.4. It no longer
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
own component's type.

**They had no runtime representation for a long time after that**, and this
entry used to end by saying so: `khora build` reported that tuple literals were
not supported yet, which was the same honest refusal it gave for list literals.
Both refusals are gone. A tuple is an anonymous record — one heap object with
positional fields — and `[a, b, c]` is a `List::Cons` chain. Roadmap 9.5.1 and
D13.

Worth keeping as its own lesson, because the gap lasted through five phases:
**a type the checker understands and the backend cannot represent is a feature
that parses, type-checks, and then fails at the end of the pipeline.** That is
the worst place for a refusal to live, and it took a stranger's-first-afternoon
audit to notice that three of the four things on that list had the same
shape.

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
  single-threaded reading. Became D10, and was decided in phase 5: atomic, no
  opt-out. The "every `dup` and `drop` already emitted assumes it" half of this
  entry was itself wrong — generated code never touches a refcount — which is
  why the change took thirty lines rather than a rewrite.
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
pub trait Eq { fn eq(self, other: Self) -> Bool; }
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
pub type Option<A> = | Some(value: A) | None;
pub type Maybe<A>  = | Some(value: A) | None;
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

## 31. Three features were only ever tested one file at a time

Checking the reference application against `std/` for the first time found
three holes, and none of them was subtle once seen:

- **An imported `effect` brought nothing.** `import_types` matched
  `ItemKind::Type` and `ItemKind::Trait` and had a `_ => {}` for the rest. An
  effect declares exactly what a type does — an entry in `adts`, and one
  record of operations — so the type arrived as `Unknown` and every
  `ai.extract(..)` on it read as a call to a method that was not there.
- **`with Mock { .. }` installed nothing.** Installation read the block's
  *record literal*, and a named context is not one. No `let`s were emitted, no
  labels recorded, and the requirement it was supposed to discharge stayed on
  the enclosing function — where, in a single-file test, it was declared
  anyway.
- **A bare `'r` in type position parsed as no name** (entry 30).

The pattern is not that these were hard. It is that every one of them had a
passing unit test *of the same feature*, written in one file, against one
module, with the row spelled out. `docs/roadmap.md` has said since phase 1 that
the reference application is the exit criterion for phase 4; what it did not
say is that a whole-program check is a different test, not a bigger one.

The lesson, then: **a feature that crosses modules needs a test that crosses
modules**, and it is worth writing that test before the feature looks finished
rather than after — the single-file version will pass either way, which is
precisely the problem.

## 32. A specialization kept its rows unsubstituted

`specialized_signature` substituted the type arguments into a signature's
parameters and its return type, and copied both rows across untouched. The
comment above the copy even said why the rows have to survive — the capability
row decides how many extra parameters the function takes, the error row whether
it returns a tagged value — and then dropped the substitution that makes either
of them mean anything.

Invisible while every `with` clause named its labels: a written row has nothing
to substitute. The first row-polymorphic function to reach code generation —
`fn apply_to<'r>(f: (Int) -> Int with 'r) -> Int with 'r` — was compiled as
though it needed nothing, so its caller passed evidence it never took.

Two related things were missing with it, both consequences of `with 'r` naming
nothing. The body has no binding for `ledger` and cannot mention it, so
`bind_parameters` had nothing to bind and `evidence_for` had nothing to find;
both now read the *specialization's* labels, and forward what they were handed.
And nothing released it: evidence is passed owned, and the named ones are
locals the reference-counting plan covers, so an unnamed one had no binding to
hang a plan on. It is now a temporary of the outermost scope, which is what
makes every path out — falling off the end, a `return`, a `raise` — release it.

The shape here is not `Unknown` this time. It is **a substitution applied to
some fields of a structure and not others**, which reads as correct at every
call site that never exercises the difference.

## 33. A closure could use a capability it had not captured

`apply(fn n => report(n), 4)` inside a `with { ledger: .. }` block type checked
and segfaulted. The lifted lambda read a slot for `ledger` that its frame did
not have.

A `with` block lowers to a block of `let`s, so a capability *is* an ordinary
binding, and a closure that uses one should capture it the way it captures any
other name it reads. But nothing in the body names it: `report(n)` needs
`ledger` without saying so, the requirement is discovered from the callee's
row, and the capture scan in lowering watches `Expr::Local`. The one pass that
knew a capability was in play was the checker, and it was not telling anyone.

The fix is the errata-19 shape once more: publish it. The checker records, per
lambda, the bindings its body uses implicitly, and code generation reads that
list instead of deriving a second one. Two smaller things came out with it.

**The same fact under two keys.** The checker records a demand against the
*callee* — that is what carries the signature — and the capability scope was
filed against the *call*, so the lookup missed. Entry 28 is this exact pair
disagreeing about a key, which is why the fix is the same one: record the scope
under both, and stop having to remember.

**Two lists of the same thing, one of which grew.** The closure object was
*sized* from lowering's capture list and *filled* from the site's. That was
fine for as long as the two agreed, and wrote past the end of the allocation
the moment the checker's captures were added to one of them. It now reads the
site for both, and the other list is not passed in at all — a parameter that
can disagree with the truth is a parameter worth deleting.

Worth saying plainly, because it is the fourth entry of its kind: **when two
places compute the same list, delete one.** Not "keep them in sync" — the bug
here was introduced by a change that had no reason to look at the sizing line.

## 34. A reference taken for a call was released after it

Calling a closure reads it — which dups it, because the callee is handed an
owned reference — and released it on the line after the call. That line is not
on every path out. A fallible callee leaves through the branch `!` emits, and
the branch returns before reaching it, so every raise through a closure call
leaked one reference to the closure.

Invisible until a closure could raise. Before that, a closure call had exactly
one way out.

The fix is to make it a scope rather than a line: the closure is a temporary of
a scope opened around the call, so `unwind_to` releases it on the way out and
`leave_scope` releases it on the way through. That is the same shape `match`
uses for a temporary scrutinee, and it is the third time the answer to "this
cleanup is missing on the error path" has been "it was not in a scope".

Worth stating as a rule, because it will come up again: **a reference held
across a call belongs in a scope, not in a statement after it.** Anything that
can leave early — a raise, a cancellation, a `return`, a `break` — sees the
scope and does not see the statement.

## 35. Every test passed, including the ones that failed

The test runner reported three passes for a suite whose third test asserted
`4 == 5`. The generated code was right — `ret { i32, i64 } { i32 -2, i64 0 }`,
the reserved tag for a failed assertion, plainly there in the IR — and the
runtime read the tag as zero.

A tagged return is a 16-byte aggregate, and **how a 16-byte aggregate comes
back is a target decision, made separately by LLVM for `{ i32, i64 }` and by
rustc for a `repr(C)` struct of the same shape.** On x86-64 Windows they
disagree. Nothing warns: the two sides compile independently, the link
succeeds, and the value that arrives is whatever happened to be in the register
the reader looked at.

Silent in the worst direction, too. A tag of zero means "returned normally", so
every failure read as a pass — the one wrong answer a test runner must never
give.

The fix is to stop crossing the boundary with an aggregate. A trampoline on the
*generated* side calls the function, takes the tagged pair apart where both
halves of the call are LLVM's and agree by construction, returns the tag as an
`i32` and writes the payload through a pointer. One per arity, not per callee.

The rule this leaves behind is narrow and worth keeping: **only scalars and
pointers cross between generated code and the runtime.** Everything else in
this interface already obeyed it — `khora_alloc` takes two integers, `khora_drop`
takes two pointers — and the one place that did not was the one place that
broke. It had been broken for as long as fibers had existed; the fiber tests
passed because none of them could tell the difference between "read the tag as
zero" and "the fiber finished".

## 36. An annotation nobody read

`let x: Bool = 5;` compiled clean, and had for as long as `let` had existed.
The annotation was parsed, reached HIR lowering, and was dropped on the floor:
`Stmt::Let` carried a pattern and an initializer and nothing else, so the
binding simply took whatever type the initializer had.

Nothing failed because of it, which is the whole trouble. **An annotation that
is only a comment is worse than no annotation, because it is believed** — by
the reader, and by the next person who changes the initializer and trusts the
line above it to catch them.

It survived this long because the type system never needed it. Inference is
good enough that an annotation is usually redundant, so no test was written
that turned on one, and the only annotations in the standard library happened
to agree with what was inferred anyway. It was found while making
`let b: U8 = 65` work, which is the first case where an annotation is the
*only* thing that can decide the answer.

The fix needed HIR to carry a type, which it could not: HIR sits below the type
system and cannot name a `khora_types::Type`. So it carries `TypeRef`, an echo
of the syntax — and `khora-types` resolves that through the same `named_type`
that resolves the syntax, because two interpreters of one name is how the two
come to disagree, and the disagreement would be silent.

The rule: **a feature the compiler never reads is not a feature.** Anything the
grammar accepts and the checker ignores is a promise the language is not
keeping, and the only reliable way to find one is to ask, of each piece of
syntax, which test fails when it is deleted.

## 37. An entry that stopped being itself

Pushing an expected type into a call — so that `let cells: Array<U8> =
Array::new(4, 0)` can decide `A := U8` before the `0` decides it — made a row
error appear in `std::core`'s `retry`, on a line that had not changed:
`` `E` is not accounted for here ``, against two rows that printed identically.

An error row labels each entry by *its own type's* name, since two errors of
one type cannot be told apart. An entry whose type is not known yet therefore
has no name yet, and carries the variable as a placeholder. `zonk` re-reads
those labels when the variable is solved. **Unification did not — and it
compares rows before anything zonks them.**

While the variable was solved *by* the row unification, nobody noticed: the
entry was still a placeholder when it was matched, and `pair_nameless` matched
it by position. Solving it from somewhere else first — which is exactly what
pushing a return type in does — left an entry still called `_` staring at an
identical entry called `E`. Neither matched, both rows looked short, and a
closed row that cannot grow reported a label nobody was missing.

A latent bug rather than a new one. Any solution arriving from outside the row
would have done it, and one eventually would have.

The rule, which is the same one as entry 33 from the other side: **a derived
value has exactly one place it is derived.** A label computed from a type has
to be recomputed everywhere that type can change, and the way to be sure of
that is to compute it on the way *out* rather than store it on the way in.

## 38. A test suite that linked last week's runtime

Making an array pack its elements changed `khora_array_new` to take a `stride`
and grew the array header by a field. Every array test then failed, in a way
that looked exactly like a code generator bug: an integer element dropped as if
it were a pointer, `misaligned pointer dereference: address must be a multiple
of 0x8 but is 0xa` — `0xa` being the number 10, which the program had just
stored in the array.

The generated IR was correct, and reading it proved nothing was wrong with it.
What was wrong was on the other side of the link. **Generated executables link
`khora-rt`'s `staticlib`, and `cargo test` does not build that** — it builds
the rlib, because that is what a dependency needs. The archive on disk was
forty minutes old. A program calling a five-argument function against an
archive whose copy took four arguments read the stride as the `boxed` flag,
concluded every element was a counted pointer, and released the integers.

One test binary, `compile.rs`, did know to build the archive first. That made
it worse rather than better: cargo runs test binaries in parallel, so whether
any *other* binary saw a current archive was a race it usually won and
sometimes did not. A suite that is right most of the time is how a real bug
gets attributed to flakiness.

The fix is a shared `tests/harness` every test binary that links a program
calls. Cargo takes its own build lock, so the second caller finds the work
already done.

The rule: **when two artifacts have to agree, one build step has to produce
both.** Until that is true, the check belongs at the start of every consumer,
not at the start of one of them.

## 39. A bound that parsed and meant nothing

`impl<K: Hash, V> Map<K, V>` was accepted by the grammar, and every method
inside it was then told that `K` "is a type the caller chooses and **has no
bounds**". The bound on a `fn`'s own parameters was read; the bound on the
enclosing impl block's parameters was replaced, on the way into the method's
signature, with `vec![Vec::new(); generics.len()]`.

Not an oversight in the parser — `bound_lists` existed and worked. The impl
paths simply never called it, and reserved the right shape of the wrong data.

The same species as entry 36, and worth pairing with it: **syntax the compiler
accepts and then ignores is a promise the language is not keeping.** Both were
found the same way, by trying to write something that could not be written any
other way — `let b: U8 = 65` for one, a hash map with string keys for the
other. Inference and monomorphization are good enough that neither gap had ever
been load-bearing before.

What makes this one worse than 36 is the diagnostic. It did not say "bounds on
impl blocks are not supported"; it said the parameter *has no bounds*, which is
a statement about the user's code and was false. A compiler that reports a
missing feature as a user error sends the reader to fix the wrong file.

## 40. A name with no type, and nothing complained

`let mock = handler for Ledger { .. }` at module level parsed, resolved, and
had no type. The name reached the item table — which is why every mention of it
resolved — and then nothing else happened to it: no body was lowered, no
signature was recorded, and a reference typed as `Unknown`.

`Unknown` is compatible with everything, so the checker was silent. `khora check`
on the reference application said "no errors" while five of the six things
standing between it and a binary were caused by this one gap. The first sign
was the *code generator* saying it could not represent the type of a binding
nobody had worked out — a message about the backend, three layers away from the
cause, naming a variable the author never wrote.

The fix decides what a module-level `let` *is*: a **constant**, lowered
wherever it is mentioned. Rust's `const` rather than its `static`, and the
choice pays for itself three ways — there is no initialization order to get
wrong, nothing to release at exit, and no shared state for two fibers to reach.
It cost one new rule (`let mut` at module level is refused, because a mutable
global is exactly the thing `memory.md` §5a will not let cross) and one guard
against `let a = b; let b = a;`, which inlining would otherwise turn into a
stack overflow.

*Later:* the spelling caught up with the decision. A module-level binding is
written `const` now, because calling it `let` left one word covering two
different constructs and no way to tell them apart except by indentation. The
semantics here are unchanged — this entry is what they are. See
`docs/design/keywords.md`.

The rule is the fourth entry to say it, and this is the plainest case yet:
**`Unknown` is a silence, not a type.** Entries 24, 26, 27 and 30 are the same
sentence about different holes. What is new here is the *distance*: the other
four were caught by a test that was green for the wrong reason, and this one
was caught by a completely different subsystem, in a message that pointed at
neither the declaration nor the mention.

It suggests the check worth having is not "did anything report an error" but
**"is any published type `Unknown`?"** A body the checker finished with an
`Unknown` in it is a body the checker did not understand, and saying so at the
end of checking would have caught all five of these where they happened.

## 41. Four entries, one check

Entries 24, 26, 27, 30 and 40 are the same sentence about different holes:
**`Unknown` is a silence, not a type.** It is compatible with everything, which
is what makes it right downstream of an error — one mistake should not become
five — and exactly what makes it invisible when nothing went wrong.

Five times was enough. The checker now refuses to finish a body with an
`Unknown` in it: if one is left and nothing else was reported, that is either a
program nobody can type or a gap in the compiler, and both deserve a sentence
where they happen rather than a symptom three subsystems away.

Two exemptions, and both are the rule rather than holes in it. After an error —
this pass's own, HIR's, or the parser's — `Unknown` is doing its job, and
saying so again would bury the message worth reading. And the report names the
*narrowest* expression, because an unknown type makes the block around it
unknown too and the innermost one is where the trail starts.

Turning it on found two things immediately.

**The reference application does not typecheck**, and has not for as long as
anyone has been claiming it does. `ai.extract` is declared
`forall <A: Extract> . (Prompt, A::Spec) -> A`; the checker had nowhere to put
the `A`, produced `Unknown`, and `Unknown` agreed with everything downstream of
it. Phase 4's exit criterion was met the same way entry 24's test was green.
The test now asserts what is true — everything fits *except* the one construct
nobody has decided how to compile — and goes back to `is_empty` when it is
decided.

**A `loop` had no type.** The comment said so plainly — *"a `loop` yields
whatever `break` carries; without tracking that in phase 2 it is left open
rather than guessed"* — and left open meant `Unknown`, which meant
`let n: Bool = loop { break 1 };` was accepted. A loop now takes the type its
`break`s agree on, or `()` when none of them carries a value.

The lesson is about the shape of the check rather than any of the bugs.
Asking *"did anything report an error?"* is asking the compiler whether it
noticed a problem. Asking **"is any published type `Unknown`?"** is asking
whether it understood the program, which is a different and better question —
and it is the one that would have caught all five.

## 42. The intrinsic that ate a standard library function

`Int::to_string` is written in `std::core`, in Khora, in four lines. Calling it
crashed the compiler: *"Found PointerValue but expected the IntValue variant"*.

The code generator recognises `Int::` methods by their owner and sends them to
`int_intrinsic`, which implements `wrapping_add`, `xor`, `shl` and the rest —
all two-argument integer operations. `Int::to_string` is a one-argument method
returning a `String`, so the second argument it did not have was read as an
`i64` that it was not.

This is the *second* time. `attempt` had it in phase 5: a program with its own
function called `attempt` got the intrinsic instead. That was fixed by checking
`!self.be.is_defined(&symbol)` at the one call site that had the problem, which
fixed the symptom and left the shape of the bug in place for the next name to
collide.

The rule, now applied once and before all of them:

> **A method somebody wrote wins over one the backend implements.** An
> intrinsic is a *declaration the backend fills in*, so the test is that
> nothing else filled it in first.

The general form is worth keeping in view: **a table keyed on a name is a
collision waiting for the name.** The other keys in that table — `Array`,
`String`, `Ptr`, `Region`, `Fiber` — were all one standard-library function away
from the same crash, and none of them had a test for it, because the intrinsics
and the library are written by the same person on different days.

## 43. `\r\n` was four bytes

The reference application served its first HTTP response and no client could
read it. The status line arrived as

```text
HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n...
```

— one line, with a literal backslash where each carriage return should have
been. **Khora's string literals did not process escapes at all.** The lowering
was `text.trim_matches('"')`, so `"\r\n"` was four characters and every one of
them went on the wire.

Nothing had noticed because nothing had needed one. Every string in the
standard library, the test corpus and the reference application was plain text
until a protocol turned up whose separator is not typeable. The one place a
missing escape would have been caught earlier — a test asserting output with a
newline in it — always wrote the newline on the *Rust* side.

`trim_matches` was a second bug in the same line, waiting: it strips *every*
leading and trailing quote, so `""""` lost more than the two it should have.

The lesson is not "handle escapes", which is obvious. It is that **a feature
nothing exercises is a feature nobody has checked**, and the way to find that
class is to ask what the corpus has never contained. This one was found by an
HTTP client refusing to parse a response — three layers and a socket away from
a `trim_matches` call.

## 44. A one-byte slot and an eight-byte store

`Option::Some(true).unwrap_or(false)` leaked an object. `Option::Some(7)` and
`Option::Some(1.5)` did not. That is a strange enough shape to be worth
following, and what was underneath it was not a leak.

Binding a variant's payload read the field at the type the *variant declared*.
`Option::Some(value: A)` declares `A`, and `A` has no machine type, so the read
fell back to `i64`:

```llvm
%v = alloca i1, align 1                    ; the binding, one byte
%field = load i64, ptr %field.ptr          ; the payload, eight
store i64 %field, ptr %v, align 8          ; seven bytes into the frame
```

The seven bytes went over whatever the frame had put next, which was the slot
holding the scrutinee. So the release at the end of the `match` dropped a
pointer that had been overwritten, and the object was never freed. **The leak
was the symptom; the bug was a stack buffer overflow**, and it had been there
for every payload narrower than a word since generics landed.

Nothing caught it because everything else is word-sized *by accident*. `Int` is
eight bytes and matches. `Float` is eight bytes, so the bits make the round trip
through an `i64` and come back correct — right for the wrong reason, and a test
that only checked the value would have passed. `Bool` is the one type where the
slot is smaller than the store, and `U8` through `I32` would have been too, had
anything put one in an `Option` yet.

The fix is to stop asking the declaration. The checker already recorded the
specialized type of every bound local — `v` in `Option::Some(v)` at
`Option<Bool>` is a `Bool` and the checker knows it — so the binding's own type
is right there and always exact.

Two rules, and the second is the general one:

**A type parameter has no machine type, and a default is a guess.** Falling back
to `i64` reads as caution and is not: it is correct for word-sized things and
silent memory corruption for everything else. Where a type must be known, it has
to be *looked up*, not assumed.

**An accident that holds for every case you tried is not a rule.** Four of the
five payload types worked, one of them for a reason that had nothing to do with
being right. The way to find that class is to ask which case is *shaped*
differently rather than which case failed — and a narrow integer is shaped
differently from everything else the language had.

Found by the agent writing `std::json`, from a single stubborn `1` in a live
count where every other number in the file was `0`.

## 45. Every program shipped with a debug runtime

`khora build` produced executables in which `khora_alloc`, `khora_drop`,
`khora_str_find` and every other runtime entry point were compiled at
`opt-level = 0`. Not in some configurations — in all of them, for as long as the
runtime has existed.

`toolchain::runtime_archive` looks for `khora_rt.lib` beside the running
compiler and then one directory up. A compiler built with `cargo build` sits in
`target/debug`, and beside `target/debug/khora.exe` is `target/debug/khora_rt.lib`.
The search is correct and does exactly what it says. The problem is what it
implies: the runtime's optimisation level tracked **how the compiler was built**
rather than what it is for, and nothing in the profile said otherwise.

Measured on parsing an eighty-byte HTTP request: 9,000 nanoseconds against
3,650. Two and a half times, on every Khora program, from a setting nobody had
thought to write down.

The fix is one stanza:

```toml
[profile.dev.package.khora-rt]
opt-level = 3
```

It was found while chasing something else. `String::find` is a call to
`memmem`; it measured 315 nanoseconds against an expected 40, which is the kind
of gap that means the thing being measured is not the thing you think it is.
Chasing *that* found a whole archive compiled without optimisation.

**A compiler's output is not the compiler.** The runtime is an artifact the
toolchain ships, and it should be built the way a shipped artifact is built no
matter how the tool that ships it was compiled. Anything else makes a
user-visible property of their program depend on a developer's build command.

The general shape is worth more than the fix: **a benchmark that is off by a
constant factor everywhere is a configuration bug, not a code bug.** Every
number in the string benchmarks was 2.5× too slow together, which reads as "this
language is slow" and is actually "one flag is missing". A single primitive
measured against what the platform can do — one call to `memmem`, whose cost is
known — is what turned an atmosphere into a number.

## 46. A type is a string, and two strings are equal

`Type::Adt { name: String, args }`. That is the whole of what the checker knows
about a declared type — a name, as the file using it happens to spell it. Three
things follow, and none of them was written down anywhere.

**A user type called `Array` was given the runtime's array layout.** Not
"received no intrinsic": the backend matches `name == runtime::ARRAY_TYPE`, so
a four-line record inherited an array's header, and dropping one read its first
field as an element width. Measured, not reasoned about:

```text
thread panicked at khora-rt/src/lib.rs:2322:
assertion `left == right` failed: a counted element is a pointer, so it is
always a whole word wide
  left: 26740419144712271
 right: 8
```

A program that never mentions an array, aborting inside the collector with a
message about pointers. The same is true of `Shared`, `Fiber`, `SharedFn`,
`Fibers`, the `Share` trait, and — through `named_type`, which answers before
it consults the file — `Int`, `Float`, `Bool`, `String` and `Ptr`.

**Two modules that each declare a `Point` are one type.** The importing file
looks its fields up by name, finds the local declaration, and reports that
`Point` has no field `label` about a value whose type has exactly that field.
No `unsafe` reading, no crash, just the wrong answer to an ordinary question.

**An alias splits one type in two.** `import other::{Point as Other}` keys the
imported declaration under `Other`, so `Other` and `other::Point` no longer
unify — the rename invented a type.

All three are one cause. `TypeMap` is per file and keyed by *local spelling*
throughout: `adts`, `variants`, `signatures`, `kinds`, `declared_here`, and the
head of every impl. That is a coherent design for a single module and it has no
way to express "the `Point` from over there".

**The fix, and then the guard that outlived it.**

A `Type::Adt` carries the module that declares it now, resolved at
`named_type` — the one place a type name has ever meant something, which is
what made this tractable. Two modules may each declare a `Point` and they are
two types; an alias resolves to the declared name, so `Point as Other` is the
type it renames rather than a new one. Unification compares the module, a
mangled symbol carries it, and every lookup driven by a `Type` — a field read,
a field write, a record literal, a constructor, a tag — asks by declaration
rather than by spelling.

Four places had to learn it beyond the obvious. `Resolution::Variant` carried
the module of the file *mentioning* a constructor rather than the one declaring
it, so `List::Nil` written in `std::ai` claimed `List` was declared there — a
lie nothing had read closely enough to mind. The backend's `merged_types`
deduplicated variants by `(type_name, name)`, keeping whichever module merged
first and dropping the other. And record construction and field access each
kept their own lookup by name, which is why the first version compiled and then
segfaulted: the literal stored nothing, because the layout it was matched
against had no such field, and the read found garbage.

**The guard from before stays, and is no longer a workaround.** A name the
compiler already means may not be given a definition. Identity fixed the
general collision but the *backend* still recognises `Array`, `Shared`, `Fiber`
and the rest by bare name — a smaller and more contained thing than the type
system did, and one that will only go when those declarations get an identity
the code generator can ask about. Until then a `type Array = { .. }` would
still be handed the runtime's layout, so it is still refused.

The rule this belongs to is not "`Unknown` is a silence" but its neighbour:
**an identifier is not an identity.** Entry 45 said a benchmark off by a
constant factor everywhere is a configuration bug; this one says a lookup that
is right whenever you only tried one module is not a lookup, it is a
coincidence. The way to find that class is to ask what happens when two things
have the same name — which costs one four-line test file per table.

## 47. A no-op runtime measures a program with a leak

Phase 9.3 was to be measured before it was written — entry 45's rule, and the
roadmap said so in the entry itself. The measurement was an ablation: build a
throwaway runtime in which `khora_dup` and `khora_drop` return immediately, and
see how much faster the benchmark runs. Whatever that gap is, it is the ceiling
on anything that makes reference counting cheaper.

It ran **slower**. 1,910 nanoseconds against 1,670 on an HTTP request parse,
consistently, in the direction that should have been impossible.

Nothing was being freed. The benchmark parses the same request a million times,
and with the drops gone every one of those fifty allocations stayed live
forever: fifty million objects, an allocator walking an ever-longer free list,
and a working set that stopped fitting in any cache long before the run
finished. The ablation removed a cost and added a larger one.

**An ablation has to preserve the invariant the thing being ablated exists to
maintain.** Reference counting is not overhead attached to a program that would
otherwise be correct; it is what makes the memory behaviour bounded. Removing it
does not produce the same program without a cost, it produces a different
program with a leak, and the number that comes back is about the leak.

The measurement that worked was the ordinary one: build the optimisation, run
the benchmark, compare. The two halves were then separable because they are
different mechanisms — inlining the counter arithmetic (§3) and dropping the
atomics (§4) — and each could be measured against the state before it.

There is a second correction in the same investigation, and it is the more
useful one. The roadmap called drop specialization "the cheapest of the four and
the least interesting", reasoning about the *work* the runtime does on a drop:
the field count and the layout are compile-time constants, so a specialized drop
saves a little arithmetic. It was the second largest win in the phase, because
what cost was not the work but the **call**. An HTTP parse performs 280
reference-count operations against 50 allocations, and 230 of those calls did
nothing but add or subtract one from a word.

**The cost of an operation includes the cost of reaching it.** A function whose
body is three instructions is not a three-instruction function, and the ratio
that says so — 280 operations to 50 allocations — was sitting in the design
document for weeks before anybody read it as an argument about call overhead.

## 48. A guarantee that expired without anybody noticing

`docs/design/memory.md` opened with a section called "The invariant that
currently holds", and inside it:

> **Perceus reference counting is currently complete.** No object can leak,
> because the only way reference counting leaks is a cycle, and there is no way
> to build one.

That was true and load-bearing when it was written. The heap graph was a DAG
because constructors build bottom-up, closures capture by value, assignment
rebinds a name rather than mutating an object, and a `let` initializer cannot
see its own binding. No cycle, therefore no leak.

**Phase 6.1 added mutable fields and it stopped being true.** This compiles
today and leaves four objects alive:

```khora
pub type Node = { name: String, mut next: Option<Node> };

let a: Node = { name: "a", next: Option::None };
let b: Node = { name: "b", next: Option::None };
a.next = Option::Some(b);
b.next = Option::Some(a);
```

The document was not wrong about *that*. Section 4, ninety lines below, says
plainly: "Decided in phase 6, and the DAG is gone." Both halves were written by
somebody who understood the situation exactly.

**The defect was the ordering, and it is a documentation failure rather than a
code one.** A guarantee stated in the present tense at the top of a file, and
retracted ninety lines down, is a guarantee that will be quoted without its
retraction — by a reader in a hurry, by anybody grepping for "leak", and by
whoever writes the marketing page. Section 1 is now titled "The invariant that
used to hold" and carries the counter-example, so the two halves cannot be
separated.

**A claim in the present tense is a claim with an expiry date, and nothing
prints the date.** Errata 45 said a benchmark off by a constant factor
everywhere is a configuration bug; errata 46 said an identifier is not an
identity. This one says: *a document that records both a rule and the thing
that will end it has to put them in that order, because the second one is what
makes the first safe to read.* The cheap version of the discipline is to write
the retraction where the claim is, at the moment the claim is made, in the
future tense — which section 4 did, and section 1 did not.

Found by auditing the design notes for stale messaging, which is the same pass
8.5.4 ran over the README and did not extend to `docs/design/`.


## 49. The first program written on Windows did not parse

**What was believed:** that a `.kh` file is a `.kh` file, and the first thing
somebody does after installing Khora is write one.

**What is true:** on Windows the first thing they write starts with three bytes
nobody typed. `ef bb bf` — U+FEFF, the byte order mark — is what PowerShell's
`Out-File -Encoding utf8` emits, what Notepad emits, and what Visual Studio and
VS Code emit when configured for "UTF-8 with BOM". The lexer had no rule for it,
so it became a `LEX_ERROR` and the parser said:

```
error: expected a declaration
 --> src\main.kh:1:1
  |
1 | module main;
  | ^
```

**The mark does not print**, which is the whole of why this is worth an entry.
The message names the right line, points at the right column, and describes a
line that is correct. There is nothing in the diagnostic, in the file, or in an
editor to look at. The only way to see it is to hexdump the first four bytes,
which is not the fourth thing a newcomer tries.

**The fix is one character in a regex.** U+FEFF is ZERO WIDTH NO-BREAK SPACE, so
whitespace is what it is, and the lexer's whitespace class now contains it.
Anywhere rather than only at offset zero — a file concatenated from two others
has one in the middle, and it means the same nothing there. The CST stays
lossless because the mark is emitted as a whitespace token like any other, which
`a_byte_order_mark_survives_a_round_trip` pins.

**How it was found, which is the part that generalises.** By installing
`v0.1.0-rc.1` from the published release on a machine with no Khora checkout on
its `PATH`, and writing the first program with the shell that was already open.
Every test in this repository writes its sources with Rust's `fs::write`, which
does not emit a BOM; every `.kh` file in `std/`, `examples/` and `packages/` was
written by an editor that does not either. So the entire corpus — 16 modules,
1,545 tests, three platforms in CI — could not have caught this, and would not
have caught it at any scale, because the corpus is not where the input comes
from.

13.24 exists for exactly this class, and this is its first finding: a
clean-machine test is not the same test as a green suite, and the difference is
not thoroughness. It is that a user's file gets made by a user's tools.

## 50. The package a file belonged to was whichever manifest was nearest

`khora check src/main.kh` does not check one file. An entry point names where a
program starts, not everything it is made of, so the command finds the package
that file belongs to and checks the package. That was errata 30's fix and it was
right. The way it found the package was not:

```rust
if let Some(manifest) = nearest_manifest(root) {
    let package = manifest.parent()...;
    gather(&package, &mut out)?;
}
```

The nearest `khora.toml` walking upwards. Which was correct for every manifest
that existed, because every manifest that existed declared a `[package]`.

**Then 14.13 added one that does not.** A workspace root is a `khora.toml` with
a `[workspace]` table and no `[package]`, and this repository grew one at its
top. Nothing about the change touched `collect_sources`, and the baseline stayed
green. What broke was a test in `khora-cli/tests/check.rs` that checks one file
in `target/tmp` and asserts on the file count:

```
assertion `left == right` failed: checked 28 file(s): no errors
  left: 28
 right: 16
```

Twenty-eight files, from a command naming one. The walk upwards had left the
scratch directory, left `target`, reached the repository root, found a
`khora.toml`, and concluded that the file's package was the entire monorepo —
`std`, four examples, four benchmarks and a library. It compiled all of them and
said "no errors", which was even true.

**The fix is that a manifest without a `[package]` is walked past**, exactly as
a directory without a manifest is. `enclosing_package` reads each candidate and
keeps climbing until one declares a package, or the walk runs out of parents.

**What is worth keeping.** The bug is not that the workspace root confused the
lookup; it is that the lookup never asked the question it meant. "The nearest
manifest" and "the package this file is in" were the same set for as long as
there was only one kind of manifest, and code that conflates two things which
happen to coincide reads as correct right up until they stop coinciding. There
was no way to notice by reading `collect_sources`, because the difference did
not exist yet.

And it was caught by a test that is about something else entirely — whether a
file named outright is read even when its target suffix says another host. That
test asserts an exact count rather than "it worked", which is the only reason
the extra twelve files were visible at all. A test that had asserted success
would have passed.

## 51. The cache was right and the repository was wrong

14.17's build cache keys on the source *and the toolchain that turns it into
bytes*: the compiler binary, the linker binary, the runtime archive. Its tests
build a program twice and expect the second to be reused.

Under a full `cargo nextest run` they failed about one run in three. Alone, and
even alone at sixteen threads, they never failed at all.

**Three rounds of diagnosis were guesses**, and each was plausible enough to
act on. Windows holding a freshly linked executable open. A directory rename
losing to a virus scanner. Hashing the source of a copy rather than the copy.
Two of those produced real fixes worth keeping — the rename now retries and
reports instead of silently treating every failure as a lost race, and the
stored artifact is hashed rather than the one it was copied from — and neither
was the bug.

**What ended it was making the cache able to explain itself.** `Miss` gained a
`Display`, `KHORA_CACHE_EXPLAIN=1` made `khora build` print the key and every
ingredient of it, and the tests started carrying every build's output into the
assertion message. The next failure said:

```
khora: key from compiler af126c475886 linker 8bbe086dfb0f runtime 1ecc48dd160a ...
khora: key from compiler af126c475886 linker 8bbe086dfb0f runtime 52c957453b73 ...
```

Same compiler, same linker, same sources, **different runtime archive**, in two
builds seconds apart inside one test.

`crates/khora-codegen-llvm/tests/harness/mod.rs` runs `cargo build -p khora-rt`
from inside a test, and its own comment explains why: an edit to the runtime
otherwise leaves a stale archive and every compiled program links the previous
version. It even says it must be called from *every* test binary, because
"cargo runs test binaries in parallel, so a single binary building the archive
is a race the others lose".

That build resolves a different feature set from the one
`cargo nextest --features llvm` resolved, so cargo relinks the staticlib, and
`target/debug/khora_rt.lib` flips between two files **while other tests are
running**.

So the cache missed because an input had changed. It was correct every single
time.

**The fix is in the test, and the finding is not.** The cache tests now point
`KHORA_RT_LIB` at one copy nothing else rebuilds. But the underlying fact
stands: during a parallel test run, two `khora build` invocations seconds apart
link against different runtimes. That was true before the cache existed and
nothing had ever noticed, because nothing else in this repository compares two
builds for identity. Filed as 14.33.

**What generalises.** A cache is an oracle for "did anything change", and
pointing one at your own build is a stronger check than any test that only asks
whether the build succeeded. The first three fixes were attempts to make a
disagreement go away; the thing that worked was making the disagreement
describe itself. `Miss` and `KHORA_CACHE_EXPLAIN` are shipped, not scaffolding
— a cache that cannot say why it missed is a cache nobody can maintain, and the
next person to hit this deserves the sentence rather than the three guesses.
