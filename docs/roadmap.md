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
| D15 | **When Khora needs a core IR.** Today there is one semantic representation — HIR — and everything else is a side table keyed back to it: inferred types, resolutions, effect evidence, the reference-counting plan, borrows, reuse sites, closure layouts, monomorphization substitutions. That is deliberate and it is why the language could move this fast: no second representation drifts. But the tables accumulate, and at some point "HIR plus nine maps" *is* an IR, assembled implicitly and worse than one designed on purpose. The trigger, from an outside review and worth adopting because it is measurable: **introduce a post-typecheck core IR when code generation must reconstruct semantics from three or more independently-computed side tables to lower one ordinary expression.** Not before — a second IR now would be premature — and not by feel, because inertia argues for "not yet" forever. | phase 12 or later; watch it during 10 and 11 |

D13 and D14, the two before it, were both language-surface holes that parsed
and type-checked and then failed somewhere further down. Both are closed
below.

**Closed:**

- **D13** (what `[a, b, c]` denotes) is a **`List`** — `std::core`'s cons list,
  and the literal is desugared into a `List::Cons` chain during HIR lowering.

  `List` is the one of the three sequence types that behaves like a value: it
  is immutable, a `match` can take it apart, and it may cross into a fiber.
  `Array` and `Vector` are buffers a program writes into and neither is
  `Share`, so a literal quietly producing one would be three surprises from one
  pair of brackets. Phase 9.2 also removed the allocation cost of the case a
  cons list is usually criticised for: walking one and rebuilding it allocates
  nothing when it is uniquely held.

  **Desugared rather than typed as itself**, which is why nothing downstream
  needed a case for it: by the time the checker sees one it is constructor
  calls, so inference, monomorphization, reference counting and reuse all work
  on it without being told. `Expr::List` is gone from the HIR. It is also
  exactly what `derive(ToJson)` was already emitting by hand — that expansion
  tried to be the literal's first user, which is how the question was found.

  **`List` must be in scope**, as `Step` must be for a `for` loop, and for the
  same reason: a name the compiler knows and the program cannot see is what
  errata 46 was about. The diagnostic is reported at the brackets and names the
  import, rather than being carried onward as an unsupported resolution and
  arriving as "the type of this expression was never worked out" — the message
  D13 existed to be rid of.

- **D14** (whether `match` tests by equality) is **yes**: a `String` or float
  literal in a pattern compiles to an equality test. It had parsed and
  type-checked for some time and then failed in the backend — accepted through
  two phases and refused in the third, which was the one available answer that
  was clearly wrong. `khora_str_eq` already existed and `==` already compiled.

  Two details are decided with it. A string test **borrows** both sides: the
  scrutinee belongs to the `match` and the literal is a static, so releasing
  either would be wrong in one direction and pointless in the other. And a
  float test is *ordered* equality, so a `NaN` scrutinee matches no literal
  including a `NaN` one — the same answer `==` gives, because a pattern
  disagreeing with the operator would be worse than either answer alone.

  Literal patterns still do not make a `match` exhaustive, which the checker
  already knew before the backend could compile one.

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

  1.0 is blocked on package identity (10.2) and an audit of everything `std`
  exports — each of which the document names rather than assumes. Declaration
  identity was the third and landed in 8.5.2.

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

TLS is bound and done: `rustls` in the runtime, both ends,
`docs/design/ecosystem.md`'s rule applied. The other bindings A6 names —
crypto, compression, numeric kernels — are
consumers of phase 7 and are *not* on this critical path.

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
  all deliberate. *Later:* keep-alive and TLS both landed; chunked transfer and
  multipart have not.

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
- **8.5.2 Compiler-known declaration identity — done, and it was two things.**
  The hole was worse than the item assumed. A user type called `Array` did not
  merely receive an intrinsic it should not have: it was given the runtime's
  array layout, and dropping one read a garbage element width and aborted the
  process. Errata 46.

  **A type carries its declaring module now.** Resolved at `named_type`,
  compared by unification, carried into the mangled symbol, and asked for by
  every lookup a `Type` drives. Two modules may each declare a `Point`; an
  alias is the type it renames. Three further places had been storing the
  wrong module or deduplicating by the right name and the wrong key —
  `Resolution::Variant`, the backend's `merged_types`, and the two field
  lookups code generation keeps of its own.

  **The guard stays.** A name the compiler already means still may not be given
  a definition, because the *backend* recognises `Array`, `Shared`, `Fiber` and
  the rest by bare name. That is a smaller, more contained version of the same
  problem, and it goes when those declarations get an identity the code
  generator can ask about rather than when the type system does. Package
  identity extends this in 10.2.

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
  were recorded as open questions rather than fixed here: D13 and D14, both
  since closed — see the design questions above.

Explicitly out of scope here: macOS and cross-targets, package resolution,
permission enforcement that depends on package identity, the linter, LSP, and
general formatter cleanup. Those remain Phase 10 or ordinary maintenance. The
point of 8.5 is to make the foundation honest before optimizing it, not to make
the whole product complete before Phase 9 can begin.

**Exit — met.** A native round-trip test
covers derived records and variants and a negative test names a nested decode
path. `docs/design/compatibility.md` closes D12 and the open-question table
records it. Public claims match the measured implementation. The full corpus
checks and formats, the test suite is green, the applications run, ordinary
HTTP clients conform, the benchmark is reproducible in `bench/`, and clippy is
clean under `-D warnings`.

8.5.2 met its own wording in the end, though not in one go: the guard landed
first and identity followed. A `Type::Adt` knows the declaration it came from,
and `khora-types/tests/identity.rs` pins both halves — two modules declaring one
name, an alias unifying with what it renames, and the lookalike definitions that
are still refused because the backend has not caught up.

---

## Phase 9 — Perceus reuse and FBIP — done

Reuse analysis, drop specialization, borrowed parameters, and as much of the
escape analysis D10 promised as can be had without a flow analysis.

Later than it used to be, deliberately. An optimization is measured against
real code and there was none; reuse analysis over a linked list is also worth
much less than over the arrays phase 6 brings.

Together, on an 80-byte HTTP request: **2,440ns at the start of the phase, 1,555
now**, and 14,560 to 7,345 on a browser's request.

`bench/service` measured 507k req/s before the phase and 548k during it, and
then 538k in the final sitting where all four servers were run back to back —
which is the figure `bench/README.md` and the README carry, because it is the
one taken alongside its controls. All three are inside the eight per cent that
benchmark varies by. The honest reading is that nothing regressed, not that
anything was gained: most of what a request parser does is build strings and
hash them.

Two things the phase got wrong on paper and right in the measurement, both
recorded where they happened: 9.3 was called "the cheapest of the four and the
least interesting" and was the second largest win, and 9.4's remaining half
cannot be had from the `Share` marker however much it looks like it should.

`docs/design/reuse.md` has the plan, the order, and what is left.

- **9.0 A field-less constructor is a static — done.** `Option::None` was a
  heap allocation, and so was `List::Nil`, and so was every case of an enum
  with no payloads: twenty-four bytes and a pair of atomic reference-count
  operations for a value described entirely by its tag. One private global per
  `(type, case)` now, with an immortal count, the same way a string literal
  works. Measured: a hundred failed `Map::get` calls went from 107 allocations
  to 5, an HTTP request parse from 61 to 55, a JSON round trip from 142 to 128.
  Server throughput did not move, which is worth saying plainly — the parser's
  cost is strings and hashing.
- **9.1 Ownership at the last use — done for bodies that cannot unwind.** A
  backward liveness pass: a read the binding does not outlive takes its
  reference instead of copying it, and a branch that takes on one path is
  balanced by a release at the head of the arms that do not. That second half is
  what makes it worth anything — "all reads unconditional", which is what this
  was before, excludes nearly every read in real code. 314 reference-count
  operations in an HTTP parse down to 278, and 1,955ns to 1,855ns.

  Three rules earn their keep, each of them found by a double free rather than
  by thinking:

  - A `match` arm's bindings are projections of the scrutinee and own nothing,
    so their reads always copy. Taking one freed a list node its own recursion
    was standing on.
  - An arm release goes at the arm's *head*, so only an arm that never mentions
    the binding may be given one. An arm that borrows it would read freed
    memory; that branch consumes nothing and its block releases as before.
  - A binding an arm introduces itself is not the branch's to settle — it does
    not exist on the other paths, where the release would read a slot nothing
    ever wrote.

  **Bodies that can unwind are in too**, which they were not at first. The
  cleanup stack is positional and cannot express a live set that depends on how
  far execution got — so the fix is to stop asking it. The block keeps its
  release and a take clears the slot: before the take the binding is the
  block's and a `raise` releases it, after it the slot is null and releasing
  null is a no-op. The store is emitted only where something can unwind.

  56 of the 280 reference-count operations in an HTTP parse, and **no
  measurable change to the clock** — after 9.3 each operation is a handful of
  instructions, so a fifth of them is near 1% of a 1,570ns parse and beneath
  what the benchmark resolves.

  **It found a bug older than the pass, and that was worth more.** Clearing the
  slot turns a wrong answer into a null dereference rather than a stale pointer
  that usually still works, and the first thing it caught was that **a
  capability is read where nothing mentions it**: code generation hands the
  evidence to any callee that wants the same one, with no expression for the
  read. The link shortener's `health` mentions `clock` once and forwards it
  twice, so the mention was called a last use. Reachable before this too, and
  survivable then — the binding still pointed at a live handler, so the count
  was one short rather than the pointer wrong. `docs/design/reuse.md` §1.
- **9.4a Borrowed parameters — done, and it was the one that paid.** The calls
  that already borrowed and said otherwise: `Region::defer`, `Shared::get` and
  friends, `String::byte`, `Array::get`. Each took an owned reference and
  dropped it, two atomic operations to pass something the callee only reads.
  **A borrow applies inside a loop**, which is what separates it from a last
  use: 14,560ns to 10,210ns on a browser's request, and 310ns to 90ns on
  lowercasing a header.

  It also fixed 9.1's hedge. Moving a `Region` to its last use ran its
  finalizers early, which read as "some types have an observable release" and
  produced a restriction to `String`; the real cause was that `defer` borrows
  and the plan said it consumed. The restriction is gone.
- **9.2 Reuse tokens — done, and the phase's exit criterion with them.** A
  ten-element `map` over a list nothing else holds now allocates nothing.

  9.1 removed two of the three references holding a matched cell at the arm's
  constructor. The third was the `match`'s own — `lower_match` held the
  scrutinee in a scope cleanup across the arms — and moving it could not be done
  by moving a line, because an arm's bindings did not own what they pointed at.
  `bind_pattern` stores the loaded field into the slot and reads of it copy, so
  releasing the scrutinee at the arm's head would have freed the payload out
  from under every binding. So the order became the ordinary owning one: copy
  the bindings the body reads at the arm's head, release the scrutinee there,
  and let their reads be settled by 9.1 like any others.

  Then `khora_drop_reuse` at the arm's head, `khora_alloc_reuse` at its
  constructor. A token is memory with no owner, so the rule for taking one is
  syntactic and narrow: the arm's body must **be** the constructor, and nothing
  inside it may leave the frame early. The shapes are compared at run time out
  of the header, so an arm need not prove what it is about to build.

  Beyond the exit criterion: an HTTP parse from 54 allocations to 50 and 1,855ns
  to 1,770ns, `bench/service` from 507k to 548k req/s. Reuse pays where a
  program walks a structure and rebuilds it; a request parser mostly builds
  strings and hashes them, and mostly does not.
- **9.3 Drop specialization — done, and it was not the cheapest.** Written
  expecting nothing; the second largest win in the phase.

  The entry here used to call it "the cheapest of the four and the least
  interesting", reasoning about the work the runtime does. What cost was the
  **call**. An HTTP parse performs 280 reference-count operations against 50
  allocations, and 230 of those calls did nothing but add or subtract one from a
  word. The refcount is the first field of the header, so a `dup` is now a null
  test and one relaxed atomic add emitted inline, and a `drop` is a null test, a
  release atomic subtract and a branch that is not taken — only the last
  reference calls into the runtime. 1,770ns to 1,670ns on an HTTP parse, 8,935
  to 8,360 on a browser's.

  The first attempt to *measure* it before writing it went wrong in a way worth
  recording: a throwaway runtime with `dup` and `drop` returning immediately ran
  **slower**, because nothing is ever freed and the working set stops fitting in
  cache. A no-op runtime measures a program with a leak, not what counting
  costs.
- **9.4 Non-atomic counting — done where the compiler can prove one thread.**
  Measured first: with counts kept correct and the atomics removed, an HTTP
  parse ran 1,670ns to 1,555ns and a browser's 8,360 to 7,345. Seven and twelve
  per cent, a ceiling rather than an estimate.

  `Fiber::spawn` is the only way a Khora program starts a thread, so a program
  that never mentions it has one thread for its life and can count references
  with plain arithmetic. Whole-program monomorphization makes that answerable —
  the compiler already holds every body it will emit — and the scan is
  conservative in both available directions: a mention rather than a call, over
  the whole expression arena rather than a walk from the root.

  **Checked at run time as well.** A data race in a refcount is memory
  corruption arriving far from its cause and no test finds it reliably, so the
  generated `main` tells the runtime what was decided and `khora_fiber_spawn`
  aborts if a thread starts anyway. Forcing the flag on turns
  `a_program_that_spawns_counts_references_atomically` into that abort, which is
  how the guard was confirmed to guard something.

  **What is left is per-allocation-site, and no type-level rule supplies it.**
  The `Share` marker cannot: `String` is shareable, and so is every ordinary
  immutable container, because sharing an immutable value is the thing that
  ought to be allowed. Nor can the sharper version — the types a spawn's closure
  actually captures — because `bench/service`'s spawned closure captures the
  router, a router holds its route paths, and one `String` in one long-lived
  structure poisons the whole type for the program.

  **And its ceiling cannot be measured without building it.** The 7% and 12%
  above are the win already taken, from a benchmark that never spawns. Forcing
  a spawning program non-atomic to price the rest gave 82 requests a second and
  then zero — `bench/service` corrupts itself in a few hundred requests. That
  is not a measurement; it is a demonstration that this is the one optimization
  in the phase with no margin for being approximately right. What can be said:
  atomics are ~7% of a parse, a parse is a fraction of what a server does, so a
  few per cent of throughput — inside the eight the benchmark varies by.

  So neither remaining shape is started, and neither should be started for the
  number. `docs/design/reuse.md` §4 has both written up against what is known.

  This entry used to be "priced at 12%" and to claim borrowed parameters as part
  of itself. Those landed as 9.4a during the throughput work and are recorded
  above.

**Exit: met.** `map` over a uniquely-owned list performs zero allocations,
asserted by `a_uniquely_owned_walk_allocates_nothing` in
`crates/khora-codegen-llvm/tests/reuse.rs` — written before the work, and no
longer ignored.

One piece is deliberately not done, and it is not part of the exit criterion:
the per-object half of 9.4, which wants an escape analysis or biased reference
counting. It is written up against its measurement.

---

## Phase 9.5 — Surface completeness

Everything here is something a stranger hits in the first afternoon. None of it
is deep, all of it is the difference between "interesting" and "usable", and
none of it was on the roadmap because each piece was individually small enough
to keep not doing.

The order is by damage prevented, and the shape of the damage matters: three of
the four are things that **parse and type-check and then fail further down**,
which is the pattern D14 was closed for and which reads to a newcomer as a
compiler that does not know what it supports.

- **9.5.1 Tuples and irrefutable `let` — done.** `(1, 2)` parsed, type-checked,
  survived exhaustiveness checking, and then failed in the backend with
  *"phase 2 handles `Int`, `Bool`, `String`, `()` and ADTs"* — an internal phase
  number in a user-facing error. The front end had been complete for a long
  time; only the layout was missing.

  **A tuple is an anonymous record**: one heap object, positional fields,
  counted and released like every other aggregate. `instantiated_variants`
  answers for one out of its type, and that is the whole of it — the reference
  counting plan, the drop glue, pattern binding and the reuse analysis all ask
  that question already and none of them learned that tuples exist. Boxed
  rather than in registers, consistently with every other aggregate, and
  `docs/design/compatibility.md` keeps unboxing legal later.

  `let (a, b) = pair` works too, for any pattern that **cannot fail** — a tuple,
  or a constructor whose type has one case, recursively. One that can fail still
  needs a `match`, and now says which of the two it is instead of refusing all
  destructuring.

  Two bugs on the way, both found by the live-object count. A tuple took a null
  `drop_fields`, because `drop_glue` matched on `Type::Adt` and gave everything
  else nothing — so tuples were freed and their boxed elements were not. And
  pattern binding read field types from the *declaration*, so a tuple inside a
  generic container arrived as the rigid parameter `A`; both callers know the
  matched value's type now, which also retires the workaround `bind_pattern` was
  carrying for the same reason.

- **9.5.2 macOS, and a CI matrix — green.** `std/net` had
  `socket_windows.kh` and `socket_linux.kh` and nothing for macOS, so a Mac got
  "cannot find module `std::net::socket`" — and there was no CI at all, so
  nothing but one Windows desktop was known to work.

  `socket_macos.kh` is a copy of the Linux file with the two BSD differences: a
  `sockaddr_in` begins with a length byte where Linux puts the low half of the
  family, and `SOL_SOCKET`/`SO_RCVTIMEO` are `0xffff`/`0x1006` rather than 1 and
  20. File-suffix selection picks whole files, so two lines of difference still
  means a file, and a fix to one belongs in both.

  **A Mac has now run it.** The full suite and the whole baseline pass on
  `macos-latest`, including a Khora HTTP server binding a real socket and
  answering twelve `curl` checks. That is the part no amount of type checking
  could establish — a `sockaddr_in` with the family in the wrong byte
  type-checks perfectly — so the numbers are now known to be right rather than
  believed to be.

  It took three rounds to get there, and none of the three was the macOS file.
  The backend job had **never once passed on any platform**: `setup-llvm.sh` let
  the installers' stdout escape into its own answer, so `$GITHUB_ENV` got a
  multi-line value and refused it. Behind that were two more, both real and both
  invisible from Windows — `RelocMode::Default` produces objects Linux will not
  link into a PIE, and the macOS entry in `SYSTEM_LIBS` was a placeholder that
  had never been run, missing the `CoreFoundation` and `Security` frameworks
  `rustls` reaches the trust store through.

  Two tests now keep all of this findable from a laptop.
  `khora-types/tests/portability.rs` type-checks `std` for every target from any
  host, and `khora-codegen-llvm/tests/portability.rs` generates and *verifies* a
  module for every target — added after a symbol collision that existed only in
  the combination of modules a POSIX build compiles together, which no Windows
  developer could reproduce. See phase 10.0's neighbours in the log.
- **9.5.3 An install story that is not a specific tarball — done.**
  `scripts/setup-llvm.sh`, which CI runs too, so a failure there is a failure of
  the documented install rather than of a CI-only path.

  The discovery that made it easy: **`brew install llvm@22` is pinned at exactly
  22.1.8** and bottled for macOS and Linux, and apt.llvm.org has a 22 channel.
  The tarball is only needed on Windows — which is fortunate, because the LLVM
  22.1.8 release publishes binaries for Windows and `armv7a-linux` and nothing
  else. There was never a Linux or macOS tarball to point at.
- **9.5.4 String interpolation — done.** `"hello ${name}"` compiled and printed
  `hello ${name}`. That is what every language without interpolation does, and
  it is still a trap for anyone arriving from JavaScript, Kotlin or Swift,
  because it is wrong silently rather than loudly.

  `"a ${e} b"` is now `"a " + e + " b"`. Joining with the operator somebody
  would have written is the whole of the feature: `+` on strings already
  requires both sides to be `String` and already says *"string concatenation:
  expected `String`, found `Int`"*, which is the right thing to say about
  `"${count}"`. Numbers go through `Int::to_string` as they always did.

  **Nothing was added to the grammar.** A string literal is still one token; the
  holes are found in its text during HIR lowering and each is parsed as a little
  source file of its own. The cost of that is `Ctx::range_shift`, which moves a
  fragment's ranges back to where the text is — without it every diagnostic
  about an interpolated expression points at the top of the file. With it, the
  caret lands on the name.

  The lexer learned exactly one thing: a `"` inside a hole opens a nested string
  rather than ending the literal. Without it `"${f("x")}"` ends at the third
  quote and the rest of the line lexes as code, arriving as `expected )` — the
  same fail-further-down shape this phase exists to remove. `\$` is a literal
  dollar, so a template for another tool still fits in a Khora string, and `${}`
  is refused rather than quietly becoming `""`.

  Both reference applications use it now, which is the only real test of whether
  it reads better.

**Exit:** someone with a Mac and no prior knowledge can install the compiler and
write a hundred lines without hitting a wall that isn't their own mistake.

Three of the four are done. What is left is a green CI run on a real Mac, which
needs the workflow to have run once.

---

## Phase 9.6 — Internal boundaries — done

**The crate architecture is clean; the architecture inside four of the crates
is not.** From an outside review, confirmed by measurement:

| file | size |
| --- | --- |
| `khora-codegen-llvm/src/lower.rs` | 252 KB |
| `khora-types/src/lib.rs` | 205 KB |
| `khora-rt/src/lib.rs` | 107 KB |
| `khora-codegen-llvm/src/backend.rs` | 102 KB |
| `khora-hir/src/body.rs` | 84 KB |

Large files are not the problem in themselves. Two things make this worth a
phase of its own:

**Phase 10 is when navigation starts to matter.** It adds a language server and
invites contributors, and both are people trying to find where function-call
inference happens without reading 205 KB first.

**Phase 11 rewrites `khora-rt`'s deepest invariant.** Splitting the runtime
*during* the scheduler work means refactoring a giant module at the same moment
its concurrency model changes, which is the one ordering guaranteed to make
both harder.

**The boundaries have already begun to erode**, which is the argument that this
is not premature. `khora-rt/src/lib.rs` carries `// --- section ---` banners and
three of them are already wrong: `khora_str_find` and `khora_str_eq` live under
"Allocation and reference counting", `khora_utf8_valid` and `khora_sum_bytes`
under "Allocation accounting", and `khora_print_float` and the process arguments
under "arrays". A banner nobody can trust is worse than no banner.

Split by **compiler responsibility**, not by size, and as **move-only commits**
— no behaviour change, no API change, verified by the full suite and
`scripts/baseline.sh` at each step. The cost worth naming: a pure move breaks
`git blame` for a codebase whose comments and bug stories are its main asset, so
a move commit must contain nothing but moves.

- **9.6.1 `khora-rt`.** First, for the Phase 11 reason. Its banners already name
  the seams and `tls.rs` proves the pattern works in this crate. The `no_mangle`
  C ABI must come out byte-identical.
- **9.6.2 `khora-codegen-llvm`.** `lower.rs` and `backend.rs`, 354 KB between
  them. The review missed this one; it is the largest file in the repository and
  Phase 11 touches its cleanup stack too.
- **9.6.3 `khora-types`.** One `Checker` with about fifty methods that already
  cluster: calls, effects and rows, expression forms, patterns, traits and
  bounds, sharing, diagnostics. Inherent impls may be split across modules
  within a crate and a child module can see the parent's private fields, so this
  is a pure move.
- **9.6.4 `khora-hir/body.rs`**, and the tense.

  Module documentation ages every time the roadmap advances. `khora-perceus`
  opens with "Roadmap phase 2.3" and a section called "What phase 9 has to
  change here"; `khora-hir` calls itself "the first half of roadmap phase 2.1".
  Phase 9 is done and 2.1 is two years of work ago. The explanations are good
  and stay; the tense goes, so a source file describes what a pass *does*
  instead of where it sat in a plan.

**Exit — met.** 745 KB across five files became 59 modules. The largest source
file in the repository is now `khora-types/src/unify.rs` at 44 KB, which was
already one thing with one name. The suite and the baseline were clean at every
step.

| | before | after, largest |
| --- | --- | --- |
| `khora-codegen-llvm` lowering | 252 KB, one file | 14 modules, 38 KB |
| `khora-types` checking | 205 KB, one file | 11 modules, 40 KB |
| `khora-rt` | 107 KB, one file | 16 modules, 16 KB |
| `khora-codegen-llvm` backend | 102 KB, one file | 9 modules, 19 KB |
| `khora-hir` body lowering | 84 KB, one file | 6 modules, 24 KB |
| `khora-perceus` | 53 KB, one file | 5 modules, 16 KB |

**Nothing changed behaviour.** Each split was verified three ways: the function
definitions before and after, the doc-comment lines before and after, and the
baseline. The doc-line check earned its keep — a patch script failed halfway
through reuniting a comment with its function, deleting it from one file
without adding it to the other, and nothing else would have noticed.

Two visibility widenings were forced and neither leaves a crate: methods that
cross a module boundary are `pub(super)`, and `khora-types`'s `Checker` fields
are `pub(crate)` because the query layer builds one and reads what it inferred.
`khora-rt`'s modules are private and re-exported wholesale, because that crate's
API is a C ABI reached by symbol and `khora_rt::heap::khora_alloc` would be a
second name for one function.

**What the exercise found, beyond navigability.** Section banners had begun to
disagree with their contents in three of the five files, always in the same
direction: a section grows, absorbs a neighbour, and nobody renames it. The
worst was `backend.rs`, which had an empty "Drop glue" heading immediately
followed by "Closures" with the glue filed under the latter. A banner is a
promise with no compiler behind it, and a module is the same promise the
compiler checks.

---

## Phase 10 — Packaging and toolchain

Ordered by value, not by §6's numbering.

- **10.0 Prove the incrementality the rest of the phase assumes — done, and it
  was false.** `khora-hir/tests/incremental.rs`. The promise as `testing.md`
  worded it turned out to be trivially true; the claim one layer out was not.
  `Item` carries a `TextRange`, so a character typed into the first function of
  a file shifted every declaration below it, `ItemMap` compared unequal, and
  salsa correctly rebuilt `module_graph` and the `file_scope` of every importer.
  `file_scope`'s own doc comment had asserted the opposite since the day it was
  written.

  The fix is a barrier rather than a rewrite: `module_api` is a span-free
  projection of `item_map` that every *cross-file* query reads instead. It
  re-executes on each edit, compares equal, and the invalidation stops there.
  `item_map` keeps its spans for diagnostics and for go-to-definition. `Variant`
  lost its `range`, which nothing had ever read.

  A one-character body edit now re-runs the edited file's own item collection,
  scope and bodies, and nothing else. The whole of `docs/design/testing.md`
  §"What writing the first one found" is about why this was the right test to
  write first.

  The original entry, for the record:

- ~~**10.0 Prove the incrementality the rest of the phase assumes.** One test,
  first, because it is a prerequisite rather than a part: *editing a function
  body does not invalidate item collection for unrelated modules.* Everything
  asserted today is at the parse layer — `khora-db/tests/incremental.rs` proves
  an edit to file B does not reparse file A, and that reverting text is
  backdated. Item collection is a different query and nothing measures it. It
  is the claim 10.4 rests on, `khora-db` already logs query execution so the
  machinery exists, and if it is false that changes the language server's design
  rather than adding a bug to fix. From `docs/design/testing.md`.~~
- **10.1 Apply D12 at publication — the enforceable half, done.**
  `khora-manifest`'s `semver` module. `compatibility.md` is written entirely in
  terms of major, minor and patch, and none of it meant anything against a
  version string nobody parsed: `"1.2"`, `"v1.2.3"` and `"latest"` were all
  accepted, and the first place any of them would have been noticed is a
  resolver comparing two and giving an answer nobody could explain. A leading
  zero is refused too, so `01.0.0` and `1.0.0` cannot become two spellings of
  one version in a lockfile.

  **The rest genuinely waits on the registry.** The substance of "apply D12 at
  publication" is comparing a package's public surface against its previous
  published version and refusing a minor release that added a case to a sum
  type, a field to a record, or a requirement to a `with` row. There is nothing
  to compare against until something has been published, so this is blocked on
  the registry rather than on effort.
- **10.2 `khora-pkg` — mostly done.** `khora.lock`, a content-addressed store,
  transitive resolution and the task DAG all exist, and the exit criterion
  below is met. `crates/khora-pkg`, and `khora-codegen-llvm/tests/packages.rs`
  is the end-to-end proof.

  **A `git` source was added ahead of the registry.** The manifest modelled
  `path` and `version`, and neither exercises what a package manager is for: a
  path is not fetched, so nothing is hashed, pinned or cached, and `version`
  needs a registry that does not exist. `{ git = "...", rev = "..." }` is the
  smallest source that is really a source, and a git dependency with no
  revision is refused rather than quietly taking the default branch.

  A git package is pinned twice, to a full commit id and to the SHA-256 of the
  tree it produced. The commit id is what a server said it was serving; the
  hash is what arrived, and them disagreeing is the case a lockfile exists for.

  There is **no version solver**, because every source names one exact thing.
  Two packages wanting different revisions of a third is therefore an error
  naming both askers. That is where a solver goes when a registry arrives.

  Still open here: the orphan rule is not yet *enforced* across packages, and
  the registry and `khora publish` do not exist.

  Both things the outside review left at this bullet are done:

  - **The borrow table's rule is enforced rather than remembered.**
    `borrowed_arguments()`'s doc comment always said only bodyless declarations
    may appear, because a Khora body owns its parameters and releases them.
    Nothing checked it, and packages made it uncheckable by hand: the key is a
    bare type name, and anybody may declare a `Shared` with a `get`. The
    planner now takes `Defined` — what the program implements in Khora — and
    consults the table only for a pair nothing implements.

    Worth knowing before touching it: restricting the table to types `std`
    declares is the obvious fix and is **wrong**. A self-contained program may
    declare its own `Region` and let the runtime implement `defer`, which most
    of the backend's tests do, and refusing to lend to them reorders
    finalizers. `docs/design/reuse.md` §1.
  - **The `extern` hole is closed, not merely tested.** `permissions.md` had
    carried it as "cannot be implemented yet, because packages do not exist",
    which stopped being true earlier in this same phase — going to write the
    test is what noticed. `[permissions] extern = [..]` now decides which
    packages may declare a foreign function; `std` always may, absent grants
    everything, and `khora-cli/tests/permissions.rs` holds both the refusal and
    the two cases saying the hole is no wider than documented.

- **10.3 Linter — done, two of three.** `crates/khora-lint`, wired to the
  `[lints]` table through `khora check`. The third the entry asked for already
  existed: a `match` arm that cannot be reached is a *type error*, out of the
  same usefulness algorithm that decides exhaustiveness, and making it a lint
  as well would give one mistake two voices.

  `std`, all three examples and `bench/service` are clean.

  **Both are narrow on purpose.** A lint people learn to ignore is worse than
  no lint, so where a judgement was available each takes the quiet side.
  `dangling-expression` reports only an expression that *cannot* do anything —
  no call, no assignment, nothing that could raise — which leaves out the
  interesting case of a call whose result is discarded, because deciding that
  needs a purity analysis rather than a table. `unused-capability` stays silent
  whenever the body contains any call, because a call may be forwarding the
  capability without naming it.

  Sharpening the second is a small, well-defined piece of work: `BodyTypes`
  needs a per-call-site record of the labels the callee required. The checker
  computes exactly that in `check/effects.rs` to do its row subtraction and
  then drops it; `lambda_captures` is the same fact published for a different
  consumer and is the shape to copy. With it, "used" becomes "read, or required
  by something this body calls" and the call-free restriction goes away.
- **10.4 LSP — the half that answers, done.** `crates/khora-lsp`, and
  `khora lsp` starts it. Diagnostics (parse, type, and lints at their manifest
  level) and hover. Completion, capability inlay hints and rename are not
  here: each needs an index the server does not build, and a completion that
  half answers is worse than none.

  One thread, one message at a time. Nothing is slow enough yet to want
  cancellation, and a single thread means the database has one owner and no
  locking — worth keeping until something measured says otherwise.

  **10.0 is why this works at all**, and it is worth restating where it will be
  read: an editor asks the compiler a question after every keystroke, so a
  keystroke that invalidates the world is a language server that recompiles the
  world. One did. `khora-hir/tests/incremental.rs` holds it closed.

  Two things worth knowing before extending it. `serve` is generic over its
  streams, so `tests/session.rs` drives a whole session through two buffers
  with no editor and no subprocess — that is why there are seventeen session
  tests rather than none. And positions are UTF-16 code units by default while
  the compiler counts bytes: they agree exactly until the first accented
  letter, which is why `position.rs` has its own tests and why the negotiated
  encoding is echoed back in the `initialize` reply.
- **10.5 `khora bench` and filtering — done. Snapshots are not.**

  **`bench` had been in the grammar since phase 1 and nothing collected it.** A
  `bench` block parsed, type-checked, and then compiled to nothing and ran
  never — silently, which is the worst way for a promised feature not to work.
  Four places had to learn about it, and the last one is the reason it stayed
  hidden: without a synthesised signature in `type_map`, no instance is
  registered, so `emit_function` declines to declare the body, so the entry
  point's registration loop finds no function to point at. The build succeeds
  and prints `no benchmarks`.

  `khora bench` reports **P50, P95, P99 and a sample count, and no mean**. A
  mean over a run containing one scheduler preemption describes none of the
  iterations, and the tail is usually the interesting half. Percentiles are
  nearest-rank, so every number reported is a measurement that happened rather
  than an interpolation between two that did.

  Two limits worth knowing. There is **no `black_box`**: a bench whose body
  computes something nobody reads may be optimised away and will then report a
  few nanoseconds very confidently. Adding one means a compiler intrinsic, not
  a library function. And benches run **one at a time**, unlike tests —
  overlapping tests find tests that lie, and overlapping benches contend for
  cores and measure that.

  `--filter` on both, matching a name by substring, read from `argv` rather
  than from the environment so the compiled harness behaves the same when
  somebody runs it by hand. A filter that matches nothing says how many it
  skipped, because otherwise it reads identically to a file with no tests and
  one of those is a typo.

  **Snapshots with `--update-snapshots` are not written.** They need a file
  format, a comparison that reports a readable diff, and an assertion API in
  `std` — the last of which is a language-surface decision rather than a
  toolchain one, since `assert_snapshot` has to name the file it is comparing
  against.
- **10.5.5 Toolchain versions — done.** Not previously on this list, and it
  should have been: everything above assumes one Khora on the machine, and the
  moment a project has a lockfile it also wants to say which compiler produced
  it.

  ```toml
  [toolchain]
  version = "0.1.0"
  ```

  **In `khora.toml` rather than a file of its own**, which is the opposite of
  what Rust and Node do. Their argument — a compiler version is not a property
  of the package — is good, and it loses to a simpler one: a project with two
  files describing how to build it has two that must both be found and both be
  committed, and only one anybody remembers.

  Toolchains live in `$KHORA_HOME/toolchains/<version>/bin/`, the same root the
  package store uses, so a machine has one Khora directory rather than two.
  `khora` hands over to the pinned version **before argument parsing**, so a
  project pinning a version with flags this build has never heard of still
  works — which is the whole point of a pin.

  **A missing pinned version stops the build.** Falling back is the exact
  failure the feature exists to prevent: a build that quietly used a different
  compiler looks like it worked.

  Two things worth knowing before extending it. `khora toolchain ...` never
  hands over, because otherwise standing in a project whose pin is missing
  leaves you unable to run the command that installs it — the pin becomes a
  trap. And there is **no `install`**, because there is nothing to download
  from; `link` registers a build already on disk, and copies rather than
  symlinks so a `cargo clean` cannot leave a toolchain pointing at nothing.

- **10.5.6 An MCP server, so an agent can ask the compiler — done.**
  `crates/khora-mcp`, started by `khora mcp`. Also not previously on this list.

  **The premise is that no model has seen Khora.** An agent asked to write some
  produces something plausible — it borrows enough syntax from Rust and enough
  ideas from Effect that a good guess is easy — and is wrong in ways it cannot
  detect by reading. The failures with no analogue elsewhere are the expensive
  ones: a capability that must appear in a `with` row, an error that must
  appear in `raises`, `Share` on anything crossing into a fiber.

  So the server is not documentation. It is `khora_check`: the agent writes
  Khora, the real compiler answers with real diagnostics against the real
  `std`, and the agent learns from the answer. That loop needs no training data
  and cannot go stale, because the thing answering is the thing that will
  compile the code. Four other tools — searching `std`, the grammar, the design
  notes, the formatter — exist so the first guess is worth checking.

  Version-aware for free: `khora mcp` goes through the toolchain shim, so a
  pinned project gets that compiler and that `std` answering.

  Two things to know. The transport is **newline-delimited JSON**, which is MCP
  stdio and is *not* what `khora-lsp` uses — LSP frames with `Content-Length`,
  and confusing the two gives a client that hangs on the first message. And a
  tool failure comes back as content with `isError` rather than as a JSON-RPC
  error, because an agent can read the first and usually cannot see the second.

- **10.6 Cross-targets**: `x86_64-unknown-linux-musl`, `aarch64-apple-darwin`.
- **10.7 WASM build plugins** via wasmtime. Last: largest scope, least critical,
  and it needs D4 settled first.

Note that A7 pulls the *quality* of diagnostics and LSP latency forward into
Phases 2 and 3. What remains here is surface area, not standards.

**Exit — met for 10.0 and 10.2.** A package built outside this repository,
resolved through `khora.lock`, compiled and run:

```
$ khora build src/main.kh
built ticket from 14 module(s)
$ ./ticket
910a2dec-8902-4cc1-beeb-8da1658eec67
```

`uuid` is at `~/dev/khora-uuid` — RFC 4122 version 4, eleven of its own tests,
no permissions at all because randomness arrives as the caller's capability. It
is not in this repository and not in `std`.

The repository's own proof is a test rather than an example, because an example
whose manifest reads `file:///C:/Users/...` builds on one machine.
`khora-codegen-llvm/tests/packages.rs` builds the repository, the commit, the
manifest and the app in a temporary directory, then compiles and runs. The
package it builds has a type, an `impl` of a standard library trait and a
generic, so the expected output only appears if a dependency's types are
visible, its trait impls are found by the consumer's checker, and
monomorphization crossed the boundary.

10.0's test still passes, which was the thing to watch: a resolver is the most
likely thing to make item collection depend on something it should not.

**Watch D15 throughout.** Not an item to schedule; a thing to notice. The
trigger is measurable and this is the phase where a language server starts
asking the compiler questions it was never asked before: introduce a
post-typecheck core IR when code generation must reconstruct semantics from
three or more independently-computed side tables to lower one ordinary
expression.

---

## Phase 11 — The scheduler

**A fiber becomes a stackful coroutine multiplexed onto worker threads**, which
is what `docs/design/fibers.md` decided a fiber *is*. Phase 5.3 shipped the
interface and made each fiber an operating-system thread, on the argument that
a program cannot tell the difference. That argument is sound and it has a
price, and this is the entry that names the price rather than leaving it in a
design note.

**Why it is load-bearing rather than an optimization.** A fiber costs about
33 KB today, so a server holds thousands of connections and not hundreds of
thousands — `std::net::http` says so in its own doc comment, and
`docs/positioning.md` claims Khora should be a candidate wherever a team
compares Rust, Go and TypeScript. Go's answer to that comparison *is* cheap
goroutines. Until this lands, the honest version of the claim is "for services
with hundreds of concurrent callers", which is a smaller claim than the one the
positioning makes.

It is also what makes the language's concurrency bet pay. Khora has **one**
concurrency model on purpose: the state-machine transform was rejected outright
because "can this suspend" colours every call graph, and there is deliberately
no non-blocking socket API for a framework to build a competing event loop on.
The upside is that there is no tokio-versus-async-std schism to have and every
library composes with every other. The downside is that the ceiling is the
runtime's alone to raise, and nobody else can work around it.

**What it costs**, from `docs/design/fibers.md` §"a real project":

- context switching, which is per-target assembly;
- stacks that start small and grow, which needs guard pages or segmentation;
- a work-stealing scheduler across worker threads;
- and the part that is easy to forget: **a reactor**. A scheduler whose fibers
  make blocking syscalls parks a worker thread per blocked fiber and has bought
  nothing, so `khora-rt` needs `epoll`, `kqueue` and IOCP underneath. That work
  stays inside the runtime — no Khora program sees a readiness API, and
  `std::net::socket` keeps its blocking shape — which is precisely the property
  that lets every existing program benefit without being rewritten.

**What it does not change: anything a program can see.** No signature in `std`,
no line in a reference application, no `async` keyword. `Fiber::spawn`, `join`,
`cancel` and the nursery mean what they meant. That is the whole reason 5.3 was
allowed to ship threads, and this phase either honours it or invalidates the
decision retroactively.

**Ordering.** After Phase 10 rather than before, and it is a genuine trade: the
scheduler is one deep piece of runtime work that one person does, while
packaging is what lets anybody else do anything at all. If the goal changes to
"be credible for services" before "be contributable to", these swap.

**Exit:** a hundred thousand fibers, each holding an idle connection, on a
machine that could not hold a hundred thousand threads — and every test in the
suite passing unchanged, because none of them can tell.

**Designed before it is built: `docs/design/scheduler.md`.** An outside review
went through this entry and the runtime, and most of what it asked for is
adopted there. Four things are worth pulling up to here, because they change
what the phase *is* rather than how it is done.

- **Yield points are not cancellation points.** The largest addition. A
  cancellation is observed at `!` in something that can raise, so
  `fn crunch() { while true { calculate() } }` has no cancellation points at
  all — which is fine on a thread and owns a worker forever on M:N. The runtime
  needs a second, cheaper idea: a safepoint that asks "should somebody else
  run?", cannot fail, and unwinds nothing. Loop back-edges are the required
  site, and emitting them is a **compiler** change, which is why it is decided
  now rather than discovered in the middle.

- **The reactor's interface is operation-oriented, not readiness-oriented**,
  which is where the design disagrees with the obvious answer. A
  `register/poll/wake` interface is epoll's model, and IOCP is
  completion-based; forcing IOCP to fake readiness costs a buffer and a copy on
  the platform this compiler is developed on. Submitting an operation and
  suspending until it completes is native on Windows and easy to emulate on
  epoll and kqueue — the retry lives inside the backend and the scheduler never
  learns it happened.

- **Growable stacks and an object-graph-ignorant scheduler pull against each
  other**, and the usual resolution does not survive the target. Growing by
  copying needs to know which stack slots hold references, which is exactly the
  knowledge Perceus lets the scheduler avoid. Growing by guard page keeps that
  separation and splits a mapping per stack — and Linux's `vm.max_map_count`
  defaults to **65530**, so a hundred thousand fibers with guard pages fails to
  allocate long before it runs out of memory. The recommendation is one large
  reservation carved into fixed slots with a prologue stack-limit check, which
  costs one mapping, commits lazily, needs no stack maps, and turns overflow
  into a clean error naming the fiber.

- **One thread-local in the runtime is already a bug waiting for this phase.**
  `shared.rs` keeps a per-fiber id in thread-local storage so `Shared::update`
  can refuse re-entry. Under M:N it fails in both directions, and the worse one
  is the false positive: a fiber scheduled onto a worker whose previous
  occupant holds the lock reads the same id, matches the recorded holder, and
  is killed with `fatal()` for a re-entry it never performed. A correct program
  aborts, dependent on timing. The runtime needs an explicit current-fiber
  pointer.

Staged **11A–11F** so a failure is attributable: the context switch first, then
workers and fairness, then the reactor and timers, then stealing, then the
bounded blocking pool, then soak. Khora stays buildable throughout; threads
remain the implementation wherever a backend has not landed.

**Where it has got to.** 11A through 11G are built, on Windows and on Linux —
the latter through `scripts/check-linux.sh`, which runs the runtime's tests
under WSL2 and is now part of the baseline. `Fiber::spawn` still makes threads,
deliberately, until the remaining backends land.

| | |
| --- | --- |
| 11A | context switch, and fiber identity that survives it |
| 11B | workers, queues, and a loop back-edge safepoint from the compiler |
| 11C | the wait protocol, timers, park and wake, cancel-while-waiting |
| 11C.2 | a `poll` reactor, so a socket read suspends a fiber and not a worker |
| 11C.3 | `recv`, `send` and `accept` that suspend a fiber rather than a worker |
| 11D | work stealing, so a burst spawned on one worker reaches the others |
| 11E | a bounded blocking pool, and `std::fs` routed through it |
| 11F | adversarial soak, a full state audit, and the scale numbers |
| 11G | `std::net` and `Fiber::spawn` wired to it, behind `KHORA_FIBERS` |
| 11H | the reactor could not be woken, and that was the twelve times |
| 11I | idle workers poll the backend themselves — 55% to 63% of threads |
| 11J | **next** — the wake path's remaining locks, then epoll, kqueue and IOCP |
| — | epoll, kqueue and IOCP, which is what the *socket* scale row waits for |

Measured on both platforms now, and they agree to within one per cent. **A
hundred thousand fibers waiting at once cost 407 MB and every one of them
woke** — about 4,270 bytes each, against roughly 33 KB for a thread, and flat
from a thousand up. The round trip is 1.04 s on Windows and 4.49 s on Linux.
And the compiler's safepoint is under `bench/service`'s noise floor: 800,730
req/s without it against 796,116 / 781,456 / 784,215 with, where the spread
among the three is wider than the gap.

**`vm.max_map_count` has an answer.** Two mappings per fiber, so a kernel at
the traditional default of 65,530 caps a program near 32,700 fibers. The one
measured here allows 1,048,576. The slot allocator the design argued about is
the fix if that ever bites, and it is not needed to reach the phase's number.

Three things found by building it rather than by designing it:

- A yielder installed once per body rather than once per resume is undefined
  behaviour that *looks* like it works, because every yielder switches back to
  the same worker. It survived a three-fiber interleaving test and became an
  access violation at five hundred fibers across four workers. The regression
  test keeps a value on the fiber's own stack across a suspension, which fails
  deterministically with three.
- The slot allocator the design argued for is not needed on Windows: stacks
  commit one page each and stay flat to a hundred thousand. Linux is unmeasured
  and is where `vm.max_map_count` would bite.
- The safepoint is a call and a budget rather than the inlined flag and timer
  the design assumed, and costs less than either was expected to.
- **A thread-local read on both sides of a stack switch is a use-after-move of
  the thread itself.** The compiler computes a thread-local's base address once
  and reuses it across a function, which is correct everywhere except in a
  scheduler, where the thread can change in the middle of one. `coro.rs` did
  exactly that with the yielder, and the result was a `SIGSEGV` a quarter of
  the time on Linux, never on Windows, landing on an unrelated thread long
  afterwards. Reading the code did not find it and neither did assertions,
  which perturbed the timing enough to hide it; core dumps and ThreadSanitizer
  did. `docs/design/scheduler.md` has the account, and the rule it produced
  applies to every thread-local this runtime touches.
- **Wiring it up found what six commits of testing could not.** Every number
  in 11A–11F measures the scheduler in isolation, and all of them are good: a
  hundred thousand waiting fibers cost 407 MB rather than 3.3 GB, and the soak
  runs millions of them without losing one. Then `bench/service` went through
  it and answered **59,965 requests a second against 760,771 on threads**, one
  machine, one sitting, which is the only comparison `bench/README.md` says
  travels. Correctness was never the problem — the HTTP conformance suite
  passes on both paths, pipelining and all — and no amount of soaking would
  have said so. 11H is that number.
- **A scheduler's bugs are arithmetic, not answers.** Nothing in 11F was
  found by a test that computed something wrong; every one was a count that
  did not come back to zero, and the instrument that found them —
  `Scheduler::audit`, naming all six places a fiber can be — was worth more
  than any individual test. The corollary is that the audit has to be
  *complete*: a waker carrying a task between two queues is a place, and
  omitting it reported ten fibers missing on four workers.
- **And the rule was not believed hard enough the first time.** Having found it
  in the yielder, the design doc asserted that the other four thread-locals had
  been checked and were fine. Stealing disproved that within a day: `current()`
  had the identical bug, a fiber asked who it was and was told about a
  different one, and the worker that panicked took the fiber it was holding
  with it. Every thread-local in `khora-rt` now goes through an
  `#[inline(never)]` accessor, with the safepoint budget the one documented
  exception.

Two things the review assumed were missing and are not. `bounded_nursery`
already exists, so the backpressure that cheap fibers make necessary has its
primitive. And `Shared<A>` exists, which unblocks the one item `fibers.md` left
open — a child's failure reaching its nursery rather than stderr — and that is
worth finishing alongside the scheduler, since structured concurrency is not
complete until a failing child cancels its siblings and hands its parent a
typed error.

---

## Phase 12 — What a service needs that Khora has not got

Phase 11 finished the concurrency claim. This is the list of everything else
`docs/positioning.md` promises and the tree does not have, found by reading the
positioning against the code rather than by planning.

**Two of these contradict the positioning outright**, which is why they are
first. The rest is ordered by what blocks deployment, then by daily friction.

`docs/design/ecosystem.md` §"Applying the rule to what is not written yet"
decides for each of these whether it is `std`'s, a package's, or neither, and
most of them are neither — they belong to the compiler or the runtime. Nothing
below should widen `std`.

### 12.0 `Decimal` — the positioning's own claim — **done, without the literal**

`std/decimal.kh` and `crates/khora-rt/src/decimal.rs`. A scaled integer:
`units` counted in steps of `10^-scale`, exact for anything written in decimal,
which is what money is. Almost all of it is Khora — add, subtract, multiply,
compare and rescale are `Int` arithmetic with the scales lined up, trapping on
overflow like every other number. Only division needed the runtime, because its
intermediate overflows sixty-four bits for ordinary money and is done in a Rust
`i128` nobody above has to see.

Seven tests compile and run, and the first is the one that matters:
`0.1 + 0.2 == 0.3` is `exact`. Also that `1.0` and `1.00` are equal, that `1.50`
prints as `1.50`, that a hundred pounds splits three ways to `33.33` with a
penny left over, that half-to-even sends `0.125` down and `0.135` up, and that
`1e-3` is refused as text because a number in exponent notation has been
through a float somewhere.

**The literal is not built.** `numbers.md` specifies `0.01d` and it stays
worth doing — an exact *constant* is most of the point — but the type without
the literal is useful and the literal without the type is not.

The original entry:


`positioning.md` says "particularly well suited to financial reconciliation".
There is no exact decimal type anywhere: not in `docs/design/numbers.md`, not
in `std`, not in `khora-types`. Only `Int` and `Float`, and `Float` deliberately
implements neither `Eq` nor `Ord` because its equality is a trap. An engine that
cannot represent ten pence reconciles nothing.

First because it is `std`'s and partly the language's, and because everything
written before it that touches money will have to be written again after.
`numbers.md` §"Decimal" has the four decisions.

**The literal is decided: `0.01` stays a `Float`, and a decimal is `0.01d` or
`Decimal::of(...)`.** Making bare decimals exact would be the single most
visible thing about the language and would make it a finance language whatever
the documentation said — and `positioning.md`'s first paragraph says it is not
one. Finance is what this is tested against, not what it is. That makes `0.01d`
the language's first literal suffix, which `numbers.md` spells out along with
the three lexer traps in it.

### 12.1 Civil dates and time zones — **done, and the split held**

`std/time.kh`. `Date`, `Time`, `DateTime` and `Offset`, on the proleptic
Gregorian calendar, with Howard Hinnant's civil-day algorithms — exact over the
whole range of an `Int`, no tables, and no leap-day special case because
shifting March to the start of the year removes it from the middle of the
arithmetic.

Eight tests against dates whose answers are known independently: 1900 is not a
leap year and 2000 is; the thirty-first of February is refused rather than
rolled into March; the millisecond before the epoch belongs to 1969, which is
where a truncating division puts the answer a whole day out; and four weekdays
including the moon landing.

**The tzdb is not here and cannot be**, exactly as `ecosystem.md` argued.
`Offset` is minutes east of UTC — an answer — and the rules that produce one
are a dataset released several times a year, which nothing behind a
compatibility promise can carry. A program that stores UTC and renders with an
offset it was handed is correct forever; one that bakes in zone rules is wrong
the next time a government moves a clock.

One thing found while writing the tests: `2024-02-28` plus 366 days is
`2025-02-28`, not the first of March, because the span contains the leap day.
I expected March and the calendar was right.

The original entry:


`Clock` gives `unix_millis` and `monotonic_millis`, and that is the whole of
time. Reconciliation is date-bucketed by its nature — value dates, settlement
dates, business-day calendars, what day it was in Tokyo — and none of that is
expressible.

The split is the interesting part and `ecosystem.md` argues it: the **types** are
`std`'s, and the **IANA database** cannot be, because it is a dataset released
several times a year and `std` is behind a compatibility promise that cannot
move that fast.

### 12.2 Cross-compilation — **step one done, and wasm comes out of it**

`KHORA_TARGET` now takes a target triple as well as the three platform names,
and `target_machine` initializes every backend inkwell was built with rather
than only the host's. `crates/khora-codegen-llvm/tests/targets.rs` reads the
first bytes of what comes out: **a WebAssembly module, an aarch64 ELF and an
x86-64 ELF, all emitted from a Windows host**, and a triple with no backend
refused by name.

Reusing `KHORA_TARGET` rather than adding a flag beside it is deliberate. It
already chose which `std` files a build reads; a second setting could disagree
with it, and then a build would generate for one platform while compiling
another's bindings.

**Nothing runs on another machine yet.** Steps two to four — a cross-built
`khora-rt`, a linker and sysroot, and a toolchain that fetches them — are not
done, so a cross build stops at the link. It now says so usefully: the raw
failure was `lld-link: unknown file type` about a perfectly good aarch64
object, and it now names the target the object was for and what is missing.

One thing worth keeping, **since fixed**: `family_of` mapped `wasm32` to the
`linux` `std` files, which was wrong — a Worker has no sockets and no
filesystem. It was written down in `khora-db` as the next thing that had to
change, and then was not for several entries. A comment admitting a bug reads
like a decision and is an outstanding defect; this one shipped underneath wasm
code generation.

WebAssembly is its own family now, and `_native` marks the five unsuffixed
modules that call an operating system — `fs`, `env`, `process`, `net/http`,
`net/tls`. A wasm build selects eight `std` files rather than sixteen, and
`portability.rs` checks that the remainder has no dangling import, which is the
failure removing modules actually causes. `docs/design/targets.md` §"Which
`std` a wasm build gets".

The original entry:


`khora-codegen-llvm` calls `initialize_native` and `get_default_triple`; there
is no `--target`. Competing with Go without `GOOS`/`GOARCH` is a losing
position: no arm64 container, no musl static binary in a `scratch` image, no
macOS build from a Linux runner — and that last one is what
`.github/workflows/runtime.yml` pays a ten-times billing multiplier to avoid.

**WebAssembly is wanted for edge hosting** and is the hardest consumer of the
same mechanism, so it goes second rather than first. `docs/design/targets.md`
has it in full, including the part that is not a matter of effort:
**WebAssembly cannot switch stacks**, so Phase 11's coroutines cannot exist
there until the stack-switching proposal ships. The recommendation is to ship
wasm *without* fibers — an isolate is single-threaded anyway, and 11E's
blocking pool already falls through to inline when there is no worker to
protect — rather than pay Asyncify's cost on every wasm user.

**The first wasm target is `wasm32-unknown-unknown`, for Cloudflare Workers**,
which is the motivating platform. It settles several questions at once: the
host does the networking, so sockets and TLS are not needed; the isolate is
single-threaded, so the no-fibers build is the right one; and there is no
filesystem, so `Db` is satisfied by D1 behind the capability rather than by
SQLite. Fastly and Spin want `wasip1` instead, so a second wasm target is
separate work rather than a rename.

That document also records a correction worth keeping: **AWS CloudFront does
not run WebAssembly.** CloudFront Functions is restricted JavaScript and
Lambda@Edge is Node or Python.

### 12.3 Observability — **the middle layer, done**

`std/trace.kh`. The vocabulary — `Context`, `Span`, `Attribute`, `Value`,
`Status` — plus W3C `traceparent` in both directions, a `Tracer` effect, and a
no-op handler that is the default because tracing which costs when it is off
gets turned off.

Five tests, and the ones that matter are about the wire format, since that is
the only part another system reads: a header round-trips exactly, the sampled
flag survives, and six malformed headers are refused — short, wrong version,
uppercase hex, wrong separator, non-hex digits. A header half-read is a trace
joined to the wrong parent, which is worse than starting a fresh one.

**The rank-2 question `observability.md` left open turned out not to need
answering.** A scoped `around` is generic in the body's result, and an effect
field that is itself polymorphic is a different feature from ordinary
generics — so `around` is an ordinary generic *function* that takes the tracer
as an argument instead. Same scoping, no type-system change, and the effect
stays monomorphic.

Exporters remain a package, by the same rule that keeps Postgres out. What is
not built yet is the part `observability.md` calls the real middle layer:
propagation across a spawn, a steal and a cancellation, which needs the
scheduler to carry the context. The types exist for it now.

The original entry:


Nothing in `std` emits a log line, let alone a span. For services, workers and
event consumers that is disqualifying — and Khora can do it better than the
ecosystems it is competing with, because the three things that make
instrumentation manual elsewhere are structural here: a capability is an
interception point, a fiber's lifetime is a span's lifetime, and code generation
already inserts runtime calls at loop back-edges.

`docs/design/observability.md` has the design. `std` owns propagation and the
vocabulary; exporters are a package, by the same rule that keeps Postgres out.

### 12.4 Debug information — **line tables done, variables not**

A trap used to say what happened and not where:

    khora: Int addition overflowed

and that was the whole of it, in a program of any size. `khora_bounds_fail`'s
own doc comment said "the useful thing to do is say where", which it could not.
Now:

    khora: Int addition overflowed
       6: deep
                 at examples/demo/src/main.kh:6
       7: middle
                 at examples/demo/src/main.kh:10
       8: main
                 at examples/demo/src/main.kh:15

**What is emitted.** `khora-codegen-llvm/src/debug.rs`: a compile unit, a
`DISubprogram` per emitted function naming its own file and line, and a debug
location on every expression. One `DIFile` per source file, because a build is
whole-program — a specialization of `List::map` belongs to `std/list.kh`
however deep in an application it was reached from, and a backtrace that walks
out of user code into `std` should say so. DWARF everywhere, CodeView on an
MSVC target, chosen by the triple.

**What is not.** Variables. A `DILocalVariable` needs a `DIType` for every
Khora type, which means describing the heap layout — header, tag, field words —
in DWARF. That is a second piece of work of comparable size, and worth having;
it is not worth blocking line tables on, because a backtrace without variables
is most of the value and no variables at all is none of it.

### Four places it silently did nothing, which is the story worth keeping

Every one of these verified clean, produced no error, and emitted no usable
debug information. That is this feature's failure mode: it does not break, it
evaporates.

**The builder keeps its last location across functions.** `Debug::leave`
forgot the subprogram but the *builder* held the location, and the next
function's `alloca`s inherited it. LLVM's verifier calls that "!dbg attachment
points at wrong subprogram" — a failed build, and the right answer: a location
naming another function's scope is not a worse answer, it is a corrupt one. 42
tests caught it at once.

**A lifted lambda is not its enclosing function.** The closure pass re-entered
the *owner's* symbol, so `create_function` attached a second `DISubprogram` to a
function that already had one, and every instruction in the lambda pointed at a
scope it was not in.

**The linker discards what it is not asked for.** The object carried `.debug$S`
and `.debug$T`; the executable had neither and no PDB was written. Perfect
metadata, emitted into an artifact that threw it away. `-g` on the link.

**The trap handler's own frames are not the answer.** Six frames of
`backtrace_rs` and `force_capture` sat above the line that overflowed, and the
top of a backtrace is what anybody reads first.

Which is why the tests assert on **the output of a program that trapped**
rather than on the metadata. Every intermediate check — the IR has
`DISubprogram`, the object has debug sections — passed at a point where the
feature did not work.

### The cost, measured

| | executable | cold build |
| --- | --- | --- |
| `risk_analyzer`, debug info on | 5,898 KB | 2,470 ms |
| off (`KHORA_DEBUG=0`) | 3,816 KB | 2,192 ms |

About 2 MB and 13%, which is what `-g` costs in any toolchain. **On by
default**, and that is a decision worth revisiting rather than a law: there is
no release mode to hang it off yet, and the default that serves a language
being brought up is the one where a crash can be read. It should become part of
an optimization level when there is one — and 12.2 makes that sooner rather
than later, because a Cloudflare Worker has a size limit and 2 MB of DWARF is a
material fraction of it.

Backtraces themselves are behind `RUST_BACKTRACE`, the switch every Rust binary
on the machine already answers to rather than a Khora-specific one nobody would
guess. A trap without it says how to get one.

**Verified on Windows only, locally.** The DWARF half — every non-MSVC target —
is exercised by CI's backend job on ubuntu and macos and by nothing on this
machine, because WSL has no LLVM. That is a gap in what was checked before
pushing, not a claim about what works.

### 12.5 Database access — **the capability and the contract, done**

`std/db.kh`. The `Db` effect, the `Cell`/`Row` types two packages must agree on
to exchange a result, a deliberately coarse `DbError`, and `transaction`.

**`transaction` is the whole reason the module is in `std`.** A caller who
writes begin, body and commit by hand has written the correct thing only for
the path where nothing goes wrong; the paths where something does are the ones
that leave a connection holding locks until somebody restarts the service.
Four tests run against a handler that prints what it was told to do, so the
transcript *is* the assertion: a body that returns commits and does not roll
back, a body that fails rolls back and never commits, a refused commit is
reported as itself, and cells do not coerce.

`Cell::Money` carries a `Decimal` and there is no `Float` variant, which is
12.0's argument applied here: a `NUMERIC` column read through a float is a
number that has already lost.

**Still open, and it is the half that matters most:** rolling back when the
fiber is *cancelled* rather than when the body returns an error. That needs
`Region`'s `defer` threaded through `transaction`, and the doc comment says so
where it will go.

### Three things this cost, which are worth more than the module

Writing the tests hit three rough edges, none of them in the database code.

**`+` on `String` did not infer inside a closure — since fixed, and it was
not about handlers.** `soFar + "begin;"` in a `Shared<String>` update reported
`arithmetic: expected Int, found String` with the parameter annotated `String`.
Recorded here as a handler problem because that is where it was met; it was
nothing of the sort. `+` on a `String` failed inside **any** closure, and two
separate bugs were stacked under it — see "The closure that could not
concatenate" below. `std/json.kh` was unaffected only because its
concatenations are of literals and named-function parameters, never of a
closure's.

**A generic error parameter did not monomorphize.** `transaction<A, E: Show>`
failed with "`problem` has no type the backend can represent" even where `E`
was plainly `String`. The signature is now concrete in `DbError`, which is
better design anyway — but it was chosen under pressure from a limitation, not
freely, and that is worth recording.

**A diagnostic named the wrong problem, again.** An ambiguous type variable —
a body that only ever returns `Err`, so nothing says what `A` is — reports
"`answer` has no type the backend can represent; phase 2 handles `Int`,
`Bool`, `String`, `()` and ADTs". The type is not unrepresentable, it is
*unknown*, and the message sends the reader looking at the backend's
capabilities instead of at their own annotation. This is the second such
message this session; `vision.md` non-negotiable 4 says diagnostics are the
product.

The original entry:


Nothing. `ecosystem.md` decides the shape: the `Db` capability, the row and
value types, and **what a transaction does when its fiber is cancelled** belong
to `std`; the SQLite engine is a first-party package; Postgres is a package.
The transaction-under-cancellation contract is the middle layer here and the
only part that fails in production rather than in testing.

### The closure that could not concatenate — two bugs, one symptom

Chased because 12.5 left it undiagnosed, and it was worth the hour: the
symptom was one line in one test, and underneath it were two independent
defects, each of which silently mistyped correct programs.

**One: the string check ran before zonking.** `infer_binary` asked
`matches!(left, Type::Str)` on whatever `infer` handed back, and two lines
below it the arithmetic branch asked the same question of the *zonked* type.
A `String` that arrives as a solved inference variable — which is what every
closure parameter is — answered "not a string", fell through to arithmetic, and
was reported as `expected Int, found String`. Only two literals worked. That
one branch zonked and the other did not was the whole of it.

**Two: a closure parameter's annotation was dropped in lowering.**
`lower_lambda_named` read `p.name()` and never `p.ty()`, so `fn (s: String) =>
…` reached the checker with the annotation gone and the parameter got a bare
variable. This is errata 36 exactly — `let x: Bool = 5` compiling clean —
committed a second time in a different place, and the `TypeRef` doc comment
that describes the first sits forty lines from the code that repeated it. An
annotation that is only a comment is worse than no annotation, because it is
believed.

The two hid each other. Fixing the zonk alone still left `let g = fn (s:
String) => s + "b"` broken, because a closure in a `let` has no call site yet
to hint from and the annotation is its only evidence.

**And a third thing, found by fixing the first two.** `fn s => s + "b"`, with
nothing on the left to go on, defaulted the variable to `Int` and then reported
the *string literal* as the mismatch — naming the wrong operand in a line where
nothing is wrong. The left operand decides which arithmetic this is, but it can
only decide when it knows something; where it is still a variable, a `String`
on the right settles it, because there is no `Int + String` to be ambiguous
with. Arithmetic defaults exactly as it did.

Regressions in `khora-types/tests/closures.rs` for each, and one in
`khora-codegen-llvm/tests/shared.rs` that accumulates a log through
`Shared::update` — compiled and run, because the original symptom was a program
and not a judgement.

### A type name that names nothing — **fixed**

Found while confirming the concatenation fix, by writing down what the compiler
*should* say and checking, and the most serious thing this session turned up.

**`fn f(x: Wibble) -> Int { 1 }` type-checked clean.** Unresolved names become
`Type::Adt { home: None }`, whose comment says the error is "already an error
where the name was resolved" — and `TypeHomes::of` says the same thing. For a
*value* name it is true, and `cannot find x in this scope` is reported. For a
type name nothing resolved it and nothing complained. Two comments asserting
that somebody else handles it, and nobody did.

What kept it alive is that the consequence looks mild. The invented type is
nominal and distinct from everything, so it never unifies and genuine
mismatches are still caught — reported as ``this function returns `Wibble`, but
its body has type `Int` ``, which reads as two real types disagreeing. The
reader goes looking for a mismatch instead of for a typo, and a typo in a
signature is the ordinary way to meet this.

The same gap made a bound naming no trait surface as ``no method `hi` on `A`,
whose bounds are `Wibble` `` — blaming the method for the bound's problem, and
reading as though `Wibble` were a real trait that happens to lack `hi`. One
fix covers both, because a bound is a type mention.

`khora-types/src/unresolved.rs`, walked over the syntax rather than over
resolved types: the point is to report the written name where it was written,
and by the time a `Type` exists the range is gone. Deliberately narrow in two
places — every type parameter in a declaration counts as one scope rather than
being scoped precisely, and a qualified name is passed over — because a
diagnostic that has never existed before earns its place by never being wrong,
and both narrowings can only make it report less.

Ten tests, half of them cases that must stay quiet: the builtins, type
parameters in their own declaration, `Self` and `Self::Item`, function and
tuple types, positional variant payloads. The real check was the corpus — all
of `std`, both reference applications and the whole test suite pass with it on,
which is what says the over-approximations are the right ones.

### 12.6 A C export surface

`docs/design/compatibility.md` is right that there is no stable Khora ABI, and
that is a different question from whether a Khora library can be *called*.
Exporting a C ABI — a shared library with a header — is how Khora gets used
without anybody rewriting anything: a Python extension, a Node addon, a plugin
for something written in C++. It is also the cheapest adoption path there is,
and it costs a calling convention and a lifetime story rather than a language
feature.

### 12.7 The compile-time budget — **measured, and it is not a crisis**

`vision.md` non-negotiable 4 calls compile speed a requirement tested from the
first working compiler, and "cold `khora build` beats `cargo build` on an
equivalent Rust program" is one of its falsifiable checks. Nobody had run it.

Cold builds of the three reference applications, each including all of `std`
and the link:

| | lines | cold build |
| --- | --- | --- |
| `core_demo` | 43 | 1,382 ms |
| `risk_analyzer` | 245 | 1,527 ms |
| `link_shortener` | 430 | 1,723 ms |

**The shape matters more than the numbers.** About 1.3 seconds is fixed —
`std`, code generation, the link — and 387 extra lines cost about 340 ms. That
is a fixed-cost-dominated profile, which is what says whole-program
monomorphization is not yet superlinear at this size. It would be the first
thing to look at if that line ever bent.

Two honest caveats. These are a **debug** compiler; a release build of `khora`
would change them substantially and the comparison against `cargo` is not fair
until it is made. And the corpus is small — the number worth defending is the
one taken when there is a program big enough to strain it.

Not a crisis, so not a project. Recorded so the next measurement has something
to be compared against, which was the point of the check.

The original entry:


Whole-program monomorphization plus LLVM is superlinear in program size, and
compile speed is the headline feature of the language Khora is compared to.
Nobody has measured it. The corpus is still small enough that a baseline is
cheap to take and expensive to reconstruct later — this entry is a measurement
and a number to defend, not a project.

### 12.8 What a trap does to a process — **argued, and decided against for now**

`docs/design/traps.md`. The argument this entry said had not been had, had.

**A trap ends the process, and that stays true.** Not because containment is
wrong — a server wants it, and phase 11 already built the boundary it would use
— but because the mechanism it needs is phase-sized and taxes every program
that never traps.

**The decisive argument is mechanical, not philosophical.** Containment means
unwinding, and Khora deliberately has no unwinder: `backend/types.rs` says "no
landing pads, no personality routine: a raise is a return with a tag". Perceus
is what makes removing that expensive — every live value between the trap and
the fiber boundary holds a reference count, so ending a fiber without running
the decrements leaks everything it touched. On a server that is memory growth
proportional to the rate of the bug with no allocation site to blame, which is
the worst possible shape for a production problem. And the unwinding would have
to cross a `corosensei` stack switch, which is the part with no recipe.

Of the three cheaper answers, two fail and one is already deployed: leaking on
cancel turns a trap into a denial-of-service primitive an attacker can drive;
region-backed allocation would sidestep the whole problem and is the
interesting long-term answer, but allocation is not arena-backed and `Shared`
outlives its scope by design; and an external supervisor — a container runtime,
systemd, a Workers isolate — needs no language change and is what operators
actually run. Its real cost is that in-flight requests on that process are lost
and not just the bad one, which is the honest weakness of this decision.

**What the decision obliges.** A trap that cannot be contained must be
maximally diagnosable. 12.4 gave it the line and its callers; this entry adds
the fiber:

    khora: Int addition overflowed on fiber 4102

Empty on the root, where a number would be noise. On a machine running a
thousand at once, that clause is what lets a crash be matched against a request
log instead of guessed at.

And `docs/positioning.md` promises no fault isolation, which this decision means
it must not start. A language that says one request cannot take the others down
and then does is worse than one that never said it.

**Three things would overturn it**, written down so the decision is falsifiable
rather than permanent: a real service showing traps frequent enough that
restart is an availability problem; region-backed allocation landing for other
reasons; or a target with no supervisor to restart anything.

### 12.9 Supply chain — **the SBOM, not the signature**

`khora sbom` writes CycloneDX 1.5 JSON. `khora-pkg/src/sbom.rs`.

Almost nothing new is computed. `khora.lock` already records every resolved
package, the immutable revision it came from, the SHA-256 of its visible files
and what each depends on — which is most of a bill of materials already. This
renders it in the shape scanners read.

Three decisions worth keeping:

**No timestamp.** §6.1 asks for bit-for-bit reproducible builds, and a clock in
a generated artifact is the ordinary way to lose that: the same input would
produce two documents and nothing downstream could tell a real change from time
passing. CycloneDX makes `metadata.timestamp` optional, so it is omitted and the
document is a pure function of the manifest and the lockfile. Components are
sorted by name for the same reason — a resolver may reorder without anything
having changed. The cost, named rather than hidden: a consumer that wants to
know when a document was produced has to learn it from where the file came from.

**Rendered from the resolution, not from a lockfile read off disk.** Those
differ exactly when the lockfile is stale, which is the case an audit most wants
not to be misled about. `--locked` refuses the difference rather than absorbing
it.

**A path dependency says it is unpinned**, in a property, because there is no
immutable thing to hash and a component with no checksum otherwise reads as an
omission.

**Not signing, and not provenance.** Both need a decision about keys that this
does not make. An unsigned SBOM is still what a scanner reads; a signature is
what makes it *evidence*, and inventing a key story to have one sooner would be
worse than saying so. That is the rest of 12.9.

Eleven tests: seven on the document's shape, four running the binary — the
empty case produces a document rather than nothing, two runs agree byte for
byte, and a directory with no `khora.toml` is refused by name.

### A feature gate that only the front-end build could catch

Found while wiring the command up, and worth its own note because the thing it
broke is the thing the gate exists for. 12.4 added `mod debug;` without
`#[cfg(feature = "llvm")]`, so `cargo build` with no features failed on
inkwell. Every check in `scripts/baseline.sh` passes `--features llvm`; CI's
`check` job is the only thing that builds without it, and this would have gone
to CI green from here.

Moving the gate then broke the other way: `toolchain.rs` is unconditional and
needs `debug_info_wanted()` to decide whether to pass `-g`. An environment
variable does not belong behind an LLVM feature, so it lives in `toolchain`
now, with a comment saying why it is not where it looks like it should be.

### 11H — the reactor could not be woken, and that was the twelve times

`bench/service`, one machine, one sitting, 48 reused connections:

| | req/s |
| --- | --- |
| threads | 782,149 |
| scheduler, after this entry | 429,000 |
| scheduler, before it | 59,965 |

**One `poll` that could not be interrupted was the whole of it.** The reactor
waited on the set of sockets it had been given, and a socket registered a
microsecond later was not in that set — so a fiber that had just parked waited
for the timeout rather than for its data. At a millisecond that is not a
latency, it is a ceiling: 48 connections divided by a millisecond is about the
number of requests a second that were coming out.

The fix is a loopback pair the reactor also watches. `register` writes a byte
to it, `poll` returns at once, and the next round includes the new socket. That
also lets the timeout go from one millisecond to fifty, because it is now a
backstop rather than the mechanism.

**Claiming `polling` before reading the watch list is the correctness
argument** for nudging only when somebody is waiting. A registration that sees
it false knows the reactor is not in a `poll`, so the entry it just pushed will
be read when the next one starts. A registration that sees it true may not be
in this round's list, so it nudges. Reading the list first would leave a window
where a registration is neither visible nor able to announce itself, and that
fiber waits out the timeout.

### The trail

Kept because the rejections are worth as much as the fix, and because a single
benchmark number with several changes folded into it says nothing.

| | change | req/s |
| --- | --- | --- |
| — | scheduler, as 11G left it | 57,467 |
| E1 | `BUDGET` 128 → 4,000,000,000, so nothing is preempted | 59,197 |
| E2 | reactor spins instead of sleeping — diagnostic only | 613,571 |
| E3 | wakeable reactor, 50 ms backstop | 422,346 |
| E4 | nudge only while `polling`, claimed before the list is read | 415,708 |
| E5 | one `recv` retry before parking | 428,874 |
| — | E4, measured again at the end | 440,012 / 418,247 |

E2 is not a candidate implementation: it is a core at a hundred per cent to
prove where the time went. E1 and E5 were reverted.

### What did not work, and one of them was my own first answer

**Preemption is not the cost, though it looks like it.** The counters said
587,130 of 747,963 resumes ended on the safepoint budget running out — 78%,
about twice per request — which is a striking number and the obvious suspect.
Raising `BUDGET` from 128 to four billion moved throughput from 57,467 to
59,197, which is nothing. A preempted fiber goes back on its own worker's queue
and is picked up again immediately; it never touches the reactor, the parked
map or another thread. **A large counter is not a large cost**, and the only
way to tell is to turn the thing off.

**One retry before parking does not help.** Registered, then read again before
suspending, on the theory that a keep-alive connection is read the moment its
response is written and the data is nearly there: 428,874 against 415,708,
inside the eight per cent this repository calls noise. The data is genuinely
not there a microsecond later. Deleted rather than kept on the grounds that it
might help somewhere else.

### 11I — the worker that will run the fiber is the one that notices its socket

`bench/service`, one sitting: **513,500 against 816,963 on threads, 63%**, from
429,000 and 55%.

An idle worker now waits on the backend instead of on the condvar, and wakes
what it finds. Readiness discovered that way is discovered by the thread that
is about to run the fiber, which is one operating-system handoff shorter than
being told about it — `scheduler.md` §10a's argument, which is that a socket
becoming ready and a task being injected are the same kind of event and should
end the same wait.

Three things make it work and each is load-bearing:

  - **Exactly one worker waits on the backend**, held by a token. Every idle
    worker in `epoll_wait` is a herd; on a completion port it would be right,
    which is why who may block stays a backend's business.
  - **`inject` nudges the reactor**, because a worker waiting in `poll` cannot
    hear a condvar, and the worker best placed to run an injected task would
    otherwise be the last to know.
  - **The reactor thread stays**, as a backstop. A pool that never goes idle
    would otherwise never poll, and correctness must not depend on a lull.

A ten-millisecond slice rather than one: throughput is the same to within
noise, and an idle worker wakes a hundredth as often. The nudge is what makes
the longer slice safe.

### What is left, in the order worth trying

The gap is now 1.8× rather than 12×, and spinning the reactor instead of
sleeping gives 613,571 — so roughly half of what remains is still the wake
path and the rest is elsewhere.

  - ~~Let workers poll the reactor themselves.~~ Done in 11I, above.
  - **Carry the fiber's state in the `Watch`** rather than looking it up in the
    `live` map on every readiness. One global `HashMap` lock per wake, for
    something the registration already had in its hand. **A wake token may
    carry the state and must never carry the `Task`** — enough to mark a fiber
    runnable, not the thing that gets resumed. Only the scheduler path owns and
    moves a runnable `Task`, and a token holding one would put a second owner
    on the far side of the reactor, which is exactly what `Audit::in_hand`
    exists to catch.
  - **Batch the wake.** A `poll` that finds thirty ready sockets currently
    takes the shared queue thirty times and notifies thirty times.
  - **Then epoll, kqueue and IOCP.** The row above this one in the phase table
    has been waiting for a measurement to justify it and now has one — but
    after the three items above, because each of them is cheaper and none of
    them is made unnecessary by a better `poll`. Not io_uring: nothing yet
    says it is needed.

`docs/design/scheduler.md` §10a is where that goes, written before it is built:
the shape to move toward, the invariants a new I/O architecture may not bend to
get there, who is allowed to block in the backend, and the acceptance criterion
— seventy to eighty-five per cent of the thread figure, which E2 showed is
reachable.

**And the measurement to take first.** The remaining gap is 429,000 against
613,571 spinning, and nothing says which part of the wake path that is. Sample
the stages, report percentiles, then choose. 11H is the argument: the counter
that looked most damning was the one that cost nothing.

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
