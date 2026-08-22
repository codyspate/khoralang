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
| D4 | **What in `[permissions]` is actually compile-time enforceable?** `allow-net=0.0.0.0:8080` is checkable when the address is const; a computed URL is not. Likely part static, part runtime-gated. Capability rows make this far more tractable than it would otherwise be. | 6.x |
| D8 | **The FFI contract.** Narrowed by A6 from "map Rust onto Khora" to three answerable questions: exactly which types may cross and in what layout, how a foreign resource's lifetime is tied to a Khora binding, and what a callback looks like. The first is mostly written already in `khora-rt`'s module documentation, and the second has a working shape — a region and a fiber handle are both foreign resources with runtime-provided drop glue. | 7 |
| D11 | **What happens to reference cycles.** None can be built today — the heap graph is provably a DAG — and mutable fields end that. A tracing cycle collector is ruled out by non-negotiable 5, which leaves "a cycle leaks, and a weak reference breaks it". Decide alongside records rather than in the abstract. `docs/design/memory.md` §2 and §4. | records |
| D12 | **What Khora promises not to break.** Observable semantics, package identity, public ABI and versioning rules, editions, and which changes are allowed in a minor release. Nothing in this roadmap owns this today, which is the failure mode errata entry 20 names. | 8.x |
**Closed:**

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
artifact could ever stand alone is D12.

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

**Exit:** the reference application typechecks — **met**, pinned by
`the_reference_application_type_checks` — and an unhandled capability is
rejected with a diagnostic naming the absent label and the function that
required it — **met**. Serving a request needs real I/O and a backend that can
build a value out of an effectful function; both belong to phase 5 and after.

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

- **6.1 D11, mutable fields, and what may cross a fiber.** The widest blocker in
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
- **6.2 Fixed-width integers, and bytes.** No `u8` means no bytes, which means
  no parsing, no wire formats, no encoding. `Int` alone is a toy.

  **Overflow traps, in every build.** Swift's answer rather than Rust's: a
  program that passes its tests and then wraps in production is the failure
  worth spending a branch to prevent, and phase 9 can remove many of them.
  Explicit wrapping operators are how you ask for the other thing.
- **6.3 Floats.** Not in the backend and not in `Type`. `std::ai` promises
  tensors and the reference application cannot compile without them.
- **6.4 Arrays.** Contiguous and bounds-checked. `List` is a linked list, which
  is the wrong shape for almost everything and the wrong shape for reuse
  analysis to pay off on.

**Exit:** a hash map, written in Khora, in `std`, with a test that a
round-trip of inserts and removals leaves the live-object count at zero.

---

## Phase 7 — The foreign boundary

Per A6 and `docs/design/ecosystem.md`. Small, because the boundary already
exists: every runtime call generated code makes is a C ABI crossing, and the
rule for what may cross was settled the hard way in errata 35.

- **7.1 Decide D8** — which types cross and in what layout, how a foreign
  resource's lifetime is tied to a Khora binding, and what a callback is.
- **7.2 `extern` declarations that carry effect clauses.** A foreign function is
  opaque, so its `with` and `raises` are a promise the compiler takes on trust
  and then enforces on every caller. This is where the capability discipline is
  asserted rather than inferred, and where D4 gets its teeth.
- **7.3 Foreign resources as counted values.** A Khora object holding the
  pointer, with a release that calls the foreign close — the shape a region and
  a fiber handle already have, so an open file closes on every path out
  including a raise.
- **7.4 Syscalls**: files, sockets, a clock.

**Exit:** read a file and write its contents to a socket, from Khora, with the
file closed by the region that opened it — on the error path as well as the
ordinary one.

---

## Phase 8 — A standard library worth using

The first real test of whether Khora is pleasant to write libraries in, which
is a question no amount of compiler work answers. Collections, strings and
encoding, JSON, time, logging, and HTTP over the syscalls from phase 7 —
everything a normal program touches, written in Khora and generic over its
effects.

The bindings A6 names — TLS and crypto, compression, numeric kernels — are
consumers of phase 7 and are *not* on this critical path. A great deal of Khora
can be written before anything needs TLS.

**Exit:** the reference application runs and serves a request. That is phase
4's unmet half, and it stays the criterion because it exercises the whole
stack: capabilities, a fallible service, `catch`, a router carrying its
handlers' requirements, and now real I/O underneath.

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

- **10.1 Decide D12** — what Khora promises not to break. It comes due here
  because publishing a package is the first act that makes a promise.
- **10.2 `khora-pkg`**: `khora.lock` with SHA-256 hashes, content-addressed
  cache, DAG task runner. Also what finally lets the orphan rule be *enforced*,
  which needs cross-package resolution.
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
purpose: D12 asks what may not break, and that question is unanswerable while
the language is still moving. A minimal package manager could come earlier if
the goal changes to letting other people start.
