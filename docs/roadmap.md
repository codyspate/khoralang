# Roadmap

Working plan for building the Khora compiler. Iterate on this file directly —
it is the source of truth for sequencing, and each phase states the test that
proves it is done.

Scope comes from `docs/project.md`. Where that document is wrong or silent,
`docs/errata.md` records it and this file schedules the fix.

## Decisions taken

`docs/vision.md` states the goal these serve. When a call is ambiguous, that
document breaks the tie.

| # | Decision | Rationale |
| --- | --- | --- |
| A1 | **Thin vertical slice first.** A deliberately small subset of Khora goes all the way to a running native binary before any stage is widened. | De-risks the backend early. A type system with nothing to run behind it hides integration problems until they are expensive. |
| A2 | **LLVM via `inkwell`, no Cranelift.** | Matches the spec, and gets `-O3`/LTO plus mature static-musl and aarch64-darwin linking without building a second backend. Toolchain cost paid in Phase 0.1. |
| A3 | **Salsa from the start.** `khora-hir` and `khora-types` are salsa queries. | Retrofitting incrementality means rewriting every pass. §6.5 wants sub-15 ms LSP responses; that is not a bolt-on. |
| A4 | **Full HKT and typeclasses.** Native `* -> *`, kind inference, instances. | What Rust structurally cannot express. Carries `Traversable`, `Stream` and user abstractions. Note it is justified by *containers*, not by the effect system — see A8. |
| A5 | **Structured concurrency with interruption in v1.** Fibers, cancellation that runs finalizers, `Scope`-bound lifetimes, `Schedule`. | Effect's headline safety property, and §6.4 already assumes it. Retrofitting interruption into a runtime that never had it is close to a rewrite. |
| A6 | **First-class Rust interop** as the ecosystem strategy. | A new language with no libraries loses to Go and Node on merit-independent grounds. crates.io is native and LLVM-based — the closest ecosystem to reach. |
| A7 | **Developer experience is a product requirement.** Diagnostic quality, compile speed and LSP latency are tested from Phase 2, not polished in Phase 6. | The thesis is "beats Rust's DX". Rust's advantage is mostly cargo, rustc diagnostics, rust-analyzer and clippy. Deferring all of it means we cannot evaluate our main claim until the end. |
| A8 | **Direct-style algebraic effects, not a monadic `Effect<A, R, E>`.** Effects are rows on the signature (`with` / `raises`), discharged by handlers. | The spec already specifies Perceus and Leijen/Rémy scoped rows — both Koka, which pairs them with exactly this model. A monadic API fights that substrate. Effect-TS's `Effect.gen`/`yield*` is itself a simulation of direct style, just as `TypeLambda` simulates HKT. |

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
| D1 | **How do handlers execute natively?** One-shot versus multi-shot continuations; how handler frames interact with Perceus reference counting and with fibers and interruption. Koka is direct prior art for the whole combination, which narrows this considerably from where it started. Still the largest unknown. | 4.3 |
| D2 | **What does `Type.member` mean?** Under "universal dot", `Effect.map`, `report.risk` and `RiskLevel.Low` are spelled identically. The parser deliberately refuses to guess. See errata #10. | 2.1 |
| D3 | **`Schema.Spec` projects an associated type off a type *variable*.** With A4 this is tractable — associated types on typeclasses — but the coherence rules still need deciding. | 4.2 |
| D4 | **What in `[permissions]` is actually compile-time enforceable?** `allow-net=0.0.0.0:8080` is checkable when the address is const; a computed URL is not. Likely part static, part runtime-gated. Capability rows make this far more tractable than it would otherwise be. | 6.x |
| D6 | **Which typeclasses ship in `std`, and what are the coherence rules?** Orphan instances, overlapping instances, and whether instance resolution is nominal or structural. A4 settled *whether*; this settles *how much*. | 3 |
| D7 | **Effect and handler syntax.** How an effect is declared, the exact `with`/`raises` clause grammar, handler syntax, and whether `raises` is sugar over `with` or a distinct row. Blocks the `std/` rewrite. | 1.5 |
| D8 | **The Rust interop boundary.** How Rust's ownership and traits map onto Khora's reference counting and rows; whether we bind at the C ABI or generate richer shims. | 5 |

**Closed:** D5 (`ask` arity, errata #3) is dissolved by A8 — `ask(:label.op)`
does not exist in direct style; you call `ledger.get_history(x)`.

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
§5.1 remain Phase 6.

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

- **1.1 `test` and `bench` declarations.** `test "name" = { ... };` and
  `bench "name" = { ... };` per §6.4. Currently a parse error — confirmed.
- **1.2 Manifest parser.** `khora.toml` per §4.1, including `[permissions]`,
  `[fmt]`, `[lints]`, `[tasks.*]` and `[build] plugin`. Unknown keys warn rather
  than abort.
- **1.3 `khora fmt`.** CST to canonical text per §6.2: two-space indent, explicit
  semicolons, pipeline continuation aligned to the source expression,
  alphabetised and deduplicated imports.
- **1.4 Decide D2** → `docs/design/associated-items.md`. Blocks 2.1.
- **1.5 Decide D7** → `docs/design/effects.md`: effect declaration, `with` and
  `raises` clause grammar, handler syntax. Then extend the grammar and rewrite
  `std/` and `examples/risk_analyzer` in direct style. This supersedes the
  monadic API those files currently show.

**Exit:** corpus and manifests parse; `khora fmt --check` clean on the corpus;
property tests show `fmt(fmt(x)) == fmt(x)` and that formatting preserves the
non-trivia token stream; `std/` reads in direct style.

---

## Phase 2 — Vertical slice: Khora Core to a native binary

The milestone. A subset chosen to exercise every stage while excluding
everything hard.

**In:** modules; top-level monomorphic `fn`; `Int`, `Bool`, `String`; `let`;
arithmetic and comparison; user ADTs; `match` with constructor, literal and
wildcard patterns plus guards; calls; `|>` and `_`.

**Out:** effects, rows, generics, typeclasses, closures, records, cross-package
imports.

- **2.1 `khora-hir`.** Module graph, name resolution (per D2), pipe and
  placeholder desugaring, `match` to a decision tree, body lowering with stable
  IDs. All as salsa queries.
- **2.2 `khora-types`.** Monomorphic checking plus exhaustiveness and
  reachability over the decision tree.
- **2.3 `khora-perceus`.** Uniform boxed representation, `dup`/`drop` at scope
  boundaries. Reuse analysis deferred to Phase 6.
- **2.4 `khora-rt`.** New crate: allocator shim, RC header, `khora_alloc`,
  `khora_dup`, `khora_drop`, `print`. Static library.
- **2.5 `khora-codegen-llvm`.** HIR plus RC ops to LLVM IR to an object, linked
  against `khora-rt`.
- **2.6 Diagnostic harness (A7).** Snapshot tests over rendered diagnostics, so
  message quality is a tracked regression surface from the first error the
  compiler can emit — not a Phase 8 cleanup.

**Exit:** `khora build examples/core_demo` produces an executable that runs,
prints the expected output and exits 0; a counting-allocator test asserts every
allocation is freed; diagnostic snapshots are committed and reviewed.

---

## Phase 3 — Generics, HKT and typeclasses

Algorithm W with occurs check and let-generalisation, extended with a kind
system. Const generics as `Type::Const`. Typeclasses with instance resolution
per D6. Monomorphise in HIR before codegen so abstraction costs nothing at
runtime.

**Exit:** `matmul` with a mismatched shared dimension is a compile error naming
both dimensions; a `traverse` written once works over `Option`, `List` and a
user type; instance resolution errors name the missing instance.

---

## Phase 4 — Effect rows and handlers

- **4.1 Decide D1** → `docs/design/effect-runtime.md`: one-shot versus
  multi-shot continuations, and how handler frames interact with Perceus.
- **4.2 Scoped row polymorphism.** `Type::Row(Fields, TailVar)`, unification
  with field reordering and tail extension, row subtraction for handler
  installation, and the empty-row obligation on the entrypoint. Settle D3.
- **4.3 Handler lowering and runtime**, per D1.
- **4.4 `Layer` as handler composition**, including merge.

**Exit:** the reference application typechecks and serves a request; an
unhandled capability is rejected with a diagnostic naming the absent label and
the function that required it.

---

## Phase 5 — Structured concurrency

Fibers, cancellation that runs finalizers, `Scope`-bound resource lifetimes,
`Schedule` policies. Shares continuation machinery with Phase 4.

**Exit:** a cancelled fiber runs every finalizer in scope, verified by test;
`khora test` runs isolated fibers across cores.

---

## Phase 6 — Perceus reuse and FBIP

Reuse analysis, drop specialisation, borrowed parameters.

**Exit:** `map` over a uniquely-owned list performs zero allocations, asserted
by a counting-allocator test.

---

## Phase 7 — Rust interop

Per A6 and D8: consume crates.io from Khora. The hard part is mapping Rust's
ownership and trait system onto reference counting and rows at the boundary.

**Exit:** an HTTP server with real TLS and JSON, built on Rust crates through
the interop boundary.

---

## Phase 8 — Toolchain and platform

Ordered by value, not by §6's numbering.

- **8.1 Linter** (needs types): unused capability, dangling pure expression,
  redundant match arm.
- **8.2 `khora test` / `khora bench`**: snapshots with `--update-snapshots`,
  P50/P95/P99.
- **8.3 `khora-pkg`**: `khora.lock` with SHA-256 hashes, content-addressed cache,
  DAG task runner.
- **8.4 LSP** over the salsa database: diagnostics, hover, completion, capability
  inlay hints, rename.
- **8.5 Cross-targets**: `x86_64-unknown-linux-musl`, `aarch64-apple-darwin`.
- **8.6 WASM build plugins** via wasmtime. Last: largest scope, least critical,
  and it needs D4 settled first.

Note that A7 pulls the *quality* of diagnostics and LSP latency forward into
Phases 2 and 3. What remains here is surface area, not standards.
