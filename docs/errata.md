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

**Fixed, and the numbers are worth keeping.** `cargo build -p khora-rt` writes
98,725,916 bytes; `cargo build --workspace --features llvm` writes 98,490,170.
They are not the same archive and never were — the harness was not refreshing
a stale file, it was substituting a differently-resolved one. It now builds
only when the archive is older than the runtime's sources, which is the
question its own doc comment was asking all along. With the archive current a
codegen test takes 1.0 s and leaves it alone; touch a runtime source and the
same test takes 8.0 s and rebuilds it. Roadmap 14.33.

One thing the fix exposed and did not solve: a `touch` with no content change
rebuilds the archive to *different bytes at the same size*. The Rust staticlib
is not byte-reproducible across rebuilds. That is upstream of Khora's own
reproducibility claim, which is about the compiler's output given a fixed
archive, but it does mean the cache key's runtime component changes whenever
`khora-rt` is relinked even without a real change — correct, and pessimistic.

**What generalises.** A cache is an oracle for "did anything change", and
pointing one at your own build is a stronger check than any test that only asks
whether the build succeeded. The first three fixes were attempts to make a
disagreement go away; the thing that worked was making the disagreement
describe itself. `Miss` and `KHORA_CACHE_EXPLAIN` are shipped, not scaffolding
— a cache that cannot say why it missed is a cache nobody can maintain, and the
next person to hit this deserves the sentence rather than the three guesses.

## 52. Publishing a second thing broke the installer for the first

Releasing the VS Code extension on its own `vscode-v*` tag broke
`curl | sh`, in the twenty minutes between pushing the tag and noticing.

`install.sh`, `install.ps1` and `khora toolchain install` all asked GitHub the
same question:

    https://api.github.com/repos/<repo>/releases/latest

That endpoint does not mean "the newest version of your software". It means
**the newest release in this repository that is not a draft and not a
pre-release** — the whole repository, everything published from it.

Every Khora toolchain release so far is a candidate, published as a
pre-release. That was deliberate: it is how the two channels are built, and it
is why a plain install correctly said "no stable release yet" while `--pre`
found one. With only pre-releases present the endpoint 404s, and all three
installers treat that 404 as an answer rather than an error — there is a
paragraph in each of them saying so.

Then `vscode-v0.3.0` was published. The extension is not provisional, so it is
not a pre-release. It became the only non-pre-release in the repository, and
therefore the answer to `/releases/latest`.

What the installers then did with it is the part worth remembering:

```sh
TAG=vscode-v0.3.0
NUMBER=${TAG#v}            # scode-v0.3.0
BUNDLE="khora-$NUMBER-$TRIPLE.tar.gz"
```

`v` was being stripped by prefix, because every tag it had ever seen started
with `v` and then a digit. `vscode-` starts with `v` too. So a fresh
`curl -fsSL … | sh` went looking for
`khora-scode-v0.3.0-x86_64-unknown-linux-gnu.tar.gz`, which has never existed,
and reported a download failure naming a version nobody had ever released.

`--pre` was left standing by luck rather than by design: it takes the first
entry of `/releases`, and GitHub happened to order the older candidate ahead of
the newer extension. Nothing guaranteed that.

**The fix is that `/releases/latest` is no longer asked, anywhere.** All three
installers list `/releases` and filter to the tags that name a toolchain — `v`
followed by a digit — then pick the newest, or the newest that is not a
pre-release, from the release's own flag. The filtering is theirs to do,
because "which of the things this repository publishes is a compiler" is not a
question GitHub has been told the answer to.

**What generalises.** A repository that publishes one artifact can let the
forge decide what "latest" means. The moment it publishes two, that endpoint
starts answering a question nobody asked, and it answers it with a 200 and a
plausible-looking tag rather than an error. The failure surfaced in the
*consumer* — an installer for a component that had not changed — days or
minutes after a change to something unrelated.

The `v`-prefix strip is the second half. `trim_start_matches('v')` and
`${TAG#v}` were reasonable when one tag series existed; they are silent
corruption when a second one shares the first letter. Neither had a test with
a tag it should refuse, because a tag it should refuse could not previously
exist. `names_a_toolchain` is now a named predicate with those cases written
down, in all three implementations.

## 53. A block's type hint was eaten by its first statement

`postgres::pool::held` installs a leased connection as a capability and hands
the body's result back:

```khora
Result::Ok(body()! with { db: over(leased) })
```

which the checker rejected, twice:

    this argument: `A` is a type the caller chooses, so it cannot be
    assumed to be `Db`

`A` is `held`'s own type parameter. Nothing in that line says `A` is `Db`, and
the postfix `with` is ordinary Khora -- `WithExpr` is in the grammar and the
AST documents it as `analyze(id) with { .. }`.

### Not the parse, and not generics

The syntax tree is identical in both positions -- `WITH_EXPR[ CALL_EXPR,
RECORD_EXPR ]` -- and the same expression checks clean as a function's tail,
with a concrete type *and* with a generic one. It fails only as a **call
argument**. Three facts that together name the culprit: it is the hint, and it
only matters when the hint is an unsolved variable.

### What was happening

`expr with { .. }` lowers to a block: the row becomes `let` statements binding
the capabilities, and `expr` becomes the tail. That is the whole of
installation, and it is why an inner `with` shadows an outer one for free.

`Expr::Block` set `self.hint` and called `infer_block`, and `self.hint` is
*taken* by the first `infer` that runs. In a block with statements, that is
the first statement -- not the tail it describes.

Against a concrete hint this is invisible. `hint_at` unifies and throws the
result away, so `Int` against `Db` simply fails and nothing is said. Against an
unsolved variable it is not invisible, because the unification **succeeds**:

    Result::Ok( body() with { db: over(leased) } )
                ^ the hint here is Ok's payload variable ?P
      let db = over(leased);   ->  hint_at(?P, Db)  ->  ?P := Db
      body()                   ->  A

So `Ok`'s payload was decided to be `Db` before the tail was looked at, and the
tail's real type then "disagreed" with a variable the block itself had solved.
The error named the innocent line.

### The fix

`infer_block` takes the hint at entry and restores it immediately before
inferring the tail. Statements are checked on their own terms; the tail gets
the hint that was always meant for it.

Both halves have tests, and the second is the one worth having: deleting the
restore makes `take({ 5 })` fail, because a block's tail stops narrowing a
literal. A fix for the first half that quietly dropped the hint would have
passed everything else.

### What generalises

**A hint is a statement about a value, so it belongs to the expression that
produces the value.** `self.hint` is a field rather than an argument, which
makes "whoever infers next" the recipient, and in a block that is the wrong
expression by construction.

The reason this survived so long is the more useful half. Passing a capability
as an *argument* is what the pool did before, and every other `with` in the
repository is either a signature row or the block form, whose body is a block
in tail position. `pool.kh` was the first postfix `with` in a call argument
anywhere -- so the combination that fails is one nothing had written down yet.
A feature that is in the grammar, in the AST, in the formatter and in no test
is a feature nobody has run.

## 54. A capability was matched by name and never by type

Found while building `with <handler value>`, by writing the test that says the
*old* form still works:

```khora
fn transfer() -> Int with { ledger: Ledger } { ledger.note(5) }

fn go() -> Int {
  with { ledger: a_clock() } {   // a `Clock`, not a `Ledger`
    transfer()
  }
}
```

This compiled with no errors, ran, and printed `777` -- the value of
`Clock::now()`. `ledger.note(5)` had dispatched to a different operation of a
different effect, and the argument was dropped on the way.

### Why it was silent

A capability requirement was discharged by **label**. The subtraction in
`demand_rows` asked only whether the name was supplied:

```rust
.filter(|(l, _)| !self.installed.contains(l))
```

and the lexical check beside it asked only whether a binding of that name was
in scope. Neither looked at what the binding *was*. A `with` row is an
ordinary record literal, so nothing else in the pipeline had a reason to
compare it against the requirement either: the row is checked as a record, and
the requirement is checked as a row of labels, and the two were never brought
together.

Code generation then did exactly what it was told. It looked up `ledger`,
found a `Clock` handler, and passed it where a `Ledger` was expected. Both are
records of closures with compatible layout, so the call landed on whatever
operation sat at that offset.

### The fix

When a label is in scope at a call site, its type is compared against the
requirement and a mismatch is reported. Two details that are the whole of
getting it right:

- **Checked, not subtracted.** The first version discharged the requirement
  as well as checking it, which is wrong: a label in scope is often the
  function's *own* `with` parameter, which still has to be charged to the
  signature. Subtracting it emptied `call_rows`, and `unused-capability`
  promptly reported every pass-through function as not using the capability it
  forwards. Four tests caught that, which is the argument for having them.
- **Silent on anything undecided.** `Unknown`, a variable, `Never` and a rigid
  parameter are all "not settled", and a second message about a type nothing
  has decided is noise after the first.

### What generalises

**Two checks that never meet are one check.** The row was validated as a
record and the requirement as a set of labels, and each was correct about its
own half. The bug lived in the join, which nothing owned -- there was no line
of code whose job was "these are the same capability".

The reason it survived is worth more than the fix. Every `with` in the
repository is *correct*, so no test ever supplied a wrong-typed capability;
the hole was only reachable by writing a program nobody would write on
purpose. It was found by writing the negative case for a new feature -- the
test asserting that the **old** behaviour still worked -- which is an argument
for writing those even when the answer seems obvious. The answer was not what
anybody would have guessed.

## 55. Two modules importing each other panicked the compiler

Seven lines, and `khora check` exits 101 with a Rust backtrace:

```khora
// a.kh                        // b.kh
module demo::a;                module demo::b;
import demo::b::{b};           import demo::a::{a};
pub fn a() -> Int { 1 }        pub fn b() -> Int { a() }
```

```
thread 'main' panicked at salsa-0.28.2/src/function/fetch.rs:176:21:
dependency graph cycle when querying type_map(Id(0)),
set cycle_fn/cycle_initial to fixpoint iterate.
Query stack:
[ diagnostics(Id(0)), derive_report(Id(0)), type_map(Id(0)),
  type_map(Id(1)), derived(Id(1)), file_scope(Id(1)) ]
```

### Where it came from

`type_map` resolves an imported name by asking the exporting file for *its*
`type_map`:

```rust
let exported = type_map(db, source);     // khora-types/src/map.rs
```

with one guard beside it:

```rust
if source == file { continue; }
```

That catches a file importing itself and nothing else. Two files importing
each other each ask for the other, for ever. Salsa cannot know that the
recursion is the program's fault rather than the compiler's, so it panics --
and its message suggests `cycle_fn`/`cycle_initial`, which is advice for a
query that is *meant* to converge. This one is not; it is a user error that
had never been given a diagnostic.

### The fix, and which layer it belongs to

Two new queries in `khora-hir`:

- `module_imports(file)` -- the module paths one file imports, and nothing
  else. Separate from `item_map` on purpose: a map carries ranges, so every
  edit that moved a span would rebuild the cycle check and everything behind
  it. A list of paths changes only when an `import` line does.
- `import_cycles(root)` -- depth-first from each module, looking for a way back
  to itself.

The refusal goes in `file_scope`, which is the layer that matters: it is what
builds `scope.origins`, and `import_types` walks `scope.origins` to decide
whose `type_map` to ask for. Drop the import there and the recursion never
starts. Reporting it in `type_map` instead would have been reporting it after
the crash.

The message names both modules and draws the ring:

```
error: `demo.a` and `demo.b` import each other: demo.a -> demo.b -> demo.a.
       Move what they share into a module they can both import
```

### What generalises

**A cycle in the input becomes a cycle in the query graph**, and a memoizing
compiler turns that into a panic rather than an error, in a component that
knows nothing about the language. Salsa is right to panic -- a cyclic query
graph *is* a bug in the caller -- but the caller here is a compiler being told
about a cyclic program, and the distinction has to be made before the query
runs, not inside it.

The cheap way to find the rest of these is to ask which queries take a
`SourceFile` and call themselves on a *different* one. `type_map` was the only
one, because it is the only place a file's meaning depends on another file's
meaning rather than on its surface: `file_scope` and `module_api` reach across
files too, and both stop at what a file *declares*, which cannot cycle.

The five tests are the fix and its boundary: a two-module ring, a three-module
ring, a plain assertion that it does not panic, and -- the two that matter --
a diamond and a chain, neither of which is a cycle and both of which a
careless reachability check would call one.

## 56. `--lib` named its output after whatever sorted first

`khora build . --lib`, run on Linux in a package of one module, printed:

```
library /home/codys/dev/khora/std/ai.so from 16 module(s) [debug]
header  /home/codys/dev/khora/std/ai.h
```

It built the right code. It named the artifact after the **standard library's**
`ai` module, and wrote a 31 MB shared object and a header into `std/`.

### The line

```rust
// The binary is named after the module holding `main`, or after the one
// file when there is only one.
let entry = inputs
    .iter()
    .find(|(_, text, _)| text.contains("fn main("))
    .or_else(|| inputs.first())
    .expect("at least one source");
```

The comment says "the one file when there is only one". The code says
`inputs.first()`, with no such condition -- and `inputs` is every source the
build reads: the package, its dependencies, **and the whole standard library**,
sorted by canonical path. A library has no `fn main(`, so the fallback always
ran, and it picked whichever of those sorted earliest.

### Why Windows passed

Sorting by canonical path means the answer depends on where the package
happens to live:

| | package | standard library | first |
| --- | --- | --- | --- |
| Linux | `/tmp/...` | `/mnt/c/.../std` | **std** |
| Windows | `...\AppData\Local\Temp\...` | `...\dev\khora\std` | **package** |

`AppData` sorts before `dev`; `/mnt` sorts before `/tmp`. The test passed on
Windows for four years' worth of no reason at all, and `a_library_build_caches_
its_header_too` failed on every Linux runner with "the header should come back
with the library" -- which was true, and about the wrong header.

### The fix

The entry is chosen from the sources the *package* owns, falling back to any
source only when there is no package to speak of. `package_of` answers for both
spellings of the argument -- `khora build .` names a directory, `khora build
src/main.kh` names a file inside one.

The `fn main(` search is restricted the same way, which was a second latent
bug: a dependency containing `fn main(` could have named the executable.

### What generalises

**A fallback with no condition is a fallback that always runs.** The comment
described a guard -- "when there is only one" -- that the code never had, and a
comment that describes an intention rather than the behaviour is worse than
none, because it stops the next reader looking.

And the shape of the near-miss is worth keeping: **sorting made this
deterministic per platform and different across them**, which is the most
expensive kind of bug to own. It cannot be reproduced by re-running, it looks
like a platform difference in the compiler, and the platform it fails on is the
one nobody develops on. The fix for that class is not more testing on Linux;
it is not choosing anything by sort order that has a meaning available.

## 57. `khora check std` emptied the lockfile

Noticed as a dirty working tree that would not stay clean, and traced to a
command that has nothing to do with dependencies:

```
$ tail -4 khora.lock
[[package]]
name = "postgres"
source = "path"
path = "packages/postgres"

$ khora check std
$ tail -4 khora.lock
version = 1
package = []
```

`packages/postgres` is a path dependency of `examples/ledger_service`. It was
discarded by a command aimed at a directory that neither declares it nor
depends on anything at all.

### `Some("")` is not `None`

```rust
let root_dir = manifest_path.parent().unwrap_or(Path::new(".")).to_path_buf();
```

`Path::new("khora.toml").parent()` is **`Some("")`**, not `None`. The
`unwrap_or` never ran, and `root_dir` became the empty path.

Everything downstream then behaved correctly on a value that means nothing.
`"".canonicalize()` fails, so `same` falls back to comparing the text, so
`""` is not the workspace root, so the workspace is `None` -- and the member
seeding guarded by it is skipped:

```rust
// Every member seeds the queue, so the lock describes the workspace rather
// than whichever member happened to be built.
if let Some(found) = &workspace { .. }
```

What is left is one manifest's own dependencies, and a workspace root has
none. That empty resolution is then written over the lockfile, because the
code that writes it is asking the right question -- has the resolution changed
-- about an answer computed from nothing.

`khora check .` was unaffected, because a workspace root fans out over its
members and each member *is* a member, so the filter accepts and the seeding
runs. Only a path that is inside the workspace directory without being a
member of it -- `std`, `docs`, anything -- took the broken route.

### Why it mattered more than a dirty file

`postgres` is a path dependency and re-resolving it costs a directory read. A
git dependency is pinned in the lockfile by revision and checksum, and that is
the whole point of the file: discarding the entry discards the pin, and the
next resolution is free to take a different commit. The failure would have
been a build that silently used a different dependency, discovered later and
somewhere else.

It also reached `main`: the commit before this one shipped
`package = []`, because `git add -A` ran between a `khora check std` and the
baseline that put it back.

### What generalises

**`parent()` has three answers, not two**, and the third one is shaped like
success. `Some("")` passes every check that asks whether a value is present
and fails every check that asks whether it is a directory, which is why it
survived to the far end of the function before doing damage.

`workspace.rs` already carries a long comment about the same trap, one file
over:

> `examples/ledger_service` has two parents as a relative path -- `examples`,
> then `""` -- and the walk stops there.

Two places have now been bitten by the empty path, which is the argument for
fixing it at the point where a path is turned into a directory rather than
adding a third place that has to remember.

The regression test asserts what a lockfile is *for*: that two commands
pointed at different directories in one workspace leave the same file behind.
It fails against the old code with "checking a directory that is not a member
rewrote the workspace lockfile", which is checked rather than assumed.

## 58. Three things a first program falls into

All three were found by compiling the documentation rather than by anybody
writing Khora, which is the note worth keeping: a language's first ten minutes
are the part its authors never experience again.

### An inherent method needed an unrelated import

```khora
module main;

fn describe(v: Int) -> String { Int::to_string(v) }
```

```
error: `Int` is not a trait with a function named `to_string`
```

Adding `import std::core::{Show};` -- naming something the file does not use --
fixed it. The *presence* of an import mattered and its contents did not.

`Int`, `String` and `Array` are spelled without importing anything, and their
methods live in inherent impls in `std::core`. Those arrived through
`import_inherent`, which runs once per **imported origin** -- so a file with no
imports got none of them. Its own doc comment had already named the intent:
methods should arrive "whether or not the file imported `Params`".

`std::core`'s inherent impls are now always in scope. A type you can write
without an import has methods you can call without one.

### `for` needed a name it never mentions, and the lint said to delete it

```khora
import std::core::{List, Step, print};
for name in names { print(name); }
```

```
error: the type of this expression was never worked out, and nothing else was
reported -- so either it needs an annotation, or this is a gap in the compiler
worth reporting
```

The expansion calls `it.next()`, a trait method, so `Iterator` has to be in
scope as much as `Step`. Only `Step` was checked for, so the failure arrived
three layers later as the checker's own self-accusation -- correct, and in
front of somebody writing their first loop.

Importing both compiles. **`unused-import` then reported `Iterator`**, because
a `for` writes neither name: the lint told the reader to delete what made the
program work.

Three fixes, one per way of finding out: the desugaring checks for `Iterator`
where it already checked for `Step`; the message names both; and the lint
counts a `for` as using them, asked of the tree rather than the token stream,
because `for` is a contextual keyword and `handler for Ledger` is not a loop.

**Requiring the imports stays.** `desugar.rs` gives the reason and it is a good
one -- the alternative is a name the compiler knows and the program cannot see,
which is errata 46. The bug was never the requirement.

### A nested constructor under a generic defeated exhaustiveness

```khora
match lookup(id) {
  Result::Ok(n) => ..,
  Result::Err(UserError::NotFound(m)) => ..,
}
```

```
error: this `match` is not exhaustive: pattern `Err(_)` not covered
```

`UserError` has exactly one constructor, and every one of them is named.

A variant's field types are written in terms of *its own type's* parameters:
`Result<A, E>`'s `Err` carries an `E`, which is a `Type::Param`. `field_type`
answers `Opaque` for one -- "not known, never reported on" -- so the column
inside `Err` could not be expanded and nothing inside it could complete
anything.

The scrutinee knew what `E` was. It is `Result<Int, UserError>` and carries its
arguments; they were dropped by `column_type`, which matched
`Type::Adt { name, home, .. }` and ignored the rest. Substituting them before
reading the field types is the whole fix.

It also made the diagnostic sharper. A missing nested case is now named --
`Err(Forbidden(_))` rather than `Err(_)` -- because the checker can finally see
inside.

### What generalises

Two of the three are the same bug: **a fact that was available at the point of
use, and thrown away on the way there.** The scrutinee knew its type
arguments; the file knew `Int` needed no import. Neither was hidden, and both
were dropped by a `..` in a pattern or by a loop keyed on the wrong thing.

The third is worth a rule of its own: **a lint that contradicts the compiler is
worse than no lint.** `unused-import` and the `for` desugaring disagreed about
whether `Iterator` was used, and the reader who believed the lint got a broken
build. When two parts of a toolchain answer one question, they need one answer
-- the same lesson errata 54 gave about two type checks that never met.

## 59. A row in a type-argument position was never checked

Found by putting a row on a type for the first time. `Fiber<A, 'er>` carries the
row its body raises, which is what lets `join` on an infallible fiber compile to
a load rather than to a branch and a `raises` clause nobody wanted — and nothing
in `std` had used a row as a type argument before, so nothing had noticed that
one is not checked against an annotation.

```khora
pub type Slot<A, 'er>;
impl<A, 'er> Slot<A, 'er> {
  pub fn of(body: () -> A raises 'er) -> Slot<A, 'er>;
}

fn wants_other(_s: Slot<Int, {Other}>) -> () { }

fn main() -> Int {
  let bad = Slot::of(fn () => risky()!);   // `Slot<Int, {Boom}>`
  wants_other(bad);                        // accepted, and should not be
  0
}
```

`{Boom}` where `{Other}` is declared, and no error. The same hole is why
`Fibers::adopt`, whose parameter says `Fiber<(), {}>`, accepts a child that can
still fail.

### It was not unification, it was the converter

The first guess was that rows unify by *opening* — each side grows a tail and
absorbs the other's labels — and that opening is right for a `raises` demand and
wrong for an invariant argument. That reasoning is sound and it was not the bug.
`unify_rows` already refuses to grow a *closed* row, and says which label it
could not account for.

The row never reached it. `type_of_syntax` converts a written type, and `{ .. }`
in a type-argument position is `ast::Type::Record`, which no arm matched — so it
fell to `_ => Type::Unknown`. `Fiber<(), {}>` meant `Fiber<(), ?>`, and `?`
agrees with everything.

Three lines above that catch-all is a comment about errata 30:

> A bare `'r`. It has no `Path` of its own — it is one token — so without this it
> read as the empty name and became `Unknown`, which then absorbed whatever it
> was unified with and made every row-polymorphic signature pass by saying
> nothing.

The same failure, one case further along. Both are a type the converter did not
recognise becoming the one type that agrees with everything.

### The fix, and what it cost

`ast::Type::Record` now routes to `row_of_syntax`, which is where `with` and
`raises` clauses already go. `{}` is the closed empty row, and an annotation
naming a row is checked.

Turning it on immediately broke three nursery tests, and they were right to
break. `Fibers::adopt` had been declared `Fiber<(), {}>` on the argument that
adopting should mean settling your failure first — a compile error in place of a
line on stderr. With the row actually checked, every adopted child had to be
made infallible, and **a cancellation travels out on the same tagged return an
error does**: a fiber whose row is empty has no channel to be stopped on. The
nursery could no longer cancel its children, which is the one thing a nursery is
for.

The first fix was a `Task` — the same runtime fiber under a handle with no
parameters. It worked, and it was a type whose only reason to exist was one
signature. The second and better one was to make the signature expressible:
**an effect operation can now quantify over a row**, so `adopt` takes
`Fiber<(), 'er>` and the child keeps the channel it is cancelled on. Eight lines
in `check/expr.rs`, because the substitution in `record_field` has already
replaced every row the *effect* declares — so a `'x` still standing in an
operation's type is the operation's own, and instantiating it is what
`instantiate` already does for a generic function. `docs/design/fibers.md`.

### What generalises

**A permissive default is not a small bug, and it hides in the arm nobody
wrote.** `_ => Type::Unknown` is a reasonable-looking line that turns every
unhandled case into "agrees with everything", so the feature that was never
implemented does not fail — it passes. Twice now.

And: **a check that was never running is a design decision that was never
tested.** `adopt`'s empty row read well for a day and was wrong the moment it
had teeth.

## 60. Tuple inference gives up through a nested lambda

Reported by the message that asks to be reported:

```
the type of this expression was never worked out, and nothing else was
reported — so either it needs an annotation, or this is a gap in the compiler
worth reporting
```

The shape is a `map2` whose inner lambda builds a tuple and whose outer one
destructures it:

```khora
Validated::map2(
  a,
  Validated::map2(b, c, fn (p, s) => (p, s)),
  fn (x, rest) => match rest { (y, z) => .. },
)
```

Nesting records instead of tuples works, and that is the workaround in
`website/content/docs/cookbook/configuration.md`. The message did its job — it
distinguishes "you need an annotation" from "this should have worked" and asks
for the second to be reported, which is the only reason this is written down
rather than worked around in silence.

## 61. Build output landed among the sources, and a `.gitignore` grew to hide it

`khora build .` on a package wrote `src/main.exe` beside `src/main.kh`, along
with `src/main.exe.o` and, on Windows, `src/main.pdb`. `khora test` and
`khora bench` wrote `khora-tests.exe` and `khora-benches.exe` into whichever
directory they had just compiled. So the first `git status` after a first build
listed files nobody recognised, sitting in the directory a person keeps their
program in.

The evidence that this was wrong was already checked in. This repository's
`.gitignore` had twenty lines naming those files:

```
examples/**/src/*.exe
examples/**/src/*.o
examples/**/src/*.pdb
examples/**/src/*.ll
bench/**/src/*.exe
...
**/khora-tests.exe.o
**/khora-benches.pdb
```

one pattern per kind of file per tree that gets built, and `khora new`
scaffolded four more of them into every package it created. The list was still
incomplete: `std/khora-tests.exe` reached a commit — 3.5 MB of build output
inside the standard library — because the standard library is not under
`examples/` or `bench/` and nothing had named it.

**An ignore file that has to grow every time the compiler learns to emit
something is a compiler putting its output in the wrong place.** The patterns
were not the fix; they were the bug report, written down four separate times by
whoever hit it and never read as one thing.

Output now goes to `<package>/build/`, named after the package rather than
after the file holding `main` — `build/core_demo.exe`, not `src/main.exe`,
because a directory of its own can say what the program *is* where the old path
could only say where it came from. `khora new` writes a `.gitignore` with one
line in it, and this repository's lost sixteen.

Two rules keep the edges honest. A directory named on the command line counts
as a home even when it has no manifest, which is what keeps `khora test std` —
the standard library has no `khora.toml` — out of `std/`. And a loose file
outside any package still gets its executable beside it: `khora build
scratch.kh` writes `scratch.exe`, the way every other compiler answers that,
rather than inventing a `build/` next to somebody's scratch file.

### What generalises

**A workaround that has to be repeated is a design defect with a paper trail.**
Nobody was wrong to add `examples/**/src/*.pdb`; each addition fixed the
`git status` in front of them. What nobody did was ask why the file existed, and
the answer had been sitting in the file for four revisions.

## 62. Two modules, one type name, and the impl that went missing

`examples/ledger_service` stopped building when a record called `Entry` was
added to `std::schema` -- a module it does not import and never mentions:

```
error: `Show::show` has no body, so there is nothing to call. Give it one, or
       write `extern fn` if it is a C symbol to be found at link time
   --> examples/ledger_service/src/main.kh:459:1
    |
459 |
    | ^
```

Line 459 is the blank line at the end of the file. `khora check` passed.

The first diagnosis, written down at the time, was that record types resolve
*structurally* -- `std::core::Pair<K, V>` is `{ key: K, value: V }` and the new
`Entry` was `{ key: String, value: Raw }`, so it looked as though the field set
had picked the impl. That was wrong, and every attempt to reproduce it from
that description compiled and ran correctly. `ledger_service` declares an
`Entry` of its own. The collision was the **name**.

### What actually happened

An impl was identified by the head of its self type, as a bare string:

```rust
pub fn find(&self, trait_name: &str, ty: &Type) -> Option<&ImplDef> {
    let head = head_of(ty)?;
    self.impls.iter().find(|i| i.trait_name == trait_name && i.head() == Some(head))
}
```

`head_of` gives `"Entry"` for both. Three places did the same thing, and each
was wrong in its own way:

1. **The whole-program merge** deduplicated impls on `(trait, head)`, so the
   second `Show#Entry` was dropped before anything could look for it.
2. **The search** returned the first impl whose head *spelled* the same. Its
   parameters then failed to match the receiver, selection returned `None`, and
   the call was emitted against the trait's own bodyless method -- which is the
   message above.
3. **The method key** is `Trait#Head::method` and names no module, so both
   impls record a body under `Show#Entry::show`. Even with the right impl
   chosen, the search for its body took whichever unit came first: after (1)
   and (2) were fixed, the program compiled and printed `demo::store`'s answer
   for a `demo::main::Entry`. A wrong answer is worse than the failure it
   replaced, which is why fixing two of the three would have been the wrong
   place to stop.

The fix gives a type its full identity -- `ImplDef::target()` is the head *and*
the module that declared it -- and gives the body search the module selection
already worked out. The search prefers an impl whose self type actually matches
the receiver and falls back to the old by-name answer when none does, so a
receiver carrying no home resolves exactly as before.

### Why the corpus never caught it

A generic is what makes it visible. `List::shown` lives in `std::core`, is
compiled once per type it is used at, and resolves the impl through the whole
program -- which is the table the merge had pruned. Both same-named types have
to exist, both have to have impls, and the call has to go through a generic in
a third module that knows neither. The regression test in
`crates/khora-codegen-llvm/tests/modules.rs` is built to that shape, and was
checked against the old behaviour twice: once for the missing body, and once
for the wrong answer.

### What generalises

**A name is not a type, and the compiler said so in a comment.**
`ImplDef::head`'s own documentation reads "resolution is nominal, so this is a
name and never a shape" -- which is true and beside the point. Nominal
resolution needs the *whole* name, and a bare head is half of one. The rule the
rest of the compiler already follows is written above the variant merge in
`merged_types`, five lines from one of the three sites: *keyed by the
declaration, not by the spelling*. Errata 46 learned it for variants and it did
not travel to impls.

**And a diagnosis nobody could reproduce was a diagnosis nobody had.** The
structural-fields story survived a commit message and a roadmap entry because
it explained the symptom. It took four failed reproductions to notice that none
of them reproduced anything.

## 63. A flake blamed on Linux for months, which was about `cargo test`

One run in fifteen of `scripts/check-linux.sh` failed, and only there:

```
---- contain::tests::a_freed_object_is_not_freed_twice stdout ----
assertion `left == right` failed: the one that was freed normally is gone
  left: Some(0)
 right: Some(1)
```

Filed as #108 and carried for months as "the intermittent `khora-rt` failure in
the Linux repeat loop", which is where the investigation kept starting and why
it kept getting nowhere. The `poll` backend, the WSL2 kernel, the container's
scheduler: none of them had anything to do with it.

### What it actually was

`POLICY` is one atomic for the whole process — a host asks for trap containment
once at start-up, which is the only shape the real thing has. Six tests in
`contain.rs` each set it to `1`, do their work and set it back to `0`. `cargo
test` runs a module's tests on parallel threads **in one process**, so one
test's restore lands in the middle of another's body.

The window is between `begin` and `record`, and it is open because of a
deliberate performance decision documented ten lines above it: `record` and
`forget` check the global *first*, before the thread-local, because that check
is on the path of every allocation in every program and reading a static costs
a load and a branch never taken. It takes the hooks from 12% to 2.6%.

So:

```
    thread A                          thread B
    khora_set_trap_policy(1)
    begin()            -> registry is Some(vec![])
                                      khora_set_trap_policy(0)
    record(a)          -> returns early
    record(b)          -> returns early
    forget(a)          -> returns early
    len == 0, expected 1
```

`Some(0)` rather than `None` is the tell: `begin` had succeeded, so the
registry existed and was empty. That is the only interleaving that produces
those two numbers, and it can be read off the source without reproducing
anything.

### Why only Linux, and why that was a lie

The Windows gate runs `cargo nextest`, which gives **each test its own
process**. A global cannot be contended when there is one test in the process,
so the race is unreachable there and always was. The Linux check runs plain
`cargo test`. The platform in the bug's title was a proxy for the test runner,
and nobody noticed because the two always varied together.

Serializing the five tests with a mutex in the module fixes it. Not
`#[serial]`, not a nextest group: the contention is between these tests and
nothing else, the fix belongs in the file with the problem, and a nextest-only
answer would not have fixed the runner that actually had it.

### What generalises

**A flake's title is a hypothesis, and it is usually the first thing anybody
noticed rather than the cause.** "Intermittent, on Linux" was two observations
glued together; the second was doing all the work in everybody's head and none
of it in reality. The question that would have ended this months earlier is not
"what is different about Linux" but "what is different about how Linux runs the
tests".

**And a test that could not report itself is a test that stays broken.** This
became diagnosable in one step the moment `scripts/check-linux.sh` was fixed to
keep the log of the run that *failed* rather than the run after it — a
one-line change made for unrelated reasons a few hours earlier. The bug had
been happening the whole time and had never once printed the assertion.

## 64. `_ => Type::Unknown`, for the third time

`fn takes(xs: List<(Int)>)` accepted a `List<String>`. Parentheses round a type
argument switched off the checking of that argument.

`type_of_syntax` has an arm for each type form it understands and
`_ => Type::Unknown` beneath them. `Unknown` unifies with everything, so a form
with no arm did not fail — it agreed. Four forms had no arm: `Paren`, `Union`,
`Variant` and `Forall`.

```khora
fn takes(xs: List<(Int)>) -> Int             // accepted a List<String>
fn hold(r: Result<Int, A + B>) -> Int        // accepted a Result<Int, C>
fn colour(x: | Red | Blue) -> Int            // `Red` and `Blue` undeclared,
                                             // and nothing said so
```

The third is the worst: the unresolved-name walk never saw those names, because
the type they were in had already become `Unknown`.

### What it was hiding

`Result<Int, A + B>` reads like a union type, and the language reference listed
"union" among the type forms. There is no union type. `+` builds a row, which
is what `raises` and `with` take; in a type argument there was nothing for it to
mean, so it meant anything. A feature that was never implemented did not fail —
it passed, and the documentation described it.

### The fix

`Paren` unwraps, because `(Int)` is `Int` and always was. The other three are
reported by `crate::unresolved`, which walks the syntax and has the range the
construct was written at — `type_of_syntax` returns a `Type` and has no channel
to complain on, which is how they came to be silent in the first place.
`Forall` is left alone: an effect operation carrying one is already reported as
a type that was "never worked out".

`docs/design/unions.md` records what a union would mean if it existed, and the
decision that `+` is conjunction and `|` would be disjunction.

### What generalises

**Errata 60 wrote this down already**, about `_ => Type::Unknown` in two other
matches: *"a permissive default is not a small bug, and it hides in the arm
nobody wrote."* It said "twice now". This is the third, in a third match, and
the lesson did not travel because it was recorded as a story about the two
places rather than as a rule about the pattern.

The rule, stated so it can travel: **a fallback arm that returns the identity
of the lattice is a fallback arm that cannot fail.** `Unknown` unifies with
everything, `true` satisfies every check, an empty row demands nothing — each is
the value that makes the surrounding code agree, so the case nobody wrote is
the case nobody hears about. Match exhaustively, or make the fallback the
*bottom* rather than the top.

## 65. Three sections of the readiness gate scored without reading the tree

The gate was scored at 124 of 222 with a rule attached: *an item is ticked only
when it was checked*, and a half-done item stays unticked with a note saying
what remains. The rule is right. It was applied to three sections that had not
been read.

Section 3, resource and database semantics, was scored 2 of 6. Two of the four
unticked items were already satisfied by `crates/khora-codegen-llvm/tests/db.rs`,
a file the scoring pass never opened -- including the one that reads as the
section's whole point, that a cancelled fiber rolls back. Section 15,
compatibility and governance, was scored 1 of 8 on the same day #149 wrote
`CONTRIBUTING.md`, `CHANGELOG.md` and the compatibility page, which between them
satisfy seven of its eight items. Section 13, the package ecosystem, had three
items unticked that one end-to-end test already discharged -- a package fetched
from a repository outside the build, compiled and run -- and a fourth whose
**Left:** note read a monorepo's path dependency as evidence about what a
stranger could fetch. The score was sixteen points low across the three.

**All three failures were understatements**, which is the direction the rule is built
to fail in and the reason it is worth keeping. An unticked item that turns out
to be done costs an hour of rediscovery. A ticked item that turns out not to be
done is the thing a release checklist exists to prevent, and it is discovered by
a stranger.

But the rule does not survive being read as *unticked is free*. "I have not
checked" and "this is not done" render identically on the page, and a reader --
including the author, three weeks later -- cannot tell them apart. The section
summary said governance and compatibility policy *do not exist* at a moment when
they were seven files in the repository, and that sentence was written from the
scoreboard rather than from the tree.

**A gate item that has not been looked at should say so**, in the same **Left:**
note that a half-done item uses. `Left: not examined` is a different fact from
`Left: the pool does not discard a connection whose rollback failed`, and only
one of them is work.

And a **Left:** note is a claim like any other. Section 13's said the driver was
"consumed by a path dependency inside this repository", which is true and is not
about the question the item asks. Inside one repository a path dependency is the
right choice; whether a stranger can fetch the package is a different question,
and it took ten minutes to answer by writing a manifest in a temporary directory
and running `khora build`.

The sibling of errata 35, where a test runner reported three passes for a suite
whose third test asserted `4 == 5`. That was a green result that measured
nothing; this is a number that measured nothing. Both read as evidence, and the
tell is the same in each: the thing being reported on was never actually
consulted.

## 66. Every new project opened with an alarm about a key that had moved

Building a small program that depended on `packages/postgres`, from a directory
outside this repository, printed this before anything else:

    khora: cache miss, the key moved. Nothing is stored under this one, and the
    cache holds 1751 other(s) (000aa701c41e 002b70e6f6f0 005159a236ab, ...).
    `KHORA_CACHE_EXPLAIN=1` names the input that changed

Every clause of it was false. Nothing had moved, no input had changed, and the
1751 keys belonged to other projects. It was the first build of a new project,
which is the one case the message was written to stay quiet about.

**The variant existed for a good reason and asked the wrong question.** A
flaky test had already shown that "nothing is stored under this key" means two
opposite things -- an ordinary first build, or a key that moved on a tree
nobody changed -- and that while both were reported the same way, the second
was reachable only by setting an environment variable that changed the timing
of the thing being measured. So the two were split. The discriminator chosen
was *is the cache empty*.

But the cache is one directory for the whole machine. "Is the cache empty" is a
question about everything anybody has ever built here, and the question worth
asking is about the target in front of you. They agree exactly once: on the
first build on a fresh machine. From the second project onwards -- which
includes every user who tried an example before starting their own program --
the alarming branch is the one that fires, always.

The fix is to ask the question that was meant: the cache records the key each
target last built under, and a target with no record is a first build. That
also makes the message worth reading when it does fire, because it can name the
key this target had before instead of three unrelated ones; and it splits off a
third case that had been hiding inside the second, where the key is right and
the entry is simply gone, which is `khora cache --clear` and not an anomaly at
all.

**A near-miss worth recording.** The obvious marker is keyed by the target
path, and the obvious target path is the one on the command line -- which is
usually relative. `build/app.exe` names a different file in every directory it
is typed in, so two projects would have shared a marker: the collision being
fixed, reintroduced one level down. The marker is keyed by the absolute path.

The general shape is errata 46's, and 62's: **a proxy that is correct in the
case it was tested on and answers a different question everywhere else.** The
proxy here was cheap and available and nearly right, and "nearly right" for a
cache means the alarm fires on the users who have least idea whether to worry.

It was found by using the product from outside, which is section 19 of the
readiness gate and the one thing on it nobody in this repository can do.

## 67. A character and a hole, each fine alone

    print("caf\u{e9} ${n}")

    thread 'khora' panicked at crates/khora-hir/src/body.rs:1124:
    end byte index 4 is not a char boundary; it is inside '\u{e9}'

`split_interpolation` scans a literal's body for `${` and copies the text
between the holes. The scan is over bytes, which is right: `$`, `{`, `}`, `\`
and `"` are all ASCII and can never be part of a multi-byte character, so
finding a hole cannot go wrong. The *copy* was over bytes too:

    text.push_str(&body[i..i + 1]);

and slicing one byte out of a `str` panics on anything wider.

**Both halves were tested and neither test could fail.** A literal with no hole
never reaches this function -- it is only called when there is interpolation to
split -- so every test of non-ASCII text went down a different path and passed.
Every test of interpolation used ASCII text around the holes and passed. The
bug lives exactly in the intersection, and the intersection is
`print("caf\u{e9} ${n}")`, which is the first line of the first program that
wants to print an accented word.

The escape branch had the same bug one byte over. `&body[i..i + 2]` assumes the
backslash and what follows it are two bytes, and what follows it need not be.

**The shape to look for is a function reached by only one of two features.**
Coverage says this function was covered; it was, by inputs that could not
exercise the line that was wrong. Errata 35's test runner reported passes for a
suite whose third test asserted `4 == 5`, and this is the same thing from the
other side -- there, a green result that measured nothing; here, a green result
that measured something else.

It is also not the first time this repository has counted the wrong unit:
errata 43 is `\r\n` being four bytes. The lesson from that round was written
into `std` as `String::char_at`, `String::next_boundary` and, this week,
`String::chars_between` -- and the compiler that produced the lesson was still
slicing by hand.

## 68. The site had not built for a week, and the gate did not know

`npm run build` in `website/` failed:

    reference/debugging.md:65 -> https://github.com/codyspate/khoralang/blob/
    main/docs/release-readiness.md (source filename is not a rendered route)

The check is a good one. A link written to `../reference/traps.md` points at a
file in the content tree rather than at the route it renders as, which works in
an editor's preview and 404s on the site -- so `sync-docs.mjs` refuses it and
the build stops.

It asked in the wrong order. The `.md` test ran before the question of whether
the link was *external*, so `https://github.com/.../CONTRIBUTING.md` -- a link
to a file that is meant to be read as a file, and the correct thing to write --
was rejected as a source filename. Three of those went in with 13.14 and 13.15.

**Nothing caught it for a week, and that is the part worth writing down.**
`scripts/baseline.sh` is twenty-odd steps and covers the compiler, the runtime,
the standard library, the packages, the examples, the corpus's formatting, the
generated API pages and the Linux runtime through WSL. It did not build the
website. The GitHub workflow that would have caught it runs on a push, and this
repository's commits are local until they are not.

So the one tree that is *published to strangers* was the one tree with no local
gate over it. The gate now runs `node website/scripts/sync-docs.mjs`, which is
dependency-free -- plain Node, no `npm install` -- and is the part that broke.
The Astro build proper stays in CI, where the dependencies live.

The shape is errata 65's, one level up. That was about a scoring pass that did
not read the tree; this is about a gate that did not include one. Both are the
same question -- *what is not being looked at* -- and in both cases the answer
was something everybody assumed somebody else was covering.

A second thing fell out of building it. Two of section 17's twelve items were
already satisfied and unticked: the search index has been built over all 100
pages every time anybody ran `npm run build`, and the link checker has been
failing CI on broken internal links since it was written. Both were discovered
by running the build once.

## 69. A capability offered to a closure was one it had to use

    nursery(fn () => 1)

    error: this argument: `nursery: Nursery` is required here but not provided

The nursery is being provided. That is what `nursery` does. The body simply did
not want one, and could not be passed.

Every parameter written `with { 'ef | cap: Cap }` behaved this way -- `scoped`,
`bounded_nursery`, and anything anybody else wrote. A row on a callback names
what the callback *may* have; it was being read as what the callback *must*
use.

**The two rows were side by side and only one was right.** A lambda's type is
built with a fresh variable for each row, solved after its body is checked. The
error row was then deliberately opened:

    // Left open, because what the body raises is a lower bound rather than the
    // answer -- see `open_raises`. A closed row here is what made a mock that
    // cannot fail unusable as an operation declared to fail.

The capability row, four lines above, was not. So a mock that cannot fail was
usable where something fallible was wanted, and a body that needs nothing was
not usable where something was offered -- the same mistake, in the same
function, with the argument against it written out beside it.

Both are lower bounds. What a body raises is at least what its body raises; what
it requires is at least what its body reaches for. In both cases the caller may
have more, and in both cases the tail is what absorbs the difference.

The fix is four lines and reuses the closing pass: a tail nothing ever widened
becomes the empty row, so code generation sees exactly the row it always saw
and only a body that is *offered* something extra ends up carrying it.

**What hid it.** `nursery` is nearly always called with a body that adopts
something, `scoped` with one that acquires something, and a `with { .. }`
parameter is nearly always written by somebody who then uses the capability --
so the failing case is the *degenerate* one, and degenerate cases are what
nobody writes until they are building something else. It surfaced while trying
to write a deadline, whose timer fiber wants a clock and whose nursery body
wants nothing.

The shape is errata 67's: two things each correct alone, and the bug in the
case that needs both. Here it is narrower and worse -- two adjacent fields of
one struct, one carrying the reasoning that the other needed.

## 70. The documentation was written twice, and the second copy went stale first

`scripts/check-docs.sh` compiles every hand-written example in the tree, and it
was compiling 580 of them. 155 belonged to the Guide, whose fifteen pages were
a second telling of fifteen Reference pages: `guide/control-flow` against
`reference/control-flow`, `guide/pattern-matching` against
`reference/patterns`, `guide/shared-state` against `reference/sharing`, and so
on for fourteen of the fifteen.

That was deliberate. `reference/index.md` said so: *"The Reference is
intentionally redundant with the Guide at the syntax level: a language
construct should never exist only in prose or only in the parser."* The reason
is a good one and it argues for the Reference being complete, which is a
property a merge preserves. What it does not argue for is a second page per
construct, kept in step by hand, against a compiler that is still moving.

**What the merge measured.** Reading each pair to decide what was worth
keeping produced a number: a 196-line Guide page on control flow yielded about
five lines the Reference did not have. `guide/shared-state`, 184 lines, yielded
one clause — `reference/sharing` was a strict superset of it, table of channel
constructors included. The Guide's genuinely additive content across all
fifteen pages was the *advice* — prefer the smallest bound, reach for `for`
when the body is the point, keep slow work outside the critical section — plus
two subjects with no Reference twin at all, testing and packages, which became
Reference pages of their own.

**And one of the two copies was wrong.** `reference/failures.md` showed

    items |> List::map(fn item => process(item)!)

with one mark where there must be two. The compiler says so in two errors, one
naming the missing `!` and one naming the row, and the Guide's page had the
paragraph explaining exactly that — inner mark for the closure failing, outer
mark for `List::map` passing it on, which it can do only because its signature
ends `raises 'er`. The explanation and the broken example had been sitting on
two different pages for as long as both existed. Neither `check-docs.sh` nor
anything else could catch it: the fragment parses, and being *parseable and
wrong* is what that checker records as its own limit.

Fifteen redirects, per `docs/design/docs-urls.md`. `/guide` still resolves and
now reaches the Reference — a short path promises where somebody lands, not
what the destination is called.

## 71. The newest module was the one nothing introduced

`std::schema` shipped in #141 with 27 public items, a 229-line cookbook recipe,
and no page saying what a schema *is*. It was reachable as the seventeenth
entry of a nested `api` group in the sidebar, behind a signature list.

Looking for its front door found something worse. Its own doc comment said:

    `derive(Schema)` writes this for you and is what a reader should meet
    first; these are for a renamed key, a refinement, or a shape derivation
    cannot know.

`DERIVABLE` is `["Eq", "Ord", "Show", "Hash", "ToJson", "FromJson"]`. There is
no `derive(Schema)`. `docs/design/schema.md` calls it *"the primary one, not a
convenience"* and says it *"is required in the first version rather than
optional"*, because without mapped types it is the only way to get a record
schema without hand-writing an assembler per type. The library shipped without
the half its own design document called required, and the doc comment
described the finished state in the present tense — where it rendered, live, on
the website.

The cookbook page written at the same time got it right: *"`derive(Schema)`
will remove the assembler entirely... It is not shipped yet."* So the project
knew. The false tense is what happens when a doc comment is written from the
design rather than from the code, and `khora doc --check` cannot help — it
verifies that the page matches the comment, and the comment was the thing that
was wrong.

The tense is fixed and #170 tracks the feature. The prose page,
`/docs/stdlib/schema`, says what exists.

## 72. The baseline's receipt was a constant, and deleting a page proved it

`scripts/tree-id.sh` names the content of the working tree in one line, so a
receipt written at the end of a green baseline can be compared later and a
pre-push hook can tell whether the tree being pushed is the tree that passed.
It was:

    names=$(git ls-files -z | git hash-object --stdin)
    contents=$(git ls-files -z | xargs -0 git hash-object | git hash-object --stdin)

Deleting fifteen tracked pages broke it, in the quietest possible way.
`git ls-files` lists a file that is tracked, whether or not it is still on
disk. `git hash-object` given a path that is gone prints `fatal: could not open
... for reading` and **abandons the rest of that invocation** — and `xargs`
hands it hundreds of paths at a time, so one missing file took the hashes of
every path after it in the batch with it.

Then the pipeline hid the wreckage. A shell pipeline's status is its *last*
stage's, and the last stage was a `git hash-object --stdin` that succeeded on
the truncated stream, so `set -e` had nothing to see. The `fatal:` went to
stderr, one line, in the middle of a 300 KB log.

**What it cost.** Not "a slightly wrong hash". Editing a completely unrelated
tracked file left both halves of the receipt byte-identical:

    current:  59fbc334... d3080fd7...
    edited:   59fbc334... d3080fd7...
    restored: 59fbc334... d3080fd7...

The names hash never noticed either, because `ls-files` still listed the
deleted paths. The check had stopped distinguishing trees and kept printing a
line, which is the failure mode to fear: a check that goes silent gets noticed,
and a check that keeps answering does not.

**The fix reads no files.** The index already holds a hash per tracked path, so
`git ls-files -s` covers names and staged content without opening anything, and
one `git diff --binary` covers everything the working tree does differently —
a deletion included, which is the case the old shape could not represent.
`--no-color --no-ext-diff --no-textconv` so a machine with a differ configured
agrees with one without.

It is also about four times faster, which is beside the point.

**What found it.** Not reading the script. The baseline printed `fatal:` and
still exited 0, and the only reason that got chased is that the line named a
file this commit had just deleted. The sensitivity suite that now exists —
edit a file, delete a file, edit a file *while another is deleted* — is four
cases the old version fails and every future version has to pass.

## 73. Four invented words for four types the language already had

`std::schema`'s constructors shipped as `text`, `whole`, `exact` and `truth`.
The types they answer are `String`, `Int`, `Decimal` and `Bool`.

`docs/design/schema.md` had specified otherwise, in as many words: *"`string`,
`integer`, `boolean`, `decimal` and `secret` become the corresponding schema
constructors."* Four of the five shipped under a different name. The fifth,
`secret`, matched — which is the tell. A deliberate alternative scheme would
have renamed that one too; what happened instead was that each constructor got
named as it was written, by whatever word described its behaviour at the
moment, and nothing afterwards compared the result to the plan.

**Nothing could have caught it.** `khora doc --check` verifies that the
generated page matches the doc comment. `check-docs.sh` compiles the examples.
Both were green, because both check the code against itself. No gate compares
an API to the design document that specified it, and the design document is
not executable, so the drift was invisible to every automated check the project
has. It was found by a person reading the vocabulary table out loud and asking
why a schema library needed a word for "integer".

**The cost, stated plainly.** `std` held three vocabularies for four concepts:
the language's `Int`/`Bool`/`Decimal`/`String`, `std::config`'s
`integer`/`boolean`, and `std::schema`'s `whole`/`truth`. Every one of those
was a thing to learn that carried no information — `whole()` does not tell a
reader it produces an `Int`, and `exact()` actively hides it.

The rule is now written into the design document: **a constructor is named
after the type it answers.** `Shape`'s arms follow the constructors one for
one; `Raw`'s follow `Json`'s, because a `Raw` is what a source produced and the
first source anybody bridges from is JSON. Two needless trailing underscores
went with it — `Raw::Text_` and `Shape::Struct_` were defensive against a
collision that does not exist, which a four-line program confirms.

**The sweep missed a third of the call sites.** `git grep` over `*.kh` and
`*.md` came back clean and the rename looked done. It was not: eleven Khora
programs live inside Rust string literals in `crates/khora-codegen-llvm/tests`,
which no source-extension glob reaches. The baseline found them, eighteen
failures across `schema.rs` and `config.rs`. A grep scoped by file extension is
scoped by an assumption about where a language lives, and in this repository
Khora also lives inside Rust — so the honest check after a rename is the test
suite, not a second grep.

**What did not change: the messages.** A rejection still reads `listen.port
must be a whole number` and `rate should be an exact decimal`. Those are
sentences for somebody reading a failure, and the audience for a sentence is
not the audience for an identifier. Keeping them also meant the cookbook's
documented sample output stayed correct through the rename, which is how the
rename was verified: the recipe's complete program was extracted and run, and
its output was byte-identical to the page.

## 74. The documentation was addressed to the person writing it

A reader asked why `std::schema`'s page said this, under `int()`:

> The message still says "a whole number", which is what a person reading a
> rejection wants; the constructor is named for the `Int` it answers, which is
> what a person writing a schema wants.

That is an argument for a naming decision, addressed to whoever was making it.
It had been written the day before, in the commit that made the decision, and
it went onto a public API page because a doc comment is the nearest surface to
the code being changed.

**It was not one sentence.** Sweeping what `khora doc` publishes found about
thirty passages of the same kind and sixty-six references to files in the
repository:

- *implementation history* — "an empty map used to cost an eight-element
  array", "that check used to be a digit short", "which took a second attempt",
  "the reason is not the one that used to be written here", "the note that
  stood here said";
- *internal indexes* — `errata 35`, `Roadmap #142`, `docs/roadmap.md Phase 13`;
- *repository paths* — fifty-one `docs/design/*.md`, most of them a whole
  sentence consisting of a path and a full stop, which resolves to nothing at
  all from a browser.

**The distinction that decided each one.** Rationale for what the code does now
is documentation and stays: a reader choosing between `Clock`'s two millisecond
operations needs to know why there are two. An account of what the code did
before is a changelog entry wearing a doc comment, and it ages badly besides —
"this used to trap" is a claim about a version nobody can run.

Some cases sit on the line and were kept. `` `Clock` used to live here and now
does not`` is phrased as history but answers a live question — a reader whose
`import std::env::{Clock}` stopped resolving needs exactly that sentence. The
test is not the tense; it is whether the sentence helps somebody using the
thing.

Bare repository paths became links to GitHub where the sentence made a claim
about what the document contains, and were deleted where the path *was* the
sentence.

**What made it invisible.** Every check the project has compares the
documentation to the code. `khora doc --check` verifies that a page matches the
comment it came from; `check-docs.sh` compiles the examples. Both were green
throughout, because both were asking whether the documentation was *accurate*,
and it was. Nothing was asking who it was for.

`scripts/no-maintainer-notes.sh` now asks, and is a gate step. It matches the
unambiguous markers only — backticked repository paths, errata and roadmap
numbers, and notes about a previous version of the note. "Used to" is
deliberately not among them: it is ordinary English and it is also how a
genuine migration note reads, so it needs a person. The suite that proves the
checker works includes those four legitimate phrasings alongside six bad ones,
because a checker that fails a correct page gets switched off.

It found four more offenders the manual sweep had missed, in the same run that
first passed.

**And the fix broke a link, which only the rendered page showed.** The pass
that turned bare paths into GitHub links ran over a file where one link had
already been written by hand, so it linked the URL inside the link:

    [the design note](https://.../blob/main/[the schema design note](https://.../schema.md))

Every check stayed green. `khora doc --check` compared the page to the comment
and they matched, because both were wrong together; the site built, because
malformed Markdown is still Markdown; the link checker skips external URLs by
design. What the page actually showed was a fragment of punctuation and a dead
anchor, and the only way to see it was to fetch the deployed page and read it.
That is now the fifth case the checker matches.

## 75. Four trailing underscores, two of them for nothing

A reader asked why `std::schema` had `pub fn where_(self) -> String`.

`where` is not reserved in Khora. It is not a hard keyword, it is not one of
the seven contextual keywords, and the string `"where"` does not appear in the
lexer, the parser or the syntax crate. A four-line program declaring
`pub fn where(self) -> String` compiles and both `At::where(a)` and `a.where()`
run. The underscore was defending against nothing.

Sweeping the `.kh` sources found four:

| | why it was there |
| --- | --- |
| `where_` | nothing |
| `then_` | nothing — `then` is not reserved either; it existed to match its neighbour |
| `else_` | **real**: `else` is a hard keyword, and the parser answers `expected an identifier` for a field named `else` |
| `at_` | **real**: it shadowed `khq`'s own `diag::at` |

Two of the four were decoration. The two with a genuine constraint are the more
interesting half, because the constraint was real and the response to it was
still wrong: a name that has to be decorated to be legal is a name to replace,
not a name to decorate. `then_`/`else_` became `when_true`/`when_false`, which
reads better than either at the use site —
`if truthy(c) { when_true } else { when_false }` — and `at_` became `index`,
which is what it is.

`Raw::Text_` and `Shape::Struct_` had gone the same way two commits earlier,
for the same reason and found the same way: by asking what the underscore was
for and getting no answer.

**Where they come from.** Nobody sits down to name a field `then_`. One
identifier collides for a real reason, the underscore fixes it in a second, and
the next one gets an underscore because the neighbouring line has one. That is
how `then_` was born, sitting beside a legitimate `else_`. The habit then
travelled into `examples/khq`, which is a reference application — the thing a
new reader copies from.

There is no checker for this one. A trailing underscore is legal, sometimes
warranted, and a grep for it lands on the two cases that are fine as often as
the two that are not. What catches it is somebody reading the API and asking.

## 76. The design said a spelling could not be typed, and the language could type it

`docs/design/schema.md` had a section headed *Why a record literal of schemas
cannot be the spelling*. It said that

```khora
let s = Schema::struct({ a: Schema::integer(), b: Schema::string() });
```

"cannot be typed", quoted the checker's refusal, and concluded that the only
record forms were `derive` and an arity family: `Schema::two(..)`, `three`,
`four`, each taking an assembler closure. What shipped was `struct2` to
`struct5`, and the first person to read them asked what they were for.

The claim was true of a *library function* and false of the *language*. A
function `struct<A>(fields: R) -> Schema<A>` has no way to relate `R`, a
record of schemas, to `A`, the record they decode, without a type-level map,
and Khora has none. But the compiler is not a library function. It holds every
type, and a call it recognizes can be rewritten before the checker sees it:
`struct({ port: int(), host: string() })` lowers to `Schema::record` over
`Fields::of("port", ..)` zipped with `Fields::of("host", ..)` and a closure
that builds `{ port: a0, host: a1 }`, all of which the checker types by the
rules it already has. The result is `Schema<{ port: Int, host: String }>`, or
`Schema<Listen>` when the expected type says so, with the same diagnostics a
hand-written literal gets: an ambiguous record is reported as ambiguous, a
missing field as missing, an extra one as extra.

The mistake was in where the reasoning stopped. "It needs mapped types" was
the right diagnosis of the function; the step not taken was to ask whether the
thing had to be a function. Three months of `struct5` came from that.

| | |
| --- | --- |
| said | a record literal of schemas cannot be typed; use `derive` or an assembler |
| true | a generic *function* cannot type it; the compiler can, and does |
| shipped | `struct({ .. })` as a compiler-known call; `struct2`..`struct5` deleted |

The section is gone from the design, which now argues the opposite under *The
record form*; `docs/design/schema-derive.md` is the decision record for the
rewrite, and the reasoning above is there in full.

## 77. The load generator multiplied one connection's rate by the number of connections

Every throughput figure this project has ever recorded came from
`bench/load.py`, and every one of them was wrong by between two and twelve
times. The rig could not find a ceiling because it was not capable of finding
one: the number it printed was, by construction, one connection's rate times
the number of connections.

`load.py` starts one process per connection, each running for `seconds` from
the moment *it* starts, and then divides the total by `seconds`:

```python
counts = [out.get() for _ in running]
total = sum(counts)
print(f"{total / seconds:8.0f} req/s")
```

On Windows a `multiprocessing` child re-imports the interpreter, and forty-eight
of them do not start at once. Asked for four seconds, the rig runs for
fifty-two:

| workers | wall clock | asked for | reported |
| --- | --- | --- | --- |
| 4 | 6.7 s | 4 s | 103,608 req/s |
| 12 | 16.5 s | 4 s | 293,411 req/s |
| 48 | 52.4 s | 4 s | 1,174,907 req/s |

The workers barely overlap. Each gets a nearly idle server for its own four
seconds, measures the single-connection rate -- 26,000 a second, which is what
one Python process against this server does -- and the divisor stays at four.
So the report is 26,000 times the worker count, three times over: 103k, 293k,
1,175k are 4, 12 and 48 times one connection.

**This is why no ceiling was ever found.** A rig whose output is proportional
to its own worker count by construction cannot flatten. `bench/compare.py` was
written to walk a ladder and refuse a rate that was still climbing, and it
refused every time, and the conclusion drawn was that the client could not
saturate the servers. The conclusion was drawn from the artifact. The same
artifact explains the 1.85x spread between sittings that `docs/design/fibers.md`
recorded as irreproducibility: process startup time is what varied.

A server that counts what it answers settles it. During one 48-worker run
reporting 1,184,214 req/s, the server logged between 51,000 and 152,000 a
second, and served for fifty seconds rather than four.

### What was actually slow

The replacement, `bench/loadgen.rs`, was written as one thread per connection
doing blocking reads, which is `load.py`'s shape in a faster language. It
reported 7,900 requests a second on a connection where Python reported 26,631,
and the reason is worth recording because it is not the one anybody guesses.

The client's arithmetic was never the cost. A blocking read parks the thread
and the kernel wakes it again when the answer arrives, and on this platform
that pair costs about 120 microseconds -- on a round trip whose measured
median is 29. The same connection with the socket in non-blocking mode,
spinning on the read, answers 42,091 a second. Five times, from deleting the
sleep.

So the generator is now a handful of threads each driving many non-blocking
connections in a round-robin loop: no thread per connection, and no sleeping.
Eight threads is where its own rate stops changing -- 68k, 122k, 181k, 208k at
one, two, four and eight, then 210k, 210k, 215k at twelve, sixteen and
twenty-four -- which is the first time this project has been able to say that
the generator is not what it is measuring.

### What the numbers actually are

One sitting, 16-core Windows desktop, release builds, 32 connections,
six-second runs, mean of five, every condition checked by the script that
produced the table:

| | req/s | p50 | p99 | peak RSS | |
| --- | --- | --- | --- | --- | --- |
| C#, ASP.NET Core (Kestrel) | 266,267 | 103us | 253us | 240 MB |  |
| Khora `floor` | > 234,322 | 128us | 274us | 7.4 MB | generator still climbing |
| Khora `render` | > 234,039 | 127us | 253us | 7.8 MB | generator still climbing |
| Rust control, thread per connection | 202,182 | 150us | 319us | 5.5 MB |  |
| Go, `net/http` | 185,670 | 102us | 1,538us | 21.8 MB |  |
| Khora, `std::net::http` | 174,201 | 161us | 554us | 8.4 MB |  |
| Java, JDK `HttpServer` | > 114,200 | 251us | 663us | 699 MB | ladder still climbing |
| Node, `node:http` | 39,184 | 682us | 4,101us | 86.8 MB | spread 1.11x |

**The correction is not only to the magnitudes.** `bench/README.md` concluded
from the old rig that Khora's `Router` was "at least 6x Kestrel and at least
10x Go's `net/http`". Measured against a generator that is not the thing being
measured, it is **below** both on rate: about six per cent under Go's standard
library and a third under Kestrel. It is roughly four times Node and ahead of
the JDK's server.

What the old rig never measured at all is the column that turns out to be
Khora's: **8.4 MB against Go's 21.8, Node's 86.8, Kestrel's 240 and the JDK's
699.** Between three and eighty times less memory than the runtimes it answers
as fast as. The claim worth making was in the row nobody had instrumented.

Rows marked in the last column are lower bounds. `floor` and `render` are fast
enough that the generator is still gaining when it is given more of the
machine. Java's ladder was still climbing at 128 connections, which is a JIT
that had not finished compiling the handler.

The narrower comparisons the servers were built for survive and are worth more
than the cross-language row: `service` against `floor` is the library, so the
whole of `std::net::http` -- parse, header map, route match, render -- costs
about a quarter of the throughput of a socket loop that does none of it.

### And then the replacement got the memory column wrong

The first table published from the new rig said Khora's server peaked at
**576 KB**, and every other server under a megabyte too. That was a parsing
bug in the new tool, committed and published before anybody looked at the
numbers and asked whether they were plausible. A JVM does not run a web server
in 900 KB.

`tasklist` on Windows prints a process's memory with a thousands separator in
it:

```text
"control_keepalive.exe","48988","Console","1","4,468 K"
```

The sampler took the text after the last comma, which is `468 K"`, and read
468 KB from a process using 4,468. Everything was divided by roughly ten and
the error grew with the number, so the biggest servers were understated most:
the JDK's 699 MB was being reported as 948 KB.

The field separator in that line is quote-comma-quote, which the number's own
comma is not, and splitting on that reads it correctly. Both the Rust sampler
and the Python one had the same bug, written the same way an hour apart.

Two things are worth keeping from it. **A number nobody sanity-checked is a
number nobody checked**: the throughput figures were verified against a server
that counted its own answers, and the memory figures were not verified against
anything, so the one that was wrong was the one nobody had an oracle for. And
the corrected column is the interesting one -- it is where Khora is actually
ahead -- which is a reminder that the measurement least worth trusting is
often the one nobody expected to care about.

### The lesson that generalises

The rig had a ladder, a settling check and a refusal to report a climbing rate.
It had every piece of measurement hygiene except an oracle. Nobody asked the
server how many requests it had answered, and the server was the one component
in the experiment that knew.

A measurement of a system that the system itself can check should be checked
against it. It took one twenty-line server with a counter in it to overturn
three phases of recorded numbers.

## 78. `shut` waited for a peer that had nothing to say

Found by a test written for something else. `net_cancel.rs` proves a cancelled
fiber gives its socket back, and the peer at the other end saw the close in 24
milliseconds -- then the program did not reach its *next line* for another
120.0 seconds. Every run, to within a few milliseconds of exactly two minutes.

A print between every statement said which line:

    8.6 ms    a: started
    9.3 ms    b: bound
  203.4 ms    peer connected
  203.6 ms    c: accepted
  203.7 ms    peer read returned
  120.216 s   d: connection shut      <-- `shut` itself

**`shut` was the 120 seconds.** It closes politely, which is right and is
explained at length where it is written: closing while the peer is still
writing sends an RST, and an RST discards whatever the peer had not read --
including the answer just written to it. So it says there is nothing more
coming, reads off what is still arriving, and only then closes.

The drain read with `receive`, which suspends the fiber until something
arrives. For a peer that is *open and silent* nothing ever does. The
half-closed connection then sits in `FIN_WAIT_2` until the kernel abandons it
on its own -- 120 seconds on Windows, `tcp_fin_timeout` and sixty by default on
Linux -- and that is where the number came from. Nothing in `khora-rt` names
120 seconds because nothing in `khora-rt` chose it.

**The comment beside the drain said why it was safe, and was wrong about it:**

> The drain is bounded rather than a loop to end-of-stream. A peer that never
> stops talking must not hold the fiber, and the receive deadline the server
> already set is doing the real work.

The deadline was doing the real work only where one had been set.
`Router::serve_connection` sets ten seconds, so every connection the HTTP
server closed took ten seconds to close and nobody called ten seconds a hang.
Anything that used `std::net::socket` directly -- a test, a driver, the client
before it had a deadline of its own -- got the kernel's timeout instead.

**The fix is that a close should not wait at all.** What it wants is the bytes
that were in flight, not more of them. `khora_net_recv_now` is one `recv` with
no retry and no suspension, `receive_now` exposes it on all three platforms,
and `drain` uses it. 120.216 s became 203.7 ms, and the conformance check that
exists for the RST case -- a 9 KB header refused while the client is still
sending the ninth -- still passes, because those bytes are already in the
socket buffer when the server decides.

The test that found it now guards it: it reads to the end with the peer still
open and silent, and fails if that takes twenty seconds, against a floor of
sixty that the platform itself imposes on the old behaviour.

**Two lessons, and the second is the uncomfortable one.**

A bound that depends on a caller having done something is not a bound. The
comment named the mechanism keeping the drain honest and did not notice that
the mechanism was optional.

And the hang had been there for as long as `shut` has, in a repository with a
conformance suite, a soak, and a load generator -- because every program that
closes a connection here also sets a deadline, and every one of them was ten
seconds slower than it should have been. A test written for a different claim
found it in its first run, and only because it timed a line nobody had thought
to time.
