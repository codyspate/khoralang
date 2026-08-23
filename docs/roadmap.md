# Roadmap

Working plan for building the Khora compiler. Iterate on this file directly —
it is the source of truth for sequencing, and each phase states the test that
proves it is done.

Scope comes from `docs/project.md`. Where that document is wrong or silent,
`docs/errata.md` records it and this file schedules the fix.

## Decisions taken

`docs/vision.md` states the goal these serve. When a call is ambiguous, that
document breaks the tie — and where two options are otherwise even, its
familiarity rule picks the one a Go, Rust or TypeScript developer already knows.

| # | Decision | Rationale |
| --- | --- | --- |
| A1 | **Thin vertical slice first.** A deliberately small subset of Khora goes all the way to a running native binary before any stage is widened. | De-risks the backend early. A type system with nothing to run behind it hides integration problems until they are expensive. |
| A2 | **LLVM via `inkwell`, no Cranelift.** | Matches the spec, and gets `-O3`/LTO plus mature static-musl and aarch64-darwin linking without building a second backend. Toolchain cost paid in Phase 0.1. |
| A3 | **Salsa from the start.** `khora-hir` and `khora-types` are salsa queries. | Retrofitting incrementality means rewriting every pass. §6.5 wants sub-15 ms LSP responses; that is not a bolt-on. |
| A4 | **Full HKT and typeclasses.** Native `* -> *`, kind inference, instances. | What Rust structurally cannot express. Carries `Traversable`, `Stream` and user abstractions. Note it is justified by *containers*, not by the effect system — see A8. |
| A5 | **Structured concurrency with interruption in v1.** Fibers, cancellation that runs finalizers, `Scope`-bound lifetimes, `Schedule`. | Effect's headline safety property, and §6.4 already assumes it. Retrofitting interruption into a runtime that never had it is close to a rewrite. |
| A6 | **A C ABI foreign boundary**, and libraries written in Khora. | Non-negotiable 6 stands — a language with no libraries loses — but crates.io is the wrong way to meet it. It does not skip the work it looks like it skips: Khora has no byte buffers, so no crate can hand it one, and every primitive a binding needs is needed anyway. The mapping has no bottom either (lifetimes, traits, a second async runtime, a version treadmill), and pointing at Rust's ecosystem sharpens "why not just use Rust". The boundary is the C ABI, which generated code already crosses on every runtime call. `docs/design/ecosystem.md`. |
| A7 | **Developer experience is a product requirement.** Diagnostic quality, compile speed and LSP latency are tested from Phase 2, not polished in Phase 6. | The thesis is "beats Rust's DX". Rust's advantage is mostly cargo, rustc diagnostics, rust-analyzer and clippy. Deferring all of it means we cannot evaluate our main claim until the end. |
| A8 | **Direct-style algebraic effects, not a monadic `Effect<A, R, E>`.** Effects are rows on the signature (`with` / `raises`), discharged by handlers, with fallible calls marked `!` at the call site. Settled in `docs/design/effects.md`. | The spec already specifies Perceus and Leijen/Rémy scoped rows — both Koka, which pairs them with exactly this model. A monadic API fights that substrate. Effect-TS's `Effect.gen`/`yield*` is itself a simulation of direct style, just as `TypeLambda` simulates HKT. Decisively, only direct style lets a non-functional programmer write an effectful `for` loop at all — under a monad it must become a fold. |

### What A8 preserves, and what it costs

The dependency-injection model is unchanged. A capability `{ ledger: Ledger }`
is an effect whose operations are `Ledger`'s fields, so `ledger.get_history(x)`
performs operation `get_history` of effect `ledger`. `Layer` becomes a handler,
`Effect.provide` stays row subtraction, `run_native` still requires an empty
row. Only sequencing changes.

Costs, recorded honestly:

- `std/` and `examples/risk_analyzer` are written in the discarded monadic
  style. They remain valid *syntax* fixtures for the current grammar, but the
  API they show is superseded; they are rewritten once D7 settles the syntax.
- The runtime needs one-shot continuations for handlers — more than a monadic
  interpreter, but substantially the same machinery A5 already requires.
- HKT is less load-bearing than it would be under a monadic model, since there
  is no monadic plumbing to abstract over. A4 stands on containers and
  typeclasses instead.
- Familiarity for Effect developers weakens: the concepts map one to one, the
  syntax does not.

Versions in use: `salsa` 0.28, `inkwell` 0.10 (feature `llvm22-1`), `llvm-sys`
221.0.1, LLVM 22.1.8 from the official `clang+llvm-*.tar.xz` distribution — *not*
the winget package, which lacks the development libraries. See
`docs/llvm-setup.md`.

## Open design questions

These block specific phases. Each needs a short decision doc in `docs/design/`
before the phase that depends on it starts.

| # | Question | Blocks |
| --- | --- | --- |
| D13 | **What `[a, b, c]` denotes.** The literal parses today and means nothing: the checker gives `Expr::List` no type at all and the backend refuses it, so every use is either an inscrutable "type was never worked out" or a hard error. It has to become a `List`, an `Array`, or a thing chosen by its expected type — and if the answer is `List`, whether writing one requires `List` to be in scope. Found by `derive(ToJson)`, whose expansion tried to be its first user. Until it is decided, generated code writes `List::Cons` chains. | any phase that lets a program write one |
| D14 | **Whether `match` tests by equality.** Matching a `String` or float literal parses, checks, and reaches a backend that says it "needs a runtime comparison the backend does not generate yet" — so `match tag { "circle" => .. }` is a hard error at the end of the pipeline rather than a refusal at the start. Either the decision tree grows an equality test (`khora_str_eq` already exists and `==` compiles fine) or the pattern is refused where it is written. Silently accepting it through two phases and failing in the third is the one option that is wrong. | any phase that lets a program write one |
**Closed:**

- **D12** (what Khora promises not to break) is decided in
  `docs/design/compatibility.md`: **before 1.0, change carefully and say so;
  after 1.0, a program that compiles keeps compiling and keeps meaning the same
  thing, within an edition, for the life of a major version.**

  The consequential half is what is *not* observable, and it is why this came
  before Phase 9 rather than before publication: when memory is allocated and
  freed, and how much, is not something a program may depend on. Reuse analysis
  exists to change exactly that, and optimising first would have made an
  accident into a promise. Also unobservable: timing, `Map` iteration order,
  hash values across runs, and the text of any diagnostic.

  **Khora has no stable binary interface and will not have one.** Whole-program
  monomorphization with no dictionary passing means a generic function does not
  exist as code until something calls it at a type, so there is nothing to link
  against. A package ships source; the only stable ABI is C's, at `extern`.

  1.0 is blocked on package identity (10.2), declaration identity (8.5.2,
  errata 46), and an audit of everything `std` exports — each of which the
  document names rather than assumes.

- **D4** (what `[permissions]` enforces) is decided in
  `docs/design/permissions.md`: **the manifest decides what capabilities a
  program may hold, and the capability decides what may be done with it.** The
  first is compile-time and total — a scan of the requirement rows
  monomorphization already computes, which holds transitively through
  dependencies. The second is run-time, because `connect(config.host)` is not
  checkable any earlier and claiming otherwise is claiming to solve halting.

  A missing `[permissions]` table grants everything, categories are
  independent, and `default = "deny"` is the one line that flips it — the
  barrier to entry is nothing until somebody chooses otherwise. Wildcards are
  per-shape: `*` crosses dots in a host and stops at a separator in a path,
  because `*.internal` and `./data/*.json` both have to mean what they look
  like. Parsed and tested in `khora-manifest`; enforcement arrives with the
  capabilities in phase 8.

  The hole it names and does not close: an `extern fn` with no capability row
  reaches the OS with nothing in anybody's signature. The answer is an
  allow-list on `extern` itself, which needs package identity and so belongs
  to 10.2.

- **D8** (the FFI contract) is decided in `docs/design/ffi.md`: **a foreign
  function takes and returns scalars and pointers, requires capabilities
  without receiving them, and cannot raise.** Everything else is a Khora
  wrapper's job — which is not a limitation so much as errata 35's rule
  applied to a boundary the user writes rather than one the compiler
  generates, and the compiler now checks it. What remains under 7.3 is `Ptr`
  and how a buffer is lent across, and under 7.2 whether `extern` should be a
  keyword; neither changes the contract.

- **D10** (atomic reference counts) is decided in
  `docs/design/effect-runtime.md` §9: **atomic, with no way to opt out.** The
  forcing argument is not performance but correctness — a spawned fiber shares
  at least the closure it was handed, so a non-atomic count is a data race in
  the first concurrent program anyone writes. And a split is colouring: `Rc`
  versus `Arc` propagates into every signature that touches one, which is the
  one thing Khora's rows exist to avoid, and it would be there to save an
  increment. The cost comes back in phase 9, where an object that provably does
  not escape its fiber uses the cheap operations, chosen by the compiler and
  invisible in every type.
- **D11** (reference cycles) is decided in `docs/design/memory.md` §2, by
  phase 6 making one constructible. A `mut` field is shared by reference, so
  `a.next = b; b.next = a` is a cycle and Perceus stops being complete. A
  tracing collector was ruled out by non-negotiable 5 before this was a live
  question, so what remains is what was predicted: **a cycle leaks, and a weak
  reference breaks one.** The leak is bounded and quiet rather than unsound,
  which is the right failure to have — `khora_live_count` sees it, and every
  leak test in the repository is already watching. Weak references wait for the
  first parent pointer, when the shape of the problem is in front of us.
- **D3** (`Schema::Spec`) is decided in `docs/design/associated-items.md`. Two
  rules. A projection's owner must be *bounded* by a trait declaring the
  associated type — without a bound there is no impl to look it up in, and
  `forall <Schema> . .. Schema::Spec ..` as originally written names nothing.
  And a projection whose owner is still a variable *defers*, retried once the
  body is inferred, because the call's own return type is usually what settles
  it. Not the hardest case in the language after all: the coherence question
  was already answered by one impl per (trait, head), and what was left was an
  ordering problem with the same shape as the effect-row obligations.
- **D1** (handler execution) is decided in `docs/design/effect-runtime.md`. The
  deciding argument is reference counting: multi-shot capture must *copy*
  frames, so every reference in them needs a `dup`, so the runtime needs to
  know which stack slots hold counted pointers — a stack map, which is
  precise-GC machinery arriving through the back door of a language whose fifth
  non-negotiable is that it has no garbage collector. One-shot *moves* frames
  and needs none of it. Khora needs neither yet, because no syntax names a
  continuation.

- **D6** (typeclasses) is decided and implemented: Rust's `trait`/`impl`
  spelling, Rust's coherence rules, static dispatch through monomorphization,
  and higher kinds inferred from how a trait applies `Self`. See
  `docs/design/typeclasses.md`. The `std` trait list is decided there; the
  orphan rule is decided but waits on cross-package resolution to enforce.
- **D9** (imperative constructs) is implemented: `if`/`else`, assignment,
  `while`, `loop`/`break`/`continue` and early `return` all landed in phase 1.6.
  Generic `for` waits on the `Iterator` typeclass in phase 3. See
  `docs/design/imperative.md`.

- **D2** (what `Type.member` means) is decided in
  `docs/design/associated-items.md`. The premise is gone: "universal dot" is
  replaced by `::` for compile-time paths (modules, types, associated items,
  enum constructors) and `.` for runtime projection (fields, method calls), so
  `Effect::map`, `report.risk` and `RiskLevel::Low` are no longer spelled
  alike — errata #13. `::` resolves as module-or-type and never sees a local;
  `.` resolves as field, then item declared against the type. There is no UFCS
  fallback, and a field colliding with an associated item is an error at the
  declaration.
- **D5** (`ask` arity, errata #3) is dissolved by A8 — `ask(:label.op)` does not
  exist in direct style; you call `ledger.get_history(x)`.
- **D7** (effect and handler syntax) is decided in `docs/design/effects.md`:
  `effect` declarations, `with`/`raises` signature clauses, `raise`, `!` on
  fallible calls, `handler for` (whose value has the effect's own type),
  both installation forms,
  `catch`, and effect-row variables in generic signatures. Contexts are rows, so
  composing and overriding services is row update — and there is no layer
  memoization to reason about, because sharing is by name.

---

## Phase 0 — De-risk the environment — **complete**

Two bounded spikes, both landed. Each changed the shape of every crate
downstream, which is why nothing else started until they did.

### 0.1 LLVM toolchain spike — **done**

LLVM 22.1.8 is installed at `~/.llvm/llvm-22.1.8` and
`cargo test -p khora-codegen-llvm --features llvm` emits an object, links it
with `clang` and runs a binary that exits 42. Decision A2 stands: no fallback
needed, and Cranelift is not required.

Four Windows-specific obstacles had to be cleared; all are documented in
`docs/llvm-setup.md`, and the third is the expensive one to rediscover:

1. The `winget`/installer build omits `llvm-config`, the headers and the static
   libs. The full `clang+llvm-*.tar.xz` distribution has them, and needs no
   admin rights.
2. `llvm-config --system-libs` advertises `xml2s.lib`, which the distribution
   does not ship. An inert stub in the prefix satisfies it.
3. **LLVM's Windows libraries are `/MT`; rustc's msvc target is `/MD`.** The
   mismatched CRT heaps make unrelated LLVM calls fault with
   `STATUS_ACCESS_VIOLATION` — it surfaced in three different functions before
   the cause was found. Fixed with `+crt-static` plus a Windows SDK search path,
   which `llvm-sys` then requires.
4. `inkwell`'s default `target-all` references 17 architectures; this build has
   7. Restricted to `target-x86` and `target-aarch64`.

Host target `x86_64-pc-windows-msvc` only. The musl and darwin cross-targets in
§5.1 remain Phase 10.

### 0.2 Salsa spine — **done**

`khora-db` holds the database, the `SourceFile` and `SourceRoot` inputs, and the
first query, `parse(SourceFile) -> Parse`. `khora-syntax` stays salsa-free: it
remains a pure function from text to tree, which keeps it testable and fuzzable
without a database attached.

Salsa inputs are `Copy` handles, so `SourceFile` doubles as the `FileId` the
rest of the compiler will pass around; no separate id type is needed.

`khora check` was rewired to go through the database rather than parsing files
directly. One code path, not two — a separate CLI path would have drifted from
the one the language server uses, invisibly.

Two things fell out worth recording:

- `Parse` now derives `PartialEq`, which lets salsa *backdate* a reparse: an
  edit that produces an identical tree invalidates nothing downstream. Green
  nodes are hash-consed, so the comparison is cheap.
- `salsa::SalsaValue` accepts any `'static` value through a fallback, so rowan's
  `GreenNode` needed no manual implementation.

**Exit criterion met**, and asserted by `crates/khora-db/tests/incremental.rs`:
editing file B does not reparse file A. Four further tests cover caching,
backdating, and independence from `SourceRoot` changes.

---

## Phase 1 — Front-end completeness and effect design

Independent of Phase 0; can proceed in parallel.

- **1.1 `test` and `bench` declarations — done.** `test "name" { .. }` and
  `bench "name" { .. }` per §6.4.
- **1.2 Manifest parser — done.** `khora-manifest` covers §4.1 in full. Unknown
  keys warn rather than abort, each with a line and column, so an older
  toolchain degrades instead of refusing a newer manifest.
- **1.3 `khora fmt` — done.** `khora-fmt` normalizes indentation, spacing,
  blank-line runs and import lists, and **preserves the author's line breaks**
  rather than reflowing. That is what makes it idempotent and token-preserving,
  both asserted by property tests. It refuses to touch a file that does not
  parse.
- **1.4 Decide D2 — done.** `docs/design/associated-items.md` is decided: `::`
  for compile-time paths, `.` for runtime projection, no UFCS, and a field
  colliding with an associated item is an error at the declaration. Phase 2
  needs only the two `::` cases (module paths and variant constructors), so 2.1
  is unblocked without settling how associated items are *declared* — companion
  namespace or `impl` block — which is better decided alongside typeclasses in
  Phase 3.
- **1.5 Implement D7.** `docs/design/effects.md` is written and decided. Extend
  the grammar with `effect`, `with`, `raises`, `raise`, `!`, `handler for`,
  `catch` and `context`, then rewrite `std/` and `examples/risk_analyzer` in
  direct style. This supersedes the monadic API those files currently show.
- **1.6 Imperative constructs (D9).** `if`/`else`, assignment to `let mut`,
  `while`, `loop`/`break`/`continue`, early `return`. All self-contained grammar
  and lowering work. `if` should go first — it is a one-evening change and its
  absence is a daily papercut. Generic `for` waits for Phase 3; see
  `docs/design/imperative.md`.

**Exit:** corpus and manifests parse; `khora fmt --check` clean on the corpus;
property tests show `fmt(fmt(x)) == fmt(x)` and that formatting preserves the
non-trivia token stream; `std/` reads in direct style.

---

## Phase 2 — Vertical slice: Khora Core to a native binary — **complete**

`khora build examples/core_demo` produces an executable that prints 48, 55 and
25 and exits 0. Every allocation is freed, asserted by a test reading the
runtime's live counter, with a positive control so the check cannot pass
vacuously.

The milestone. A subset chosen to exercise every stage while excluding
everything hard.

**In:** modules; top-level monomorphic `fn`; `Int`, `Bool`, `String`; `let`;
arithmetic and comparison; user ADTs; `match` with constructor, literal and
wildcard patterns plus guards; calls; `|>` and `_`.

**Out:** effects, rows, generics, typeclasses, closures, records, cross-package
imports.

- **2.1 `khora-hir` — done.** Module graph, item collection, name resolution
  (per D2), body lowering with arena-allocated expressions and patterns, pipe
  and placeholder desugaring, lexical scopes. All salsa queries, each reading
  one file.

  `match` is *not* compiled to a decision tree here, contrary to what this item
  originally said. Exhaustiveness and reachability (2.2) are computed by
  Maranget's usefulness algorithm over a pattern matrix, and the decision tree
  is compiled from that same matrix — building the tree first would mean
  reconstructing the matrix in order to check it. HIR keeps the arms as
  written and the tree is compiled nearer codegen, which is what rustc does.
- **2.2 `khora-types`.** Monomorphic checking plus exhaustiveness and
  reachability over the decision tree.
- **2.3 `khora-perceus`.** Uniform boxed representation, `dup`/`drop` at scope
  boundaries. Reuse analysis deferred to Phase 9.
- **2.4 `khora-rt`.** New crate: allocator shim, RC header, `khora_alloc`,
  `khora_dup`, `khora_drop`, `print`. Static library.
- **2.5 `khora-codegen-llvm`.** HIR plus RC ops to LLVM IR to an object, linked
  against `khora-rt`.
- **2.6 Diagnostic harness (A7).** Snapshot tests over rendered diagnostics, so
  message quality is a tracked regression surface from the first error the
  compiler can emit — not a Phase 10 cleanup.

**Exit:** `khora build examples/core_demo` produces an executable that runs,
prints the expected output and exits 0; a counting-allocator test asserts every
allocation is freed; diagnostic snapshots are committed and reviewed.

---

## Phase 3 — Generics, HKT and typeclasses

Algorithm W with occurs check and let-generalisation, extended with a kind
system. Const generics as `Type::Const`. Typeclasses with instance resolution
per D6. Monomorphize in HIR before codegen so abstraction costs nothing at
runtime.

Also lands the `Iterator` typeclass and generic `for x in xs`, which was held
back from Phase 1 so the loop form is designed against the real protocol rather
than a `List` special case.

Done so far: inference and unification (`khora-types::unify`), monomorphization
by reachability (`khora-types::mono`), const generics as `Type::Const`, tuple
types, and traits with kinds, coherence and static dispatch
(`khora-types::traits`). Tuples arrived alongside const generics rather than on
their own schedule because a tensor shape is written `(M, K)`: without them the
shape argument typed as `Unknown`, which accepts anything, so the exit criterion
could not have been met honestly.

`for x in xs { .. }` is desugared in the front end to `loop`, `match` and
assignment over `Iterator::next`, so the checker, the reference-counting plan
and the backend need no notion of it. The protocol is
`fn next(self) -> Step<Self, Self::Item>` rather than Rust's
`fn next(&mut self) -> Option<Item>`: Khora has no mutable references and no
mutable fields, so the mutating shape is not available to express. What a
developer writes is the familiar loop; the protocol underneath is Khora's own.

`std::core` is written and type checks: `Ordering`, `Eq`/`Ord`/`Show`,
`Option`, `Result`, `List`, `Step`/`Iterator`/`Range`, and
`Functor`/`Applicative`/`Traversable` with `traverse` for both `Option` and
`List`. `the_standard_library_type_checks` runs it as one compilation so
cross-module imports resolve.

`std` is usable: `khora build <dir>` compiles every module in a directory into
one binary, and `a_program_runs_against_the_real_standard_library` builds a
program against `std/core.kh` itself — `for` over std's `Range`, std's generic
methods instantiated at the use site, a closure handed to std's `fold`, and
trait dispatch on a std impl.

Compilation is **whole-program**, not separate. A generic function is compiled
by substituting its type arguments into its body, so every module's source has
to be present at once — the constraint C++ templates and Rust generics have
too. A symbol therefore carries the module that *defines* it, so two importers
of one instantiation agree on a name and it is emitted once. Whether a compiled
artifact could ever stand alone is D12, and the answer is no:
`docs/design/compatibility.md` decides that Khora has no stable binary
interface and will not have one, because a generic function does not exist as
code until something calls it at a type. A package ships source.

`traverse` needed three things beyond ordinary generics, all of which landed
together: higher-kinded unification (solving `Self<A>` against `Option<Int>` as
`Self := Option` and `A := Int`), trait functions with no receiver so `F::pure`
can produce a container out of nothing, and method dispatch on a value of type
`F<B>` where `F` is a bounded parameter. The result is the single best
regression test in the repository — it exercises higher kinds, bounded
parameters, receiverless trait functions, closures through a recursive generic
call, and per-instantiation static dispatch, all at once.

Closures landed here rather than in a phase of their own: they were listed
under phase 2's **Out** and no later phase picked them up, yet `traverse` takes
a function argument and cannot be written without them. A closure is a heap
object holding its code pointer and its captures, under the same header as
every other value, so reference counting covers it with no new machinery. A
named function used as a value becomes a closure that captures nothing and
forwards.

`Iterator` and `for` need only what is already here: `for x in xs { .. }`
desugars to `loop` over `next()`, and `loop`, `match` and `break` all landed in
phase 1.6. `for` becomes a hard keyword at that point, as noted where it is
declared contextual in `crates/khora-syntax/src/kind.rs`.

**`traverse` is still blocked, on higher-kinded unification.** Its signature
reads `(self: Self<A>, f: (A) -> F<B>) -> F<Self<B>>`, where `F` is a type
*variable* applied to an argument. Declaring that already works — the trait
above type checks — but calling it does not: solving `F<B>` against `Option<Int>`
means deciding `F := Option` and `B := Int`, and `Type::Applied` currently holds
its head as a fixed name rather than something the unifier can solve. This is
the restricted higher-order unification every language with higher kinds
implements, and it is the last piece phase 3 needs. See `docs/errata.md`.

**Exit:** `matmul` with a mismatched shared dimension is a compile error naming
both dimensions **— met**; instance resolution errors name the missing instance
**— met**; a `traverse` written once works over `Option`, `List` and a user
type **— met**, see `traverse_works_over_three_containers` in
`crates/khora-codegen-llvm/tests/compile.rs`; `for` iterates a user-defined
type **— met**, see `a_for_loop_iterates_a_user_defined_type`.

---

## Phase 4 — Effect rows and handlers

- **4.1 Decide D1 — done**, in `docs/design/effect-runtime.md`. The framing
  turned out not to fit: the decided syntax has no `resume`, so every handler
  is tail-resumptive by construction and nothing can name a continuation.
  Effects therefore split into three mechanisms rather than one runtime —
  capabilities are evidence passed as parameters, failures are tagged returns
  checked at `!`, and suspension belongs to fibers in phase 5. None of it needs
  a stack segment, an unwinder, or a stack map.
- **4.2 Scoped row polymorphism — done** for signatures. `Type::Row` with
  Remy-style unification: shared labels agree, and whatever one side lacks has
  to fit through its tail. A call's row must be *subsumed* by the enclosing
  function's rather than equal to it, which is what lets a caller providing
  `{ ledger, ai }` call something needing only `{ ledger }`. No clause means
  the closed empty row, so an entry point is checked without annotating it.
  Row subtraction for handler installation lands with 4.3a. D3 still open.
- **4.3a Capabilities as evidence — done.** An `effect` is a record of
  closures, a `with` clause is extra parameters appended in label order, and
  installing a handler is a block of `let`s. No handler stack, no dynamic
  lookup, no stack map. Row subtraction falls out: a requirement raised inside
  a `with` block is discharged by it rather than reaching the signature.
- **4.3b Tagged returns — done.** A function whose `raises` row is non-empty
  returns `{ i1, i64 }`: the tag, and the payload as a word — one word is
  enough because every Khora value is word-sized. `raise e` releases what the
  frame owns and returns with the tag set; `f()!` reads the tag and branches,
  and the error path releases and re-returns. An uncaught raise reaching the
  entry point is a failing exit. A fallible call *must* be marked, so the
  branch is always where the source says it is. No tables, no personality
  routine, no unwinder — a raise is a return with a tag.
- **4.3c `catch` — done.** `f()! catch { .. }` handles part of the error row
  and subtracts exactly the error *types* its arms name. It is not a `match` on
  a result: it compiles to the branch `!` already emits, with the named types
  diverted to the arms and the rest returned onward. Naming a type commits to
  all of its variants, since a half-handled type would have to be both
  subtracted and left in. Discriminating them at runtime is what widened the
  tagged return to `{ i32 which, i64 payload }` — see
  `docs/design/effect-runtime.md` §2.
- **4.5 Effect rows on function types — done** in the type system. A function
  value's type is `(A) -> B with 'r raises 'e`, so naming an effectful function
  no longer charges its requirements to whoever wrote the name: they travel
  with the value and are charged where it is called. `List::map(analyze)`
  working is what `docs/design/effects.md` calls the single largest ergonomic
  difference from a monadic design. Calling *through a value* checks the same
  as calling by name, since the rows come from the callee's type rather than
  from a signature looked up by name.

  The backend follows the same rule. A closure's calling convention is read
  off its *type* the way a named function's is read off its signature:
  evidence appended in label order, a tagged return when the error row is not
  empty, and an adapter that forwards both. So one function value can be
  mounted once and served by two different handlers, which is what putting the
  requirement in the type buys over capturing it where the name is written.
- **4.4 Handler composition — done.** A service built on other services is a
  function returning a handler with a `with` clause of its own, which needed
  nothing new: a handler is a record, so building one is calling a function.
  Merge is a row with two labels — one `with` block installing both. Building a
  handler can raise, which is the region's failure rather than the served
  computation's. `Handler<E>` is gone: an effect's name is the type of its
  handlers, so `with { db: Db }` and `fn postgres_db() -> Db` agree without a
  wrapper to unwrap.

**Exit: the reference application typechecks — met *except for one thing*, and
the exception was found late.** Pinned by
`the_reference_application_type_checks_but_for_one_thing`. An unhandled
capability is rejected with a diagnostic naming the absent label and the
function that required it — **met**.

The correction is worth making plainly. This read as met for a long time
because `ai.extract` is declared `forall <A: Extract> . (Prompt, A::Spec) -> A`,
the checker had nowhere to put the `A`, and the `Unknown` it produced agreed
with everything downstream. The `Unknown` audit in 8.3a is what turned that
silence into a sentence — the same way entry 24's test was green for the wrong
reason. Everything else in the program fits; that one construct has no decided
meaning.

Serving a request needs real I/O and a backend that can build a value out of an
effectful function; both belong to phase 5 and after.

Three holes turned up only once a whole program was checked at once, each of
them a place where something arrived as `Unknown` and was therefore accepted:
an imported `effect` brought neither its type nor its operations, `with Mock`
installed nothing at all because a named context is not a record literal, and a
bare `'r` in type position parsed as no name.

---

## Phase 5 — Structured concurrency

Fibers, cancellation that runs finalizers, `Scope`-bound resource lifetimes,
`Schedule` policies.

- **5.1 Regions and finalizers — done.** A region is a reference-counted object
  whose release runs its finalizers, in reverse. That makes every path that
  ends a region a path that releases a binding, and code generation already
  emits all of them — the end of a block, an early `return`, a raise passing
  through. No new rule about unwinding, and no second notion of a scope beside
  the one Perceus has. The root region ends after `main` returns, on the
  failing path too. `docs/design/effect-runtime.md` §10.

  `Scope`'s operation became `defer: (() -> ()) -> ()`, with `acquire` an
  ordinary generic function on top. A handler's fields are closures and a
  closure is monomorphic, so an operation cannot quantify over a type — and it
  should not: the effect decides where finalizers go, and the rest is a library
  function anyone could have written.
- **5.2 Cancellation as an injected raise — done.** A cancellation travels on
  the same tagged return an error does, under a `which` no error type can be
  assigned, so `catch` cannot swallow it, no row grows because of it, and the
  unwinding is the unwinding that already existed. The promise holds: **a
  computation can only be interrupted at a point the source marks with `!`**,
  and every region between the mark and the root runs its finalizers on the way
  out. A cancellation reaching the entry point exits 130, which is what a shell
  already means by interrupted. `docs/design/effect-runtime.md` §6.

  The flag is process-wide until fibers make it per-fiber; nothing in generated
  code reads it directly, so that is a change inside the runtime.
- **5.3 Fibers — done.** `docs/design/fibers.md`: a fiber is
  a stackful coroutine multiplexed onto worker threads, and the first
  implementation makes each one an operating-system thread. Not a hedge — the
  same argument as D10. A program sees `spawn`, `join`, `cancel` and a nursery,
  is correct under either implementation, and cannot tell which is running
  except in how many fibers it can afford.

  A state-machine transform is rejected outright rather than deferred: it is
  the one option that would have to be designed into the compiler now, and it
  buys speed at the cost of the property the language is for.

  **Built:** `spawn`, `join`, `cancel`, and a cancellation flag that is now one
  per fiber rather than one per process. The structured half came free —
  releasing a fiber handle *joins* it, so a fiber cannot outlive the binding
  that holds it, on every path out including a raise. Nobody writes `join` and
  nothing can escape. That is 5.1 paying for itself a second time, with no part
  of it designed for this.

  **A fiber root absorbs a cancellation.** The spawned thunk is
  `() -> () raises 'e`, so the runtime reads how the fiber ended and a
  cancellation stops *that fiber* rather than the program. The rule reads the
  same from a fiber's side as anywhere else: a fiber with no error row has no
  channel to be interrupted on, and runs to its end.

  **A nursery is a value whose release stops what is still running.** So a
  fiber cannot outlive the block that spawned it on any path out, and nobody
  writes the cancel. The two endings — wait for the children, or cancel them
  and then wait — needed no way to ask which happened: the normal path waits
  *before* the release, so the release only ever runs on the other one.

  **Left for later, on purpose.** The coroutine itself: fibers are threads
  until there is a scheduler, and that is a change inside `khora-rt` that no
  program can see. And failure *propagation* — a child's error goes to stderr
  because the parent has nowhere to put it, which is a mutable cell and
  therefore D11.
- **5.4 `khora test` — done.** A `test` block lowers to an ordinary function
  body, which means it is *checked* — it was not before, and the reference
  application's tests had been quietly wrong for some time as a result — and
  everything the language can do works inside one with no special cases. Its
  error row is open, because an error escaping a test is a failing test rather
  than a program that does not compile.

  `khora test` compiles the program with a different entry point and gives each
  test a fiber of its own. Tests are the first thing anyone writes that is
  embarrassingly parallel, and a test that only passes when it runs alone is a
  test that is lying. `docs/design/testing.md`.

  `assert` needs no `!`, and only inside a `test` block: every test framework
  the audience knows ends the test on a failed assertion without annotating it,
  an assertion is the one place a reader of a test already looks for control
  leaving, and the bend is bounded by refusing `assert` anywhere else.
- **5.5 `Schedule` policies — done**, and in Khora rather than in the
  compiler, which was the claim worth checking. `retry` and `repeat` are
  ordinary functions over `attempt`, and a schedule is a record.

  `attempt` is the one new primitive: it turns the error channel into a
  `Result`, which `catch` cannot do because `catch` names constructors and this
  names none. It is also what makes retrying possible at all — a policy that
  runs a computation again cannot know what the computation was doing.

  Getting there needed `raises E` with `E` a type parameter to work, which it
  did not: an error row's label *is* its type's name, so an entry whose type is
  a fresh variable has no label to be matched by. Such an entry now matches by
  position among the leftovers, and substituting or solving it relabels.
  `docs/design/effects.md`.

  A schedule carries no clock. One that does needs I/O, and a policy that can
  be read, compared and tested without one is worth having first.

**Exit — met.** A canceled fiber runs every finalizer in scope, verified by
`a_cancelled_fiber_runs_every_finalizer_and_stops_only_itself` in
`crates/khora-codegen-llvm/tests/fibers.rs`, which pins the other half of what
"stops" has to mean as well: the program carries on. And `khora test` runs
isolated fibers across cores, pinned by
`crates/khora-codegen-llvm/tests/testing.rs`.

---

## Phase 6 — The values a library needs

Nothing above this can be judged until it exists. The whole type universe today
is `Int`, `Bool`, `String`, `()`, ADTs and tuples, which is enough to write a
compiler test and not enough to write a hash map. Every item here was on the
critical path whatever the ecosystem strategy turned out to be — which is the
argument that settled A6.

- **6.1 D11, mutable fields, and what may cross a fiber — done.** The widest blocker in
  the language: no hash map, no buffer, nothing that accumulates. It has already
  cost real work — a nursery cannot hold a child's error, and `retry` needed a
  runtime counter to be testable.

  **Decided.** A mutable field is *shared by reference*: two bindings to one
  record see each other's writes, which is what a hash map needs and what a Go,
  Rust or TypeScript reader expects a struct field to mean. Requiring an
  explicit cell instead would be importing `RefCell`, which exists in Rust to
  work around a borrow checker Khora does not have.

  That makes cycles constructible, so **D11 resolves as it was predicted to**: a
  cycle leaks, and a weak reference is what breaks one. The DAG invariant that
  made Perceus provably complete ends here, deliberately, and
  `docs/design/memory.md` §2 says so while it is still true.

  **And a mutable value cannot be captured by a spawned fiber.** Refcounts are
  atomic (D10) so *sharing an immutable value* across fibers is already safe,
  but nothing is mutable yet, so a data race is currently not expressible —
  shipping mutation without this rule would ship Go's problem into a language
  that does not have it. Khora can afford the rule cheaply because there is
  exactly one place a value crosses a fiber boundary: the captures of a spawned
  closure. One structural, transitive property, checked in one place, over a
  list the checker already publishes. No lifetimes and no borrow checker,
  because there are no borrows.

  `Shared<A>` — the synchronized thing that *can* cross — waits for phase 7 or
  8, where there is I/O worth parallelising and a channel may turn out to be the
  better primitive. Until then a nursery whose children all write into one
  collection is rejected. That is real friction and it is the honest cost of
  the rule.
- **6.2 Fixed-width integers, and bytes — done, except for strings.** `U8`,
  `U16`, `U32`, `U64`, `I8`, `I16` and `I32`. `I64` is not among them: it is a
  second spelling of `Int`, because two 64-bit signed integers would mean a
  conversion between them that can never fail and never does anything.

  Everything is at the type's own width, which is the only thing that makes any
  of it worth having: a `U8` addition traps at 255 rather than 2^63, `255 < 100`
  is false because an unsigned type compares unsigned, and `>>` brings in zeros
  for an unsigned type — the logical shift `Int` could never express. An
  `Array<U8>` is **one byte per element**; the array header carries the stride,
  and a byte buffer that cost eight bytes a byte would not be one. FNV-1a over
  a byte array is in the tests, which is the first real hash in this repository.

  A literal takes the type being asked of it — `let b: U8 = 65`, the `56` in
  `U8::wrapping_add(b, 56)`, and the `0` in `Array::new(4, 0)` when the binding
  says `Array<U8>`. It is a hint rather than a demand, re-armed only where a
  type passes through unchanged, so the `0` in `array[0]` stays an index. The
  sign is part of the literal too: `-128` is an `I8` even though `128` is not.

  Conversions are explicit and go through `Int` — `U8::of` traps, `U8::wrapping`
  truncates, `U8::to_int` goes back — which is four methods per type instead of
  one for each of the forty-two ordered pairs. `docs/design/numbers.md` has the
  reasoning.

  Two older bugs came out of it, both silent: a `let` annotation was parsed and
  then ignored (errata 36), and a row entry's label went stale when its variable
  was solved from outside the row (errata 37). A third was in the test harness
  rather than the compiler — every test binary now builds the runtime archive it
  links, because one of them doing it was a race the others sometimes lost
  (errata 38).

  **Overflow traps, in every build.** Swift's answer rather
  than Rust's: a program that passes its tests and then wraps in production is
  the failure worth spending a branch to prevent, and two behaviours put the
  difference where it is most expensive to find. LLVM's `with.overflow`
  intrinsics return the result and the flag together, so the check is a branch
  the optimizer can usually see through, and phase 9 can remove many of them.

  `/` and `%` trap as well, on a zero divisor and on the minimum over minus
  one. Both are undefined in LLVM and both fault on hardware with no message
  attached, so a trap that names the operation is strictly better whatever the
  eventual answer is — and whether a division by zero should *raise* rather
  than trap is still open, because a divisor off a socket is data rather than
  a mistake. `docs/design/numbers.md` has the argument; `Int::checked_div` is
  probably where it lands.

  `Int::wrapping_add` and its siblings are how you ask for the other thing, by
  name, in the places that genuinely want it — a hash, a checksum, a PRNG. The
  bit operations landed with them, which is what let the hash map stop
  apologising for its hash.

  `^`, `&`, `|`, `<<` and `>>` are five new tokens and `>>` has to be told
  apart from the end of two nested type arguments. Not hard, and not what a
  hash function was waiting for.

  Methods rather than operators for the bit operations, and the same seven
  operations exist on every fixed-width type — `docs/design/numbers.md` has the
  table and why it reads like one.

  **A string's bytes**, finally. `String::byte_length`, `String::byte` and
  `String::bytes` — named for bytes because a `String` is UTF-8 and a `length`
  that quietly meant characters would be wrong for half its callers and silent
  about which half. `+` on two strings works now too; it had been declared and
  unimplemented since phase 2.

  There is deliberately no `String::from_bytes`. Going the other way has to
  answer what happens to bytes that are not UTF-8, and the honest answer is a
  `Result` rather than a trap — bytes off a socket are data, not a programmer's
  mistake. That wants the error channel wired into an intrinsic, which belongs
  with phase 7 rather than being decided in passing.
- **6.3 Floats — done.** `Float` is IEEE-754 double precision: `f64` in the
  backend, a decimal literal in the grammar, `+ - * /` and the six comparisons.
  No overflow trap, because IEEE arithmetic reaches infinity rather than
  wrapping — the opposite of the integer rule, for a reason rather than by
  oversight.

  **`Float` implements neither `Eq` nor `Ord`, and that was decided rather than
  asked.** `==` and `<` are *primitive* on floats and mean what IEEE says, which
  is what every reader coming from Go, Rust or TypeScript expects; but `Eq` in
  `std::core` is an equivalence, and `NaN == NaN` being false means a
  law-abiding `Eq` cannot include `Float`. The operator is primitive; the trait
  is for lawful equality. Khora can afford the split without Rust's second trait
  because `==` never went through `Eq` to begin with. The cost is real and
  intended: no `Float` keys in anything that hashes.

  **No implicit promotion.** `1 + 2.0` is an error — the left operand decides
  which arithmetic is happening and the right must match. Go and Rust both do
  this, and it is what stops a rounding surprise from being invisible.

  `docs/design/numbers.md` carries the reasoning for this and for the overflow
  rule above. `Float32` is not here; one float type is enough until `std::ai`
  needs the other.
- **6.4 Arrays — done**, and taken before 6.2 and 6.3 because the phase's exit
  is a hash map and arrays are what block it. Contiguous, fixed-length,
  bounds-checked; `List` is a linked list, which is the wrong shape for almost
  everything and the wrong shape for reuse analysis to pay off on.

  Fixed length because growing is a *library* question, and it turned out to
  be one with a wrinkle. `Vector<A>` is in `std::core` now, but it is
  `{ mut items: Array<A>, mut len: Int, mut wanted: Int }` — `Array::new`
  wants a value to fill with and an empty vector has none, so the backing
  array could not be made at all until `Array::empty()` was added beside it.
  The third field is what keeps `with_capacity`'s promise in the meantime. See
  the "Not yet" entry, which carries the mistake that came first.

  Allocation and release are runtime calls, because both need the length at run
  time. Reading an element is a bounds check and a load, because the layout is
  a contract with the code generator rather than something the runtime hides.
  An index outside the array stops the program and says which index and what
  length — the same reasoning as trapping on overflow.

**Exit — met, and then met properly.** `Map<K, V>` is in `std::core`, written
in Khora: an array for the buckets, a `mut` field for the count, and a
recursive ADT for each chain. Sixty inserts followed by sixty removals leave
the live-object count at zero —
`a_round_trip_of_inserts_and_removals_leaves_nothing` in
`crates/khora-codegen-llvm/tests/hashmap.rs`.

Its key is **any type with a `Hash`**, which is what the bytes were for. `Hash`
is a trait in the standard library requiring `Eq` — equal values must hash
equal, or an entry can be inserted and never found — and `impl Hash for String`
folds FNV-1a over the bytes, in Khora. `Float` is not an instance, because it
is not an instance of `Eq`.

Getting there needed a compiler fix: a bound on an *impl block*'s type
parameters parsed and then meant nothing, so `impl<K: Hash, V> Map<K, V>` was
told `K` had no bounds (errata 39). `the_standard_maps_keys_can_be_strings` in
`tests/modules.rs` runs against the real `std/core.kh`, and includes a key
built at run time so the lookup cannot be matching on a pointer.

One thing the map is still honest about: its keys are `Int`. There are bytes
now, and an `Array<U8>` is packed, but nothing turns a `String` into one — so
hashing a string is the piece still missing rather than the bytes themselves.
The hash itself is no longer an apology — once wrapping multiplication and the
bit operations landed, `Map::slot` became a Fibonacci multiply and an xor-shift,
which is what a hash is supposed to look like.

---

## Phase 7 — The foreign boundary

Per A6 and `docs/design/ecosystem.md`. Small, because the boundary already
exists: every runtime call generated code makes is a C ABI crossing, and the
rule for what may cross was settled the hard way in errata 35.

- **7.1 D8 — decided, in `docs/design/ffi.md`.** In one line: *a foreign
  function takes and returns scalars and pointers, requires capabilities
  without receiving them, and cannot raise; everything else is a Khora
  wrapper's job.*

  **And the compiler now holds to it.** A function declared without a body is a
  foreign function, and its signature is checked where the call is generated: a
  parameter or return the C ABI cannot carry is an error naming the type and
  the reason. Before this, `fn f(p: Pair) -> Int;` compiled and handed a
  reference-counted heap object to C — only the missing symbol stopped it, and
  a symbol that happened to exist would have been worse. That is errata 35's
  rule turned from a lesson into a check.

  `raises` on a foreign declaration is refused, and for the better of the two
  available reasons: a fallible function returns the very aggregate errata 35
  says must not cross, and *C has no error channel* — it has a return value
  that means something, differently in every library. The three lines that turn
  a negative return into a raise are written in Khora, where a reader can see
  which convention this one uses.
- **7.2 Effect clauses on a foreign declaration — the capability half is
  done.** **A `with` clause on a foreign function is a permission, and nothing
  is appended to the call.** A C function has no use for a Khora record of
  closures, but the requirement is worth everything: nothing can open a file
  without holding `Fs`, and `Fs` is not something a function can conjure — it
  comes from `main`, through every frame that needs it, visible in every
  signature on the way. That is D4's teeth, with no runtime check and no
  sandbox.

  A foreign function is opaque, so its row is a promise the compiler takes on
  trust and then enforces on every caller. Which is the argument for bindings
  to the operating system living in the standard library, reviewed once,
  rather than being written afresh in every package.

  **`extern fn` says it out loud.** A function without a body used to *be* a
  foreign function, silently — the same trap as errata 36 and 39, with the
  worst possible symptom: a misspelled name became a C symbol nobody defines,
  and the only sign was `undefined symbol` from the linker. Now there are three
  kinds of declaration and they are three things: a Khora body, an `extern fn`
  that is a C symbol, and a bodyless `fn` that is a promise nobody has kept
  yet. The last is still allowed — `std::net::http` is nothing but those — and
  refused at the *call*, in a sentence that names the function and suggests the
  keyword.

  Almost every language with an FFI makes you say it: Rust's `extern "C"`,
  TypeScript's `declare`, C#'s `extern`, Java's `native`, Kotlin's `external`,
  Zig's `extern`, Haskell's `foreign import`. Go allows the bodyless form but
  refuses it unless the package really contains assembly. C is the one place a
  bodyless declaration silently means "elsewhere", and C is where the
  undefined-symbol experience comes from.

  Contextual rather than reserved, on the principle the keyword audit
  established: it costs nothing here, and adding the word cannot break a
  program already using it.
- **7.3 Foreign resources as counted values — done, and it needed nothing.**
  `acquire(value, release)` registers a release with the enclosing `Scope`, a
  `Scope` is a region, and a region runs its deferred work on every way out.
  So `acquire(open_file(path), fn f => { fclose(f); })` closes the file on
  every path out including a raise passing through — not because a file is
  special but because everything is a counted value and this is what counted
  values already did.

  `a_file_is_closed_on_the_error_path` does not take Khora's word for it: the
  program opens a real file, registers the close, raises, and lets the raise
  leave the region; then the *test* deletes the file, which Windows refuses to
  do while a handle is open.

  **`Ptr` exists now**: a C `void *`, opaque, not counted, not dereferenceable,
  and never a pointer into Khora's own heap. `Ptr::null` and `Ptr::is_null` are
  the whole of what it can do. Since nothing turns a Khora value into one, a
  dangling `Ptr` is not something the language can express — every pointer that
  exists came from the other side.

  **A buffer is lent by `with_data`**, which hands a body a `Ptr` and a count
  for the duration of the call and no longer. The bound is a call because no
  other bound is right: a bare `data(self) -> Ptr` is a dangling pointer the
  compiler creates for you, since Perceus releases the array at its last use —
  the `data` call itself — and no scope is right for all of a straight line, a
  branch and a loop body. The array is released by a scope rather than by a
  statement after the call, so a body that raises does not leak it; errata 34,
  for the third time. Only an array of numbers can be lent, because an array of
  Khora objects holds counted pointers and handing those across is the mistake
  the boundary exists to prevent.
- **7.4 Syscalls — files done, sockets blocked on something else.** `fopen`,
  `fread`, `fclose` and `strlen` are ISO C: the same names on every target
  Khora has, and `FILE *` is exactly what `Ptr` is for. `tests/files.rs` reads
  a real file into an `Array<U8>` with no Rust anywhere in between, from seven
  declarations.

  `String::with_c_string` is what made it possible — every C function taking a
  string takes a `const char *` and finds the end by looking for a zero, and a
  Khora string knows its length instead. A copy, necessarily, living exactly as
  long as the call.

  **A socket is not ISO C** — it is Winsock or it is Berkeley sockets, and the
  two do not even agree on what a socket *is*. Choosing between them is no
  longer the obstacle, though: see 7.5. What remains is writing them, which is
  a `std::net` and therefore phase 8's.
- **7.5 One target's files at a time — done.** A source file whose name ends in
  `_windows`, `_linux`, `_macos` or `_posix` is compiled only on those targets,
  so two files may declare the same module and at most one is ever in the
  build.

  Go's rule, for Go's reasons. An `#[if(windows)]` attribute would put two
  targets' code in one file — every reader reads both, the compiler parses
  both, and a third target grows a nest of conditions in the middle of
  otherwise ordinary code. A suffix keeps each target's version whole and
  readable on its own, and makes *which files did this build use?* a question
  `ls` can answer. It deliberately cannot make a *fragment* differ: if two
  targets share ninety per cent of a file, the other ten belongs behind a
  function they both call.

  A file named outright on the command line is read whichever target it names,
  because asking for a file by name is asking for it.

**Exit — the file half is met.** Read a file, from Khora, with the file closed
by the region that opened it, on the error path as well as the ordinary one:
`tests/files.rs`. Writing it to a *socket* now waits only on somebody writing
`std::net`, which is phase 8.

---

## Phase 8 — A standard library worth using

The first real test of whether Khora is pleasant to write libraries in, which
is a question no amount of compiler work answers. Collections, strings and
encoding, JSON, time, logging, and HTTP over the syscalls from phase 7 —
everything a normal program touches, written in Khora and generic over its
effects.

- **8.1 `std::fs` — done, and it is the answer to the question above.** The
  whole module is written in Khora: the C conventions, the region that closes
  the file, the effect that gates access to it.

  Two layers, and the split is the point. *Inside* are the foreign declarations
  and the code that gets their conventions right — a null `FILE *` means the
  open failed, a short read means something went wrong, and the file has to
  close on the path where it failed as much as on the path where it did not.
  None of that is exported; the module is the trusted boundary, the way Rust's
  standard library is where `unsafe` lives so nothing above it needs to.

  *Outside* is the `Fs` effect, which buys two things at once. It is a
  **permission** — a function that reads a file says so in its signature, all
  the way up to the `main` that allowed it — and it is a **seam**: a test
  installs its own handler and the code under test cannot tell.
  `the_file_system_can_be_replaced_wholesale` reads `/etc/passwd` on Windows
  and gets back what the mock decided, which is what an effect system is *for*
  and what a file-system mock is usually a poor imitation of.

  `String::from_bytes` and `Array::is_utf8` came with it, and are two things
  rather than one on purpose. The conversion **traps** on bytes that are not
  UTF-8 — the same bargain `Array::get` makes about an index, where the check
  exists and calling without it is the mistake. An `Option` would have put the
  decision in the wrong place: what to *do* about bytes that are not text
  depends on where they came from, and `read_text` is where that is known.

  Found on the way and then fixed: a mock that cannot fail now satisfies an
  operation declared to fail. See 8.1a.
- **8.1a What a lambda raises is a lower bound.** A body's error row says what
  it raises *at least*; the context may ask it to be declared as raising more.
  So a stub that never fails satisfies `raises IoError`, because raising fewer
  things is always safe — and a test double no longer has to raise on a branch
  it never takes, which was a tax on exactly the code an effect system exists
  to make easy.

  Mechanically it is small: a lambda's inferred row is left *open*, with a
  variable tail that whatever it is checked against fills in. What made it more
  than one line is what happens when nothing fills it in — an open row is a
  fallible one to the code generator, so every lambda would have returned a
  tagged pair for nothing. A tail nobody constrained is closed to empty once
  the body is done: nothing said this could fail, so it cannot.

  The widening only goes one way. Raising something the interface did not
  mention is still refused, a body that can fail is still refused where nothing
  may, and a call that really can leave still needs its `!`.

The bindings A6 names — TLS and crypto, compression, numeric kernels — are
consumers of phase 7 and are *not* on this critical path. A great deal of Khora
can be written before anything needs TLS.

- **8.2 One entry point, and the manifest names the rest — done.**
  `khora build ./app` is the whole of what a developer says. Which packages it
  is built against is a property of the package rather than of the invocation,
  and repeating it at every call is how the two come to disagree. `path`
  dependencies in `[dependencies]` are resolved relative to the nearest
  `khora.toml`; a `version` says plainly that it needs a registry, which is
  10.2.

  **`std` is not declared at all.** It is found beside the compiler, the way
  `rustc` finds its sysroot and `go` finds `GOROOT` — a line every package
  repeats and none can get wrong is not a line worth writing, and a program
  with no manifest still has a standard library. Same search as the runtime
  archive: `KHORA_STD`, then beside the executable, then this workspace.

  This is what made the reference application measurable. Building it went from
  twelve `cannot find module` errors to six real ones, which is now the list
  phase 8 is working through.

- **8.9 What a program is told, and what time it is.** `std::env`: arguments,
  environment variables, and a clock, as two capabilities — `Env` for what the
  process was handed, `Clock` for the time, because they are different things
  to deny a dependency and a test usually wants to pin them differently.

  **This is the smallest thing separating a program from a demo.** Until it
  existed, nothing a Khora program did could depend on anything outside its own
  source: every path, port and setting was compiled in. Almost all of it is ISO
  C — `getenv`, `strlen`, `memcpy`, `time` — and the one exception is the
  argument vector, which no C function returns because it arrives through
  `main` and is gone. So the generated `main` takes `argc` and `argv` and hands
  them to the runtime first thing.
- **8.10 Numbers as text, and order.** `Float::to_string`, which a program
  could `print` but could not put in a message; **`<` reaching an `Ord` impl**,
  the same bargain `==` makes with `Eq`, resolved through a bound inside a
  generic function so `A: Ord` means what it looks like; `Ord for String` by
  byte order; and `List::sort`, a stable merge sort in Khora.

  The sort was not stable at first. It split by dealing alternately, which is
  one pass and needs no length — and puts two equal elements at positions 1 and
  2 into *different* halves with the later one on the left, so a merge that
  ties towards the left swaps them. The test said so. It counts to the middle
  now.

- **8.11 JSON.** `std::json`, written in Khora: `encode`, a recursive-descent
  `parse` reporting a byte offset, and accessors. Complete on escapes both ways
  including surrogate pairs, because a parser that cannot read an emoji is not
  one. Numbers accumulate as `Float` throughout — an `Int` accumulator is exact
  for small values and *traps* on overflow, and a parser that dies on input it
  was handed is not a parser either.

  It found errata 44: a variant payload narrower than a word was read as an
  `i64` and stored into its own smaller slot, writing over the frame. A leak in
  `Option<Bool>` was the symptom and a stack buffer overflow was the cause.

- **8.12 `std::net::http`, for real.** Query strings with percent-decoding, and
  the path a route matches is the part before `?` so a client adding a
  parameter does not break a route. Headers, folded to lower case *on arrival*
  rather than on lookup, so two spellings collide — which is the correct
  reading — and a response that can carry its own. A body read to its
  `Content-Length` across as many `recv`s as it takes, into a buffer allocated
  once, so a lying length cannot make it allocate.

  Two findings came out of it and neither is about HTTP.

  **`std::core`'s text helpers recursed once per byte**, and the stack gave out
  around 9 KB — which capped *every* Khora program's text handling, not just a
  request body. The comments justified recursion with "a `while` would need a
  `mut` binding and this needs neither", which was wrong: it needs a bounded
  stack. `slice`, `index_of`, `matches_at`, the byte comparison behind
  `Ord for String` and the string hash are all loops now, and a 100 KB slice
  works. `Array::prefix` also trapped on an empty array, which is fixed.

  **A fiber per connection was impossible**, and that was 8.13.

- **8.13 What may cross into a fiber — decided, and three holes closed.**
  `docs/design/sharing.md`. An effect is a record of function types and a
  closure is unshareable because its captures are not in its type, so **every
  handler was unshareable and a fiber could never be spawned from a function
  holding one** — the shape of every concurrent server.

  Decided: the question is asked where the thing being asserted is *visible*.
  An effect is shareable, paid for by checking each operation at the
  `handler for` literal; a closure that has to be forwarded is wrapped by
  `SharedFn::of`, checked the same way, which is how a `Router` full of
  handlers now crosses. Rejected: a shareability bit in the function type,
  which colours every container of a function; and a blanket ban on capturing
  anything writable, which passed the whole corpus and would still have made
  an ordinary accumulating callback illegal.

  An outside review of the first attempt found three ways to get an unshareable
  value onto another fiber, all of which compiled and raced:

  - **An opaque type answered "yes" for want of anything to look at.** `Array`
    is written through `Array::set` and `Ptr` points at foreign memory; both
    are bodyless. A type with no body is now unshareable until `impl Share`,
    which may only be written where there is nothing to check.
  - **A generic function laundered anything.** `fn f<A>(v: A) -> Fiber` spawned
    with `v` captured, and `A` was assumed shareable. It now needs `A: Share`.
  - **A pre-bound closure bypassed handler certification.** `let leak = fn ..`
    then `handler for C { op: leak }` had nothing at the literal to look at,
    and was accepted. Refused now.

  The runtime owes what the impls promise, so `Region`, the nursery and a fiber
  handle all take locks, and `khora_fibers_wait` drains in rounds — a child may
  adopt while its parent is waiting, and one pass would return with a
  grandchild still running.

  The HTTP server answers on a fiber per connection, pinned by a client that
  connects and says nothing while another is served past it.

- **Errata 45 — a wildcard `catch` was checked but not generated.**
  `lower_catch` grouped arms by the error type they name; `_` names none, so it
  was skipped and the switch's default still propagated. The checker said the
  function could not fail while the code took the unhandled path, and a program
  with no `raises` clause walked into `unreachable` — `catch { _ => -1 }` around
  a raise printed nothing and exited 130. `std`'s own `serve_connection` was
  built on it, so a failing handler would have killed the fiber rather than
  answering 500.

  The arm takes the fall-through now, with cancellation and a test failure kept
  on the propagate path by explicit cases: a `_` that stopped a cancellation
  would break every nursery. Releasing what it caught needed a new piece — the
  arm binds nothing and has no static type, and the row may be `'e`, which
  nothing at that point can enumerate. `Backend::emit_error_releaser` emits one
  function at the end of compilation, when every error type has an id, and
  switches on it. Pinned by a live-object count of zero after catching an error
  that carried a boxed field.

- **Errata 48 — a lambda did not know its parameter type until too late.**
  `expect` works out what an argument has to be, sets it as the hint, infers,
  then requires — and the lambda arm ignored the hint, meeting the expected
  type only in that final `require`. Too late for anything inside: a `match`
  destructuring the parameter bound its name against a type that was still a
  variable, `bind_pattern` cannot take one of those apart, so the binding got
  `Unknown` and every field read off it did too. Silently, because an unsolved
  owner is the one case a field read declines to complain about — so it
  surfaced as the `Unknown` audit failing at the end, pointing at
  `url: found.url` while `found.created` beside it was fine. The same body as a
  *named* function worked, which is the tell.

  The parameter variables are unified with the expected type's before the
  patterns are bound. Found by writing the link shortener, which is what it
  took: no test in the suite passed a lambda that destructured its own
  argument.

- **Errata 47 — a generic record's field was read as declared.**
  `read_field` asked the *declaration* for a field's type, so `Pair<K, V>`'s
  `value` was a rigid parameter — never boxed — and a `Pair<Int, String>` loaded
  its string as an integer, then handed it to a `+` that wanted a pointer. Drop
  glue had already learned this and had `instantiated_variants`; field access
  had not, and only a generic record whose field is boxed at one instantiation
  and not another could show it. `Dict` was the first. Both read the fields at
  the instantiation now.

- **Errata 46 — `Share` was forgeable.** It was recognised by the bare name, so
  any file could declare `trait Share {}`, write `impl<A> Share for Array<A>`,
  and hand two fibers an array that `Array::set` writes. Only the module that
  declares a type may assert its shareability now, and an imported impl is
  skipped rather than re-judged.

  What remains is identity: `Share`, `Fiber`, `SharedFn` and the rest are still
  matched by name rather than by where they were declared, so a file declaring
  its own `Array` gets the array intrinsics. Not new and not reachable by
  accident, but it is what stands between this and calling `Share` a boundary.
  It wants compiler-known identity for the whole set at once.

**Exit — the real one.** *You can write a useful program.* The served request
below is a milestone rather than the criterion: it proved the stack works end
to end and it measured the compiler, not the library. What the library still
owes is in "Not yet" at the end of this section.

**Milestone — met.** The reference application runs and serves a request,
pinned by `the_reference_application_serves_a_request`. A real socket, a real request
line, `/analyze/:account_id` matched and its parameter bound, the handler run
with the `ai` and `ledger` capabilities its signature asked for, the ledger
flagged, and a 200 with a JSON body whose `Content-Length` is its actual
length:

```text
POST /analyze/acc_9921 HTTP/1.1

HTTP/1.1 200 OK
Content-Type: application/json
Content-Length: 87

{"account_id": "acc_9921", "risk": "critical: Immediate fund freeze", "confidence": 98}
```

That is phase 4's unmet half, closed. It was the criterion because it exercises
the whole stack, and it did: every gap it found is listed above, and none of
them was in the part anybody was worried about.

- **8.3a The checker will not finish a body it did not understand.** A
  published type of `Unknown` is now an error: `Unknown` is compatible with
  everything, so it is right downstream of a mistake and invisible everywhere
  else, and five errata are the same sentence about different holes. Errata 41.

  It found two things on the first run. A `loop` had no type at all — the
  comment said "left open rather than guessed", and left open meant
  `let n: Bool = loop { break 1 }` was accepted; a loop now takes the type its
  `break`s agree on, or `()` when none carries a value. And **the reference
  application does not typecheck**, which is the next item.
- **8.3 A module-level `let` is a constant — done.** It used to be a name with
  no type: resolved, never checked, never lowered. `Unknown` is compatible with
  everything so nothing complained, and the first sign was the code generator
  three layers away. Errata 40, and the fourth entry to say that **`Unknown` is
  a silence, not a type**.

  It is Rust's `const` rather than its `static`: lowered wherever it is
  mentioned, so there is no initialization order to get wrong, nothing to
  release at exit, and no shared state for two fibers to reach. `let mut` at
  module level is refused for that last reason.

  It is also *spelled* `const` now — that came later, and only the word
  changed. See `docs/design/keywords.md`.

  This was five of the six things standing between the reference application
  and a binary.

- **8.4 An effect operation is rank-1 — decided and done.**
  `docs/design/polymorphic-operations.md`. An operation is a *field of a
  handler*, and a field has one type, so a `forall` in one cannot be laid out
  by a compiler that monomorphizes — Rust cannot put a generic function in a
  struct field either, and for the same reason.

  The polymorphism moves one level out, into a generic function over the
  effect. `LLMService` keeps `complete` and `embed_raw`; `extract<A: Extract>`
  is an ordinary function that describes, calls, and parses. Not a compromise:
  a mock now fakes one string instead of fabricating whatever the caller asked
  for, the schema-and-parse logic is testable with no model near it, and the
  effect describes what an LLM *is* rather than what one library wanted from
  it. `Extract` gained the `parse` it was always missing, and `embed`'s
  dimension stopped pretending to be the caller's choice.

  **The reference application typechecks** — for real, with the `Unknown` audit
  watching, which is the first time that sentence has been true.

- **8.6 Text, written in Khora.** `String::slice`, `index_of`, `starts_with`,
  `split_once` and `Int::to_string` — all of it in `std::core`, over
  `String::byte` and `Array<U8>`, **with no new intrinsic behind any of it**.
  That was the test as much as the feature: if slicing a string needed the
  compiler's help, so would everything above it.

  It found errata 42 on the way. `Int::to_string` is a written function whose
  owner the code generator recognises, so the `Int::` intrinsic table ate it and
  asked a `String` to be an `i64`. The rule now applies once, before every
  intrinsic: **a method somebody wrote wins over one the backend implements.**
  `attempt` had the same bug in phase 5 and was fixed one call site at a time.

- **8.7 Sockets.** `std::net::socket`, one file per target and only one of
  them ever in a build. The Berkeley calls are `extern` declarations; the
  sixteen bytes of a `sockaddr_in` are laid out in an `Array<U8>` and lent as a
  `Ptr`, because no struct crosses the C ABI and sixteen bytes is not worth a
  runtime function. Windows and Linux; **macOS is deliberately absent**, since
  its `sockaddr_in` puts a length byte where the others put half the family and
  a wrong layout is a `bind` that fails for no visible reason. A build there
  gets "cannot find module", which is worse to read and much better to debug.
- **8.8 `std::net::http`.** A request line, headers skipped, a body, and a
  response with a length on it. The router is a list of routes carrying their
  handlers' rows, `:name` matches one segment, and `Response::json` asks for
  `Show` until there is a JSON module. Enough of the protocol to serve a
  request and no more — chunked transfer, keep-alive and TLS are all absent and
  all deliberate.

  Three compiler gaps came out of writing it, and each is worth more than the
  module: **a rigid row variable now subsumes** (a demand of `'r` against a
  promise of `{ 'r | scope: Scope }` is satisfied rather than a rigidity
  error — every row-polymorphic library function that adds a capability needed
  this); **an impl is found anywhere in the program** during monomorphization,
  not only in the file the generic body lives in, which is what
  `extract<A: Extract>` in `std::ai` needs when the `AnalysisReport` is the
  application's; and **string literals process escapes**, which they never had
  (errata 43).
- **8.5 `==` reaches an `Eq` impl — done.** A scalar compares with one
  instruction and a `String` by its bytes; **anything with a shape decides for
  itself**, in an `Eq` impl written in Khora. One meaning for the operator
  rather than two, and the type gets to say — two `Critical`s with different
  actions are different risks, and only `RiskLevel` knows that.

  The rule is not circular because `impl Eq for Int` is written *in terms of*
  `==` rather than the other way round, which is also why `Float` can have the
  operator and not the trait. `!=` is `==` negated, so a type is never asked to
  be consistent about something it cannot get wrong.

  Missing impls are reported where the comparison is, not where the code
  generator gives up. Ordering — `<`, `>` — still does not reach `Ord`; the
  message says so now instead of claiming `==` is refused too.

### Positions to hold, not gaps to close

Three things an outside review asked for a straight answer on. None is a bug;
each is a claim that would be wrong if made too broadly.

- **`Share` is not yet a safety boundary.** The orphan rule closes the forgery
  that was demonstrated, but `Share`, `Fiber`, `SharedFn` and `Array` are still
  recognised by their bare names rather than by where they were declared. It is
  a real rule with a real hole, and calling it a guarantee needs
  compiler-known identity for the whole set. `docs/design/sharing.md`.
- **The manifest is not a sandbox.** The compile-time gate is total over Khora
  code and has `extern fn` underneath it, which reaches the operating system
  with no row to gate on. Closing that needs package identity, which is 10.2.
  `docs/design/permissions.md` now says so at the top rather than at the end.
- **Fiber-per-connection is thread-per-connection.** Right for tens or
  hundreds of concurrent callers, wrong for tens of thousands. The ceiling is
  the runtime's to raise — `docs/design/fibers.md` promises a program cannot
  tell a thread from a coroutine — and until it does, the position is a bounded
  number of connections rather than an event loop that happens not to be
  written yet.

### Not yet

What still stands between here and a program somebody would use, in the order
they will be missed:

- ~~**A growable list.**~~ Done — `Vector<A>` in `std::core`, contiguous,
  indexed in constant time and appended to in amortised constant time.
  Fiber-local, like `Map`: the `mut` fields that make `push` cheap are exactly
  what the sharing rules refuse to a second fiber, and the shareable sequence
  is a `Shared<List<A>>` or a `Shared<Dict<K, V>>`.

  It is `{ mut items: Array<A>, mut len: Int, mut wanted: Int }`, and the third
  field is the interesting part. `Array::new(length, fill)` demands a value to
  fill with and `Vector::new()` has no `A` in its hands, so there was no way to
  make the backing array at all — the first attempt made the cells `Option<A>`,
  which type checked, passed its tests, read perfectly well, and cost a heap
  allocation per element: a hundred integers were a hundred and three live
  objects. `Array::empty()` closes the real gap — a zero-length array needs
  nothing to fill — the first `push` allocates using the element being pushed,
  and `wanted` is what keeps `with_capacity`'s promise until there is a value
  to keep it with. A thousand integers are two objects.
- ~~**`derive`.**~~ Done. `derive(Eq, Ord, Show, Hash)` above a `type`, with
  `derive` a contextual keyword so a program that already has a field called
  `derive` keeps it. Expanded source-to-source into ordinary impls before the
  checker runs, so nothing downstream — inference, exhaustiveness, Perceus,
  monomorphization, the backend — learns that it exists, and every range in a
  generated body points at the `derive` clause the author wrote.

  A trailing `impl Eq, Ord` clause was tried for a day, on the grounds that
  `derive(..)` is attribute-shaped in a language with no attributes. It was
  reverted: `impl` already names a block, so spending it on a clause is two
  meanings for one word, and the terminology kept leaking — into prose, and
  into four of the compiler's own diagnostics. A word a Rust reader already
  knows, used for exactly one thing, beats a word this language already uses,
  stretched to a second.

  Structural, and refusing rather than guessing: a field whose type lacks the
  trait is named. `Ord` and `Hash` require `Eq` rather than implying it, read
  off the trait's declared supertraits. Field and declaration order decide
  comparison order. A derived `Hash` and a derived `Eq` read the same fields in
  the same order, so equal values hash equal by construction, and both operands
  are reduced at every step because `*` and `+` trap on overflow and
  `Hash for Int` is the identity. Generic types get the bound: `derive(Eq)` on
  `Box<A>` is `impl<A: Eq> Eq for Box<A>`.

  Converting `std`'s hand-written impls is the follow-up.
- ~~**A finer clock.**~~ Done. `Clock` gained `unix_millis` and
  `monotonic_millis`, and `unix_seconds` is now derived from the first rather
  than from `time`, so the two can never straddle a tick. They are separate
  operations on purpose: a wall clock jumps backwards when something corrects
  it, so a difference of two readings is not a duration, and a monotonic clock
  has no epoch anyone outside the process can name, so it is never a
  timestamp. One `millis` would have been the wrong one about half the time.
- **macOS sockets.** `std::net::socket` has Windows and Linux; a `sockaddr_in`
  there puts a length byte where the others put half the family. Processes are
  already covered — `std/process_posix.kh` is named for the family rather than
  for Linux, because `popen` and a wait status are the same on both — but it
  has never been run, and neither has the Linux half.
- ~~**Processes**~~ and ~~**randomness**~~, both done.

  `std::process` is a capability with `status` and `capture`. A non-zero exit
  is not an error — it ran, and its status is the answer — so only
  `checked_output` promotes one. It goes through a shell, which means the
  caller is writing a command line and the usual quoting and injection
  concerns are theirs; an argv-based `spawn` wants `CreateProcess` and
  `posix_spawn`, which take structs the C ABI rule forbids, so it is a runtime
  function and the follow-up.

  `std::random` is a capability too, and for a sharper reason than most:
  `Random::seeded(n)` is what makes a test that draws reproducible instead of
  passing ninety-nine runs in a hundred. Its state is a `Shared<Int>`, so two
  fibers drawing at once are serialized by the same cell every other shared
  value uses. It is splitmix64 and says loudly that it is not cryptographic —
  a token wants a differently-named capability, so that a program says which
  one it needed.
- ~~**`Shared<A>`.**~~ Done — `docs/design/shared.md`. A cell of shareable
  values, not a lock over a mutable record: nothing unshareable goes in or
  comes out, so the escape question Rust answers with lifetimes does not arise.
  `change` cannot fail, which is what makes the lock safe rather than carefully
  handled — a function with no error row has no channel to be cancelled on. A
  stateful test double is a `Shared<Int>` the handler captures, and a shared
  table is a `Shared<Dict<K, V>>`.
- ~~**Evidence parameters for a lambda.**~~ Done —
  `docs/design/capability-passing.md`. A lambda resolves a capability lexically
  if it can and requires it if it cannot, so `nursery(fn () => serve()!)` works
  and eta-expansion no longer changes meaning. What remains is narrow: a lambda
  can require a capability it never mentions, but cannot *mention* one that is
  not in scope, because a bare name is resolved by ordinary lookup.

What is left needs no decision; it is library work over a language that came
out of phase 8 in good shape.

---

## Phase 8.5 — Correctness and credibility

The language can run useful programs. Before optimizing those programs, close
the gaps in claims already made about the surface they use. A safety boundary
that depends on spelling and a compatibility promise nobody has written are
more expensive than an allocation Phase 9 has not removed yet.

This phase is deliberately bounded. It adds one piece of application-facing
library surface that was already requested, makes existing compiler-special
rules identify the declarations they mean, and decides what future releases
promise. It does not add another execution model or widen the language.

- **8.5.1 `derive(ToJson, FromJson)`.** Ordinary traits in `std::json`, with
  source-expanded impls like the existing structural derives. Records encode
  as objects. Variants have one documented stable representation. A decoding
  failure retains the path to the field or payload that disagreed. The backend
  does not learn that JSON derivation exists.
- **8.5.2 Compiler-known declaration identity — guarded, not solved.** The
  hole was worse than the item assumed. A user type called `Array` did not
  merely receive an intrinsic it should not have: it was given the runtime's
  array layout, and dropping one read a garbage element width and aborted the
  process. Errata 46.

  What is done: a name the compiler already means may no longer be given a
  *definition* — `Array`, `Shared`, `Fiber`, `SharedFn`, `Fibers`, and the
  builtin spellings `named_type` answers before it consults the file at all.
  Declaring one opaquely is still allowed, because that names the builtin
  rather than competing with it, and it is what `std::core` and every backend
  test write. The corruption is now a diagnostic that names the declaration and
  says to rename it; `khora-types/tests/identity.rs` pins every case.

  What is not: recognition *by declaration*. A `Type::Adt` carries a bare
  `String`, so two modules that each declare a `Point` are still one type to
  the checker, and an alias still splits one type into two. Both are the same
  defect as the refused case and neither is fixed by refusing a name. The real
  answer is a canonical, module-qualified identity settled where a type name is
  interpreted, which rekeys `adts`, `variants`, `signatures`, `kinds`,
  `declared_here` and every impl head — its own piece of work, not a correction
  to make in passing. `identity.rs` asserts the collision still happens so that
  fixing it fails a test rather than passing silently. Package identity extends
  this in 10.2.
- **8.5.3 Decide D12 — done.** `docs/design/compatibility.md`, and the
  open-question table records it. Pre-1.0 and 1.0 are written separately:
  before 1.0 the promise is procedural — anything may change, every change that
  alters what a valid program does is named, and an edition arrives with the
  first change that needs one. After 1.0 a program that compiles keeps
  compiling and keeps meaning the same thing, within an edition, for the life
  of a major version.

  Two answers are load-bearing beyond the policy itself. **When memory is
  allocated and freed is not observable** — which is what makes Phase 9 a
  legal thing to do, and the reason this moved out of Phase 10. And **there is
  no Khora ABI**, because whole-program monomorphization means a generic
  function does not exist as code until something calls it at a type; a package
  ships source, and the only stable ABI is C's at `extern`.

  Deciding the policy does not declare the implementation stable, and the
  document says what 1.0 still waits on: package identity, declaration
  identity, and an audit of everything `std` exports.
- **8.5.4 Audit claims against the executable repository.** The README,
  positioning, vision and design notes must agree about what is implemented,
  what is only designed, and what is explicitly bounded. In particular: the
  manifest is not called a sandbox before 10.2 enforces the foreign boundary;
  fibers are OS threads today; performance numbers name their workload and
  machine; and a reference application is evidence of composition rather than
  a claim of production completeness. Effect remains a headline comparison in
  the positioning because it is half of the language's thesis, not an
  implementation detail.
- **8.5.5 Establish the correctness baseline Phase 9 must preserve — done.**
  `sh scripts/baseline.sh`: the native suite, `clippy -D warnings`, the corpus
  check and formatter over `std`, `examples` and `bench`, every reference
  application built, and `scripts/http_conformance.sh` — twelve checks against
  a real `curl`, including a POST with a body, a percent-encoded redirect
  target, three requests down one connection, and a 9 KB header that must be a
  413 rather than a crash. `curl` rather than a socket test on purpose: the
  reader that stopped at the first short recv passed its own tests and returned
  400 to a real client.

  The throughput workload is recorded too, in `bench/` — four servers and the
  load generator, with `bench/README.md` carrying the method, the machine and
  the numbers. It is deliberately *not* in the baseline script, because a
  figure measured on a machine that is also running a test suite is not a
  figure.

  Nothing failed that revealed wrong behaviour. Two things found along the way
  are recorded where they belong rather than fixed here: D13 and D14 in the
  open questions above.

Explicitly out of scope here: TLS, macOS and cross-targets, package resolution,
permission enforcement that depends on package identity, the linter, LSP, and
general formatter cleanup. Those remain Phase 10 or ordinary maintenance. The
point of 8.5 is to make the foundation honest before optimizing it, not to make
the whole product complete before Phase 9 can begin.

**Exit — met, with 8.5.2 short of its own wording.** A native round-trip test
covers derived records and variants and a negative test names a nested decode
path. `docs/design/compatibility.md` closes D12 and the open-question table
records it. Public claims match the measured implementation. The full corpus
checks and formats, the test suite is green, the applications run, ordinary
HTTP clients conform, the benchmark is reproducible in `bench/`, and clippy is
clean under `-D warnings`.

The exception is the regression tests for lookalike declarations. They prove
that a lookalike is **refused**, which is not the same as proving it receives no
privilege — a `Type::Adt` is a bare name, so there is nothing downstream that
could tell two declarations apart. The memory corruption is gone and the
remaining defect is pinned by a test that asserts it still happens. See 8.5.2
and errata 46.

---

## Phase 9 — Perceus reuse and FBIP

Reuse analysis, drop specialization, borrowed parameters. Also the escape
analysis D10 promised: an object that provably does not leave its fiber gets
the cheap reference-counting operations back.

Later than it used to be, deliberately. An optimization is measured against
real code and there was none; reuse analysis over a linked list is also worth
much less than over the arrays phase 6 brings.

**Exit:** `map` over a uniquely-owned list performs zero allocations, asserted
by a counting-allocator test.

---

## Phase 10 — Packaging and toolchain

Ordered by value, not by §6's numbering.

- **10.1 Apply D12 at publication.** Phase 8.5 decides the policy; this is where
  package metadata, the resolver and release tooling begin enforcing it.
- **10.2 `khora-pkg`**: `khora.lock` with SHA-256 hashes, content-addressed
  cache, DAG task runner. Also what finally lets the orphan rule be *enforced*,
  which needs cross-package resolution — and the `extern` allow-list from D4,
  which is the rule that turns the permission system from a convention into a
  guarantee and cannot be written until a declaration belongs to a package.
- **10.3 Linter** (needs types): unused capability, dangling pure expression,
  redundant match arm.
- **10.4 LSP** over the salsa database: diagnostics, hover, completion,
  capability inlay hints, rename.
- **10.5 `khora bench`**, and `khora test` grown up: filtering, snapshots with
  `--update-snapshots`, P50/P95/P99.
- **10.6 Cross-targets**: `x86_64-unknown-linux-musl`, `aarch64-apple-darwin`.
- **10.7 WASM build plugins** via wasmtime. Last: largest scope, least critical,
  and it needs D4 settled first.

Note that A7 pulls the *quality* of diagnostics and LSP latency forward into
Phases 2 and 3. What remains here is surface area, not standards.

**Exit:** a package built outside this repository, resolved through
`khora.lock`, and used by the reference application.

---

## When can libraries be written?

The question A6 was really about, and the phases answer it in two steps.

**After 6 and 7 you can write one.** Mutable state, arrays, bytes, floats and
syscalls are the whole of what is missing; everything above them is library
code. Phase 8 *is* the first batch, and writing it is how we find out whether
the language is good for the job while it is still cheap to change.

**After 10 you can hand it to someone.** Packaging is last rather than first on
purpose. D12 asked what may not break; 8.5.3 answers it, and the answer names
package identity as one of the things 1.0 waits for. A minimal package manager could come earlier if
the goal changes to letting other people start.
