# Production Release Readiness

This document is the release gate for the first public Khora release.

It is intentionally stricter than “the compiler works” or “trusted developers can try it.” A public release means a developer with no prior knowledge of the project can discover Khora, understand why it exists, install a supported build, write and debug a nontrivial program, use the core production facilities, deploy it on a supported target, and report a problem without requiring direct help from the language's author.

The first public release may still be `0.x`. It does not need a mature package ecosystem, 1.0 stability, or proven Fortune 100 adoption. It does need an honest, coherent, supportable product boundary.

A section is complete only when its behavior is implemented, documented, tested, and exercised through the public surface. A roadmap heading that says “done” does not override a known semantic gap listed here.

---

## Current state

Scored against the tree on 2026-08-31, item by item, against what is in the
repository rather than against the roadmap's account of itself. **151 of 222**,
and re-scored whenever a section moves.

**A score is only as good as the reading behind it.** Section 3 was scored at
2/6 by somebody who had not opened `crates/khora-codegen-llvm/tests/db.rs`, and
two of its items were already satisfied. Section 15 was scored at 1/8 the day
#149 wrote the seven files it asks for. Section 13 had three items unticked that
an existing end-to-end test already discharged, and section 17 had two that one
`npm run build` would have settled. All four were understatements,
which is the safe direction for the rule below to fail in, and none was a
finding.

An item is ticked only when it was checked. Where something is partly done it
stays unticked and carries a **Left:** note saying what remains, because a
half-done gate item is a gate item. Ticks that read *vacuously* mean the
requirement is satisfied by not making the claim — no wasm target is
advertised, so no wasm deployment has to work.

| Section | Done |
| --- | --- |
| 1. Language and compiler correctness | 13 / 19 |
| 2. Runtime soundness and structured concurrency | 9 / 16 |
| 3. Resource, database and cancellation semantics | 4 / 6 |
| 4. HTTP, overload and server behavior | 5 / 10 |
| 5. Observability | 4 / 7 |
| 6. Database ecosystem proof | 6 / 7 |
| 7. Cross-compilation and deployment | 4 / 11 |
| 8. FFI and C interoperability | 4 / 8 |
| 9. Traps, debugging and production diagnosis | 3 / 7 |
| 10. Compiler performance and scale | 1 / 6 |
| 11. Tooling and editor experience | 6 / 10 |
| 12. Installation, toolchains and release artifacts | 6 / 9 |
| 13. Package ecosystem | 7 / 7 |
| 14. Supply chain and security | 4 / 7 |
| 15. Compatibility, governance and contribution policy | 8 / 8 |
| 16. Public documentation | 44 / 46 |
| 17. khoralang.com production documentation site | 7 / 12 |
| 18. Reference applications and end-to-end proof | 6 / 6 |
| 19. External-user validation | 0 / 5 |
| 20. Public positioning and benchmark integrity | 3 / 7 |
| 21. Release automation and final gate | 4 / 8 |

**What the shape of this says.** The two halves of the product are not at the
same stage. Documentation (§16), tooling (§11) and the release machinery
(§12, §21) are largely there — installers with checksums, three tagged
candidates, a docs site built from this tree, a generated standard-library
reference the gate keeps honest. What is thin is everything that proves the
product to somebody who is not the author: external validation (§19) has not
started, and the public site's versioning (§17) is deliberately deferred until
there is a second version to be addressable *from*. Governance
and compatibility policy (§15) were the same kind of gap until #149, and are the
cheapest section on this page to have left undone for as long as it was.

The runtime and compiler sections in between are the ones to read carefully.
They are not thin — they are *partly* proven, and the unproven parts are
concentrated in the same place: the formal `unsafe` inventory (§2), cancellation
cleanup for files, sockets, TLS and processes (§3, where the database half is now
done and the rest has no test at all), and compiler performance at a scale the
corpus does not reach (§10). The largest reference application was about 460 lines, which
was the single fact behind three unticked items in §18; `examples/khq` is about
3,600 and closes all three.

---

## 1. Language and compiler correctness

- [ ] Phase 12 is complete, including all implementation work that remains in its entries rather than only the currently landed subset. **Left:** #140's remainder is Phase-12-shaped: `khora run file.kh` runs the package's main, an unused *type* import warns nothing, and `src/bin` is enforced by a lint without multiple binaries being implemented.
- [x] Every known silent-miscompile, silently ignored annotation, unresolved-name hole, and misleading diagnostic discovered during Phase 12 has either been fixed or promoted to a release-blocking issue. **Done:** #143 fixed (errata 62); #142 and #108 are tracked and listed under Known limitations.
- [ ] The compiler rejects unresolved type names, unresolved trait bounds, contradictory annotations, and unsupported constructs at the source location that caused the problem. **Left:** An unresolved name renders identically to the real type — ``expected `X`, found `X``` beside "cannot find type `X`".
- [x] Type inference and lowering have regression coverage for closures, generics, traits, effect rows, handlers, capabilities, higher-kinded types, ADTs, pattern matching and annotations. **Done:** 2,107 tests; `khora-types/tests` and `khora-codegen-llvm/tests` carry a file per feature.
- [x] Common invalid programs produce diagnostics that describe the programmer's problem rather than an internal compiler phase.
- [x] A deliberate invalid-program corpus tests diagnostic text, ranges and recovery for common mistakes. **Done:** `crates/khora-diagnostics/tests` and `khora-codegen-llvm/tests/errors.rs`.
- [x] The formatter is stable enough that a public project can use `khora fmt` in CI without routine semantic churn. **Done:** `khora fmt --check` runs over `std` and all ten corpus members in `scripts/baseline.sh`.
- [ ] The linter's supported checks are documented, deterministic and free of known high-confidence false positives. **Left:** No public lint reference page. Levels are configurable in `khora.toml` and that table is undocumented outside this repository.
- [x] The language's grammar, precedence and user-visible semantics have one canonical public reference. **Done:** `/docs/reference/grammar`, `/lexical-structure`, `/expressions`.
- [x] A `Char` type and a character-boundary string API exist, or their absence is recorded as a deliberate limitation with the byte-oriented alternative documented. **Done:** `Char` is a builtin scalar written `'a'`; `is_char_boundary`, `next_boundary`, `previous_boundary`, `char_at`, `chars` and `char_length` are the API that makes `String::slice` safe to reach for.
- [ ] `attempt` discharges a `raises` row holding more than one error type, or the one-type limit is documented and `catch` is presented as the way to handle a wider row. **Left:** `attempt<A, E, 'ef>` takes a single `E`, so a body raising `HttpError + ChildFailed` cannot go through the documented way to turn a failure into a value.
- [ ] A diagnostic never renders two different types with the same text. **Left:** two same-named types from different modules both print as their bare name — ``expected `Entry`, found `Entry```.

### Decimal

- [x] Exact decimal literal syntax is complete (`0.01d` or the final equivalent), documented and tested alongside ordinary floating-point literals. **Done:** `0.01d`, documented in `/docs/reference/lexical-structure`.
- [x] Decimal arithmetic has adversarial coverage for large magnitudes, large scale differences, negative values, rescaling, equality, ordering, addition, subtraction, multiplication and division.
- [x] Intermediate calculations cannot overflow merely because two representable Decimal values need scale alignment; where a wider intermediate is required, the implementation uses one or rejects the operation deliberately. **Done:** 128-bit significand; roadmap 13.x widened it for exactly this.
- [x] Rounding behavior, overflow behavior, parsing and formatting are specified rather than inferred from tests. **Done:** `/docs/stdlib/api/decimal` — "What it does when it cannot answer".

### Time

- [x] `Date`, `Time`, `DateTime`, `Offset` and instant/clock concepts have public documentation that clearly separates wall time from an instant. **Done:** `/docs/stdlib/api/time`.
- [x] The supported calendar range, overflow behavior and invalid-date behavior are specified.
- [ ] Time-zone database support, if provided by a package rather than `std`, has a documented integration path and is not implied to be built into `std`. **Left:** No time-zone story is written down either way; `std::time` is offset-only and the page does not say what a zone would look like.

---

## 2. Runtime soundness and structured concurrency

Khora's runtime is part of the language contract. The release cannot rely on “works in ordinary tests” for ownership, cancellation or fiber migration.

- [x] The M:N scheduler is a supported default runtime path rather than an experimental mode that ordinary users are expected to opt into manually. **Done, by settling the question rather than by switching the default:** threads are 0.1.0's default and the scheduler is a documented, supported opt-in. The argument, the measurements and their limits are in `docs/design/fibers.md`; the user-facing half is in `/docs/reference/concurrency` and `/docs/limitations`. A program cannot observe which it has, so this is not a compatibility commitment.
- [ ] The remaining scheduler/I/O work has been measured after Phase 12 and either completed or explicitly shown not to justify further architecture work before release. **Left:** it cannot be measured yet -- #160. At 320 connections neither fiber implementation reaches a ceiling and the same configuration varies 1.85x between sittings, so no throughput claim about either is available.
- [ ] Native scalable I/O backends are present for the platforms claimed as production-supported where the existing portable backend would otherwise impose a known scaling ceiling. **Left:** `WSAPoll` on Windows and `poll` elsewhere; no epoll/kqueue/IOCP backend.
- [ ] The scheduler passes sustained soak and adversarial tests across supported platforms. **Left:** Soak tests exist (`khora-rt/src/soak.rs`) and pass, but #108 is an uncaught intermittent failure in the Linux repeat loop.
- [x] Fiber cancellation always permits required finalizers/resource cleanup to run. **Done:** `tests/fibers.rs`: a cancelled fiber runs every finalizer and stops only itself.
- [x] Nursery semantics are complete: a failing child has the documented effect on siblings and the parent, with typed failure behavior tested. **Done:** #139: the first failure cancels the siblings, every child is waited for, and the nursery raises `ChildFailed`.
- [ ] No language-visible behavior depends on a fiber staying on one OS thread unless the program explicitly enters a documented thread-affine FFI boundary.
- [x] Safepoints and cancellation points remain distinct and are documented as such. **Done:** `/docs/reference/concurrency`; `a_loop_in_an_infallible_function_is_not_a_cancellation_point` pins it.
- [ ] Every runnable `Task` has exactly one owner at every instant; wake tokens or backend state never create a second owner.
- [x] Lost-wakeup regressions cover registration during backend wait, cancellation during I/O wait, injected runnable work while a worker is in the backend, and shutdown. **Done:** `khora-rt/src/scheduler.rs` tests cover all four.

### Formal unsafe/soundness review

- [x] Every `unsafe` block and `unsafe impl` in the runtime/compiler boundary is inventoried. **Done:** 282 blocks, every one carrying an argument, and `scripts/no-bare-unsafe.sh` in the gate so the count cannot drift -- it was 41 short when this was measured, having been 28 short at the audit that wrote `docs/design/soundness.md`.
- [ ] Each inventory entry names the invariant that makes it sound and the test or argument that protects the invariant. **Left:** every block now names its invariant; what is not systematic is the second half -- *which test* protects it. The load-bearing ones say so (`#[inline(never)]` on `current::running` names the test that caught its removal); most do not.
- [x] `unsafe impl Send for Task` and equivalent cross-thread/coroutine state are reviewed explicitly. **Done:** three impls -- `Task`, `Migrating`, `Handed` -- each reviewed in `docs/design/soundness.md`, with the residual obligation on Rust bodies named.
- [x] TLS/thread-local state is audited under fiber migration. No thread-local address may survive across a suspension unless the design explicitly proves it safe. **Done:** 46 `.with(..)` closures in `khora-rt`, none containing a suspension, so no reference outlives one; `CURRENT` is the one read by address and is held by `#[inline(never)]` plus the switch's memory clobber, with the test that caught its removal named.
- [ ] FFI pointers, callbacks and thread-affine handles have a documented lifetime/thread rule.
- [x] Sanitizer and dynamic-analysis coverage appropriate to the implementation is run before release; unsupported analyses and their blind spots are documented. **Done:** `scripts/tsan.sh` under WSL2; the blind spots are recorded in the script's own header.

---

## 3. Resource, database and cancellation semantics

- [x] `Region`/finalizer behavior is reliable under success, typed failure, cancellation and trap boundaries where cleanup is permitted. **Done:** and `Region::open`'s own documentation now says *which* scope ends it — the enclosing block, established by experiment rather than by reading, because the difference between a lease that ends with the call and one that ends with the caller is what made a pool of `n` behave like a pool of `n` uses.
- [x] `std::db::transaction` rolls back not only when its body returns an error but when its fiber is cancelled. **Done:** `a_cancelled_fiber_rolls_back_and_does_not_commit`. This was already true when the section was scored and was left unticked because it had not been looked at, which is the scoring rule working rather than a finding.
- [x] Transaction tests assert begin/commit/rollback ordering for success, typed failure, cancellation, commit failure and rollback failure policy. **Done:** eleven cases in `crates/khora-codegen-llvm/tests/db.rs`, each asserting an exact transcript rather than a count, so any permutation fails. The rollback-failure policy — discard it, because the engine's complaint about a rollback is a worse thing to report than the reason the rollback was needed — was a deliberate `let _ =` with no test until now.
- [ ] Database cancellation does not leave a pooled connection holding an open transaction or locks. **Left:** The ordering is proved — `a_cancelled_lease_is_returned_only_after_the_rollback` asserts the rollback reaches the engine before the lease reaches the idle channel, which is the whole of what `packages/postgres` relies on and was untested. What remains is the case where that rollback *fails*: nothing is told, and the connection is reused anyway. #161.
- [ ] File, socket, TLS and process resources have cancellation tests that prove cleanup rather than merely absence of a crash. **Left:** Zero cancellation tests across all four. `std::fs` and `std::net::tls` do register releases, so only the proof is missing; sockets and processes register none. #161 has the reconnaissance.
- [x] Bounded concurrency primitives are documented as the default way to protect externally driven resources. **Done:** `/docs/cookbook/bounded-concurrency`, and `bounded_nursery`'s own documentation.

---

## 4. HTTP, overload and server behavior

Peak requests per second alone is not a release gate. A production runtime must remain healthy when offered work exceeds sustainable throughput.

- [ ] The HTTP server has a documented distinction between connection capacity and actively executing/request-processing capacity.
- [ ] The current connection/nursery limits are intentionally tuned for scheduled fibers rather than inherited from the old OS-thread implementation. **Left:** 256 is documented as "a working number rather than a tuned one".
- [x] Sustained overload tests cover at least 100%, 125% and 200% of sustainable offered load. **Done:** `khora-codegen-llvm/tests/load.rs`: overload, recovery and shutdown.
- [ ] Under overload, RSS remains bounded within the configured operating model.
- [x] Runnable queues and admission queues remain bounded or have explicitly documented limits. **Done:** `bounded_nursery` turns the ceiling into backpressure; the listening backlog absorbs the rest.
- [x] Latency degrades predictably instead of entering overload collapse. **Done:** `overload_becomes_latency_rather_than_loss`.
- [ ] Controlled rejection uses appropriate HTTP semantics such as 503 for service saturation and 429 for policy/rate limits where relevant.
- [x] The service recovers promptly after offered load falls. **Done:** `a_service_recovers_after_the_burst`.
- [ ] Slow, half-open and maliciously quiet connections cannot occupy unbounded server resources.
- [x] The supported HTTP feature set is documented honestly, including any remaining body-size, transfer-encoding, WebSocket or HTTP/2 limitations. **Done:** `/docs/limitations` names the HTTP surface limits.

---

## 5. Observability

- [x] Trace context is carried automatically across fiber spawn, scheduling, stealing, wake and cancellation according to the documented model. **Done:** `tests/trace.rs`: the sampled flag is carried.
- [x] W3C `traceparent` parsing/formatting is covered by conformance-style tests.
- [x] A no-op tracer remains cheap enough that disabled tracing is a viable production configuration. **Done:** `the_default_tracer_records_nothing_and_stays_out_of_the_way`.
- [ ] At least one real exporter/integration exists, preferably OTLP/OpenTelemetry, outside `std`. **Left:** None. `std::trace` says explicitly that OTLP is not `std`'s job, and nothing outside `std` provides one.
- [ ] A reference service demonstrates an incoming HTTP trace flowing through application work, spawned fibers and database operations with correct parent/child relationships.
- [ ] Logging guidance explains how logs correlate with traces and fiber/request context.
- [x] Metrics/exporter responsibilities are clearly separated between `std` vocabulary/runtime context and external packages. **Done:** `/docs/stdlib/api/trace` — "Why this is `std`'s and the exporter is not".

---

## 6. Database ecosystem proof

The neutral `Db` capability is not enough by itself to prove the production database story.

- [x] At least one production-grade database package exists; PostgreSQL is the preferred first proof. **Done:** `packages/postgres`, tested in the gate.
- [x] The package exercises network I/O, pooling, query execution, result decoding, cancellation and transactions through public Khora APIs.
- [x] Pool saturation is bounded and documented.
- [x] Database numeric types preserve exact values; `NUMERIC`/money-like values do not silently pass through `Float`. **Done:** `numeric` decodes to `Cell::Money(Decimal)` and `packages/postgres/src/conn_test.kh` holds the scale against a trailing zero, keeps a value too wide for the significand as the server's own digits rather than a truncation, and pins `float4`/`float8` as `Text` — `Cell` has no float variant and this is where that is enforced.
- [x] Schema/type mismatches are visible rather than silently coerced. **Done:** `Cell::text`/`number`/`money`/`flag` answer `None` for the wrong variant rather than rendering it, tested by `cells_do_not_coerce`; and a column whose text does not match its OID stays `Text` instead of being guessed at, tested in `conn_test.kh`.
- [ ] Connection and transaction failure behavior is tested under cancellation and network loss.
- [x] A reference application uses the package rather than a test-only handler. **Done:** `examples/ledger_service` depends on `packages/postgres` and the gate builds it.

SQLite or additional engines are useful but not required for the first public release if the package story and `Db` abstraction have already been validated by a serious driver.

---

## 7. Cross-compilation and deployment

A target is “supported” only when the toolchain produces something users can actually run or deploy. Object emission alone is not target support.

- [x] The public supported-target matrix distinguishes code-generation support, build/link support and production-supported deployment targets. **Done:** `/docs/deployment/supported-targets` defines supported / experimental / emission-only and lists no triples yet, which is the honest state.
- [x] Cross-built `khora-rt` artifacts exist for every target advertised as buildable. **Done:** Vacuously: no target is advertised as buildable yet.
- [ ] Required linker/sysroot assets are obtained automatically or through a documented, repeatable installation path.
- [ ] At least Linux x86-64 and Linux arm64 have end-to-end build-and-run validation if they are listed as supported. **Left:** x86-64 Linux is validated through WSL2 in the gate and through CI; arm64 is not.
- [ ] Static/musl/container deployment is either supported and tested or explicitly excluded from the first release. **Left:** `/docs/deployment/containers` exists; whether it is tested is not recorded.
- [x] Cross-platform CI builds the same release-facing examples used in documentation. **Done:** `.github/workflows/ci.yml` runs the backend job on ubuntu, macos and windows.

### WebAssembly / Cloudflare

- [ ] `wasm32-unknown-unknown` has its own correct platform/std surface and does not inherit Linux sockets or filesystem bindings.
- [ ] The no-fibers wasm execution model is explicit, tested and documented until native wasm stack switching becomes a supported runtime basis.
- [ ] Host-provided networking/filesystem/database capabilities are modeled intentionally rather than emulated through nonexistent Unix APIs.
- [ ] A real Cloudflare deployment example builds and runs from the public toolchain.
- [x] “Cloudflare Workers support” is not claimed until the deployment example works end to end. **Done:** `/docs/deployment/supported-targets` and `/cloudflare` describe it as the motivating target, not as shipped.

---

## 8. FFI and C interoperability

- [x] Phase 12's C export/import surface is complete enough for a small Khora library to be called from an ordinary C-compatible consumer. **Done:** `tests/exporting.rs` builds a C host with clang and runs it.
- [x] The supported C ABI types and ownership rules are documented precisely. **Done:** `/docs/reference/ffi`; the "only scalars and pointers cross" rule is enforced in the backend.
- [ ] Strings, buffers, records/structs and error results have an explicit allocation/freeing contract.
- [ ] Thread-affine foreign libraries are tested against fiber migration rules.
- [ ] Blocking FFI calls have a documented interaction with the scheduler/blocking pool.
- [ ] Foreign callbacks into Khora either have a supported contract or are explicitly unsupported.
- [x] FFI failures cannot silently cross a boundary in an ABI-undefined representation. **Done:** `foreign_signature_obstacle` refuses at the call site with the rule quoted.
- [x] At least one external-language integration test (for example Python, Node or a small C host) validates the public C surface. **Done:** `tests/exporting.rs` compiles and runs a C host against the exported surface.

---

## 9. Traps, debugging and production diagnosis

- [x] Debug information is emitted for supported native targets with source file and line mappings. **Done:** DWARF line tables; `tests/debugging.rs`.
- [ ] A documented LLDB/GDB workflow can set breakpoints and inspect ordinary Khora stack frames where supported. **Left:** No debugger page exists anywhere under `website/content/docs/`.
- [x] Runtime traps identify the Khora source location that triggered them. **Done:** `a_bounds_failure_says_which_line_indexed`.
- [ ] Stack traces are meaningful enough to diagnose a production failure rather than exposing only runtime/compiler internals.
- [x] The policy for overflow, bounds failure and other unrecoverable bugs is explicitly documented. **Done:** `/docs/reference/traps`.
- [ ] The Phase 12 trap-containment decision is complete: it is clear whether a trap terminates a fiber, a request boundary or the whole process, and why.
- [ ] If some traps deliberately terminate the process, server guidance explains the operational consequence rather than pretending request isolation exists.

---

## 10. Compiler performance and scale

- [ ] Build-time measurements use a release-built Khora compiler before public performance claims are made.
- [ ] The corpus includes at least one substantially larger application than the current small reference programs. **Left:** The largest reference application is a few hundred lines; the item asks for several thousand.
- [ ] Cold build time, warm/repeated developer workflow, peak compiler memory, monomorphization cost and link time are measured separately enough to identify regressions.
- [ ] Whole-program monomorphization is tested at a size capable of exposing superlinear behavior.
- [ ] A documented budget/regression baseline exists for future compiler changes.
- [x] Any public comparison to Rust/Go/another compiler uses equivalent workloads and records tool versions/hardware. **Done:** `bench/README.md` records hardware, versions and method, and says a number only travels within one sitting.

---

## 11. Tooling and editor experience

- [x] `khora build`, `khora check`, `khora test`, `khora fmt` and package/toolchain commands work through one documented CLI without repository-internal invocation knowledge. **Done:** One CLI; `khora --help` covers it and the getting-started path uses nothing else.
- [x] The LSP provides reliable diagnostics, hover and go-to-definition at minimum. **Done:** Measured over the protocol: 15 capabilities, diagnostics including missing-import, hover and definition.
- [x] Completion is good enough for ordinary standard-library and project symbols. **Done:** 34 items for `List::` over the wire.
- [x] Formatting integrates with the editor and CI.
- [x] A maintained VS Code extension or an equivalently accessible editor integration exists for the first public audience. **Done:** `editors/vscode`, built by `.github/workflows/extension.yml`, tagged `vscode-v0.3.0`.
- [ ] Syntax highlighting covers the complete current grammar. **Left:** Not audited against the grammar since the grammar last changed.
- [x] The editor extension and compiler report their versions in bug reports/repro instructions. **Done:** The status bar runs `khora toolchain which` and shows the answering toolchain and its reason.
- [ ] The language's MCP support is documented as optional tooling rather than required to write correct Khora.
- [ ] `khora doc` works in an ordinary user package rather than only over `std`.
- [ ] A package may declare more than one executable, or the `src/bin` convention the linter enforces is documented as the only supported shape. **Left:** `misplaced-main` enforces the convention and multiple binaries per package are not implemented.

---

## 12. Installation, toolchains and release artifacts

“Clone the compiler repository and run Cargo” is not the public installation story.

- [ ] A tagged public release exists with an explicit semantic version such as `0.1.0`. **Left:** Three release candidates are tagged (`v0.1.0-rc.1` … `rc.3`); no final `v0.1.0`.
- [x] Supported platforms have downloadable compiler/toolchain artifacts or a single documented automated installer. **Done:** `install.sh` / `install.ps1`; `.github/workflows/release.yml` packages on three OSes.
- [x] Artifacts include checksums. **Done:** The installer verifies against the published checksum.
- [ ] `khora --version` identifies the exact compiler release and enough build metadata for bug reports. **Left:** It prints `khora 0.1.0` and no build metadata — no commit, no date, no target triple.
- [x] Projects can pin a compiler version and obtain it without manually linking a locally compiled checkout. **Done:** `[toolchain]` in `khora.toml`; the shim hands over before argument parsing.
- [x] A missing pinned compiler fails loudly rather than silently substituting another version. **Done:** `khora toolchain which` reports it and the editor status bar shows it as a warning.
- [ ] Release notes and a changelog describe breaking language/std/tooling changes. **Left:** No `CHANGELOG.md` in the repository.
- [x] The fresh-machine installation path is tested in CI or release validation. **Done:** `release.yml` compiles a program with the packaged artifact before attaching it.
- [x] Installation instructions never require knowledge of the Rust implementation unless building Khora itself. **Done:** `/docs/getting-started/installation` mentions a linker and never Cargo.

---

## 13. Package ecosystem

A large registry is not required, but dependency use must be coherent and reproducible.

- [x] Public documentation explains dependency declarations, exact resolution behavior, lockfiles and the content-addressed store. **Done:** `/docs/guide/modules-and-packages`.
- [x] A developer can consume a third-party package without repository-specific manual setup. **Done:** `a_package_from_a_git_repository_is_resolved_compiled_and_run` fetches a package from a repository outside the build, compiles the application against it and runs it — a generic, a method and an impl of a `std` trait all crossing the boundary. `khora build` resolves what it needs, so there is no fetch step to remember, and `khora install <url>` writes the manifest entry after checking the package's real name and whether it offers itself at all.
- [x] The policy for source packages versus binary artifacts is explicit. **Done:** `/docs/guide/modules-and-packages` — dependencies are source, fetched and compiled, and there are no binary artifacts to publish or to trust.
- [x] Version/compatibility expectations for packages are documented even if full version solving is deferred. **Done:** the same page says there is no registry, so `version = "…"` has nothing to resolve against; `git` for what you did not write and `path` for what you did; and a branch name resolves to the commit it pointed at, so `rev = "main"` is a convenience when the dependency is added rather than a moving target afterwards.
- [x] The first-party packages used by reference applications are published/consumable through the same mechanism available to users. **Done:** `packages/postgres` was consumed from a project outside this repository by `git` + `subdir`, compiled and run. `a_package_in_a_subdirectory_of_a_larger_repository_compiles_and_runs` keeps that shape honest — a package three directories inside a checkout whose root is a different, unpublished package, which is this repository's layout and most repositories that hold a library. The earlier note read the reference application's path dependency as evidence about the mechanism; inside one repository a path dependency is the correct choice, and it says nothing about whether a user can fetch the package.
- [x] Package integrity is verified from the lockfile/store as documented. **Done:** every resolution hashes what arrived and refuses the build if it disagrees with the lockfile — `resolve.rs`, tested by the tampering case in `khora-pkg`'s own tests, and now said on the guide page so that "as documented" is true as well.
- [x] If no public registry exists at first release, that limitation and the supported git/package workflow are prominent rather than hidden. **Done:** `/docs/limitations` — "Package ecosystem".

---

## 14. Supply chain and security

- [x] `SECURITY.md` defines how vulnerabilities should be reported privately.
- [ ] Release artifacts have provenance/signing or the chosen equivalent appropriate to the release infrastructure.
- [ ] An SBOM can be produced for the compiler/toolchain and, where practical, Khora application dependencies.
- [ ] Package hashes and lockfile guarantees are documented in security terms rather than only implementation terms.
- [x] CI/release credentials and publication flow do not require a developer's local workstation to be the root of trust. **Done:** `release.yml` runs on tag and uploads with `gh`; nothing is published from a workstation.
- [x] Dependencies used to build release artifacts are pinned/reproducible to the extent claimed. **Done:** Actions are pinned by commit SHA; LLVM is pinned to 22.1.8.
- [x] The `[permissions]` model is described accurately: compile-time authority control is not presented as a runtime sandbox unless a real sandbox is implemented. **Done:** `/docs/stdlib/api/permissions` says it is compile-time authority, not a sandbox.

---

## 15. Compatibility, governance and contribution policy

A `0.x` release may break. It may not be ambiguous about when and how it breaks.

- [x] A public compatibility policy defines guarantees for compiler releases, source syntax, `std`, lockfiles and packages before 1.0. **Done:** `/docs/reference/compatibility`, with a table of what counts as breaking and what does not.
- [x] The policy states what 1.0 is waiting for. **Done:** four things, none of them a feature — a bug-discovery rate that has flattened, the soundness review finished, the scheduler measured on Linux, and use by people who did not write it.
- [x] Breaking releases provide migration notes. **Done:** `CHANGELOG.md` puts every breaking change under a **Breaking** heading before anything else and names the mechanical fix where one exists, and a change that made a program *silently wrong* is listed as breaking as well as fixed. Written down and applied to the entries that exist; no breaking release has yet exercised it.
- [x] The project defines how language changes are proposed and accepted. **Done:** `CONTRIBUTING.md` § Before a change and § Governance — in the issue thread, before the code, recorded in `docs/roadmap.md` or a design document.
- [x] The boundary between `std`, first-party packages and third-party ecosystem packages is documented. **Done:** `/docs/stdlib/index` and `std::trace`'s own argument; `docs/design/effect-survey.md` §3.2 is the rule.
- [x] `CONTRIBUTING.md` explains build/test expectations and the review path for compiler, runtime, stdlib and documentation changes. **Done:** § Building it, § The gate — the whole 25-minute `scripts/baseline.sh` and what each step is for — and § Review, which names the four questions in the order they get asked.
- [x] Maintainer/governance responsibility is explicit even if one person remains final decision-maker. **Done:** "One maintainer, final say, no committee", in `CONTRIBUTING.md` and on the compatibility page, with the undertaking that a change to that arrangement is written down before it is true elsewhere.
- [x] The public project communicates a credible maintenance plan and does not imply organizational backing that does not exist. **Done:** the compatibility page states the bus factor plainly rather than dressing one person as a committee.

---

## 16. Public documentation

All public documentation lives under `website/content/docs/`; repository-internal design documentation remains under `docs/`.

### Getting Started

- [x] Install Khora on a clean supported machine.
- [x] Create a project.
- [x] Build and run it.
- [x] Run tests.
- [x] Add a dependency.
- [x] Use editor integration.
- [x] The complete path can be followed in roughly one sitting without private project knowledge.

### Language Guide

- [x] Values, bindings and functions.
- [x] Modules/imports and packages.
- [x] Records, tuples and algebraic data types.
- [x] Pattern matching and destructuring.
- [x] Collections and strings.
- [x] Pipelines and call syntax.
- [x] Generics, traits/typeclasses and higher-kinded abstractions at the level ordinary users need.
- [x] Typed failure and `raises`.
- [x] Effects, handlers and `with` capabilities.
- [x] Resource scopes/regions and finalization.
- [x] Fibers, nurseries, cancellation and bounded concurrency.
- [x] Shared state and the rules for crossing fiber boundaries.
- [x] Testing and common project structure.

### Language Reference

- [x] Grammar and lexical rules.
- [x] Precedence and associativity.
- [x] Type system and inference rules at a precise user-facing level.
- [x] Effect/failure/capability row semantics.
- [x] Trait/typeclass lookup/import rules.
- [x] Pattern/exhaustiveness behavior.
- [x] Memory/resource semantics users can observe.
- [x] Concurrency and cancellation semantics.
- [x] FFI and trap behavior.

### Standard library

- [x] Searchable API documentation exists for public `std` modules and exported symbols. **Done:** 21 generated pages under `/docs/stdlib/api/`.
- [x] API docs are generated or validated from the source of the corresponding compiler release so they cannot drift silently. **Done:** `khora doc std --check` fails the gate when a page is stale.
- [ ] Important APIs include examples, not only signatures. **Left:** Coverage is uneven; nothing checks that an exported item has an example.
- [ ] The linter's checks have a public reference page listing each check, its default level and how to configure it in `khora.toml`.

### Cookbook

- [x] HTTP service.
- [x] JSON API.
- [x] Database transaction.
- [x] Bounded concurrency/backpressure.
- [x] Cancellation-safe resource use.
- [x] Tracing/observability.
- [x] Configuration/environment access.
- [x] Testing an effect/capability with a handler/test double.
- [x] Deployment to at least one native target. **Done:** `/docs/deployment/linux`.
- [x] Deployment to Cloudflare if wasm support is part of the release claim. **Done:** Vacuously: wasm is not part of the release claim.

### Migration/on-ramp guides

- [x] Khora for TypeScript/Effect developers.
- [x] Khora for Go developers.
- [x] Khora for Rust developers.

These guides should translate mental models, not market against other languages.

---

## 17. khoralang.com production documentation site

`khoralang.com` is the canonical public home for the language.

- [x] The site is built from the repository's `website/` tree.
- [x] Deployment through Cloudflare is reproducible from CI rather than dependent on an author's workstation. **Done:** `.github/workflows/docs.yml` runs `npm run deploy`.
- [x] The deployed site records the Git revision/release it was built from. **Done:** every page's footer carries the release and the commit, linked to that commit on GitHub. `scripts/sync-docs.mjs` writes it from `GITHUB_SHA` where CI supplies one and from `git rev-parse` otherwise, and leaves it out entirely when neither can answer — a footer saying it was built from `unknown` has spent a line saying nothing.
- [ ] Release documentation is versioned and remains addressable after newer releases ship. **Left:** Deliberately, and the decision is written down in `docs/design/docs-urls.md`. Versioned paths solve a problem that needs two versions to have, and building the machinery now would mean choosing between a branch per release and a directory per release without the evidence that decides it — how often documentation is fixed *after* a release. What is promised instead is that every page says which commit it came from, which is the half a reader needs today.
- [ ] `/docs/` points at the current stable release. **Left:** There is no stable release yet, so `/docs/` is the development documentation and the pre-1.0 banner on every page says so. The contract for what happens at 0.1.0 is in `docs/design/docs-urls.md`.
- [ ] `/docs/<version>/` resolves pinned documentation for supported historical releases. **Left:** There are no historical releases. Starts at 0.2.0, per `docs/design/docs-urls.md`.
- [ ] `/docs/next/` may expose development documentation but must be visibly marked unstable. **Left:** The whole site is `next` until 0.1.0 ships, and is marked: `sync-docs.mjs` puts an unstable banner on every page. The path itself starts existing when `/docs/` stops being it.
- [x] Site search covers the language guide, reference and standard library. **Done:** Starlight's Pagefind index, over all 100 pages including the generated `stdlib/api` tree. This was already true when the section was scored — a build prints `Found 100 HTML files` — and was unticked because nobody had run one.
- [ ] Code snippets are syntax highlighted and, where feasible, checked against the matching Khora compiler during the docs build. **Left:** Highlighted; not compiled against the matching toolchain.
- [x] Broken internal links and stale symbol references fail CI. **Done, and it had a bug.** `sync-docs.mjs` has always refused a link that resolves to no route, and refused a link written to a `.md` source file rather than the route it renders as — but it applied the second test before asking whether the link was *external*, so three links to `CONTRIBUTING.md` and friends on GitHub broke the build and the site did not build for a week. Nothing caught it, because CI only runs on a push and this gate did not build the site at all. It does now, as a step of `scripts/baseline.sh`. Stale *symbol* references are the other half and are `khora doc --check`, which is a separate step here.
- [x] The site contains direct paths to installation, releases, documentation, GitHub/source, security reporting and contribution information. **Done:** `/install`, `/guide`, `/reference`, `/stdlib`, `/versioning`, `/limitations`, `/releases`, `/source`, `/security`, `/contributing` and `/changelog`, as redirects in `astro.config.mjs`, and in the footer of every page. They are the ones that get pasted into a chat window, and they survive the pages behind them moving.
- [x] Benchmarks shown publicly link to reproducible methodology rather than presenting context-free numbers. **Done:** `/docs/performance/`, which publishes the methodology and **no numbers at all** — because the load generator is currently the limit and the same configuration does not repeat to within 1.85×. It says which comparisons mean something, how to run them, and the four things that would have to be true before a figure is worth printing.

The frontend framework is not part of the language contract. URL structure, content ownership and versioning are.

---

## 18. Reference applications and end-to-end proof

Before release, Khora must have applications that use the public product rather than compiler-internal shortcuts.

- [x] A polished CLI/data application demonstrates ordinary native use outside HTTP servers. **Done:** `examples/khq`, a query language over JSON — a lexer, a parser, an evaluator over streams and forty builtins, with thirty-four tests of which half are refusals. It reads a file and writes to a terminal and touches no network.
- [x] A production-style HTTP service uses JSON, configuration, typed failures, capabilities, structured concurrency, database access and tracing. **Done:** `examples/ledger_service`: JSON, config, typed failure, capabilities, a nursery, Postgres and tracing.
- [x] If Cloudflare is advertised, an edge/wasm application deploys through the documented public path. **Done:** Vacuously: it is not advertised.
- [x] At least one application is large enough to expose compiler/tooling friction beyond toy examples—preferably several thousand lines. **Done:** `examples/khq` is about 3,600 lines across ten modules, against a previous largest of 460. It earned the item on the way in: a compiler panic on a non-ASCII character beside a `${..}` hole (errata 67), two `std` functions that did not exist (`Float::of_string`, `String::chars_between`), a boundary function whose name invites an infinite loop, a `sort_by` that cannot take a comparator which runs anything, and an `unused-import` lint that is wrong about three separate correct imports (#164).
- [x] Reference applications build using released package/toolchain commands, not repository-only harnesses. **Done:** `khora build` and `khora test`, and #158 proved the package mechanism end to end from outside this repository. A path dependency between two members of one workspace is the right choice inside it and says nothing about what a stranger can fetch.
- [x] CI continuously builds/tests the reference applications against the release candidate. **Done:** `scripts/baseline.sh` builds all four with `--no-cache`, and CI runs it.

---

## 19. External-user validation

Private testing is not a separate product milestone, but public release requires evidence from developers who did not design Khora.

- [ ] Multiple external developers install Khora from the release-candidate instructions without direct coaching.
- [ ] They build a nontrivial program using only public docs/tooling.
- [ ] Installation failures, confusing diagnostics, undiscoverable APIs and documentation gaps found in that exercise are addressed or explicitly documented before release.
- [ ] At least one fresh-machine “stranger test” completes:

  `discover -> install -> new project -> editor -> test -> dependency -> HTTP or CLI app -> debug -> deploy`

- [ ] No step requires unpublished repository knowledge or intervention from the language author.

---

## 20. Public positioning and benchmark integrity

- [ ] The homepage explains in the first screen what Khora is, who it is for and why it exists.
- [ ] The language is presented as general-purpose; finance remains a proving ground rather than the language's identity.
- [x] Claims distinguish shipped functionality from planned functionality. **Done:** `/docs/limitations` and `/docs/deployment/supported-targets` both do this deliberately.
- [ ] Benchmark pages state hardware, operating system, compiler mode/version, workload, connection count, duration, number of runs and control methodology. **Left:** There are no public benchmark pages yet.
- [x] Cross-sitting absolute numbers are not presented as controlled comparisons. **Done:** `bench/README.md` states the rule; nothing public contradicts it.
- [ ] Scheduler performance is described together with latency, memory and overload behavior, not only peak request rate.
- [x] Khora does not market a benchmark as “beats Rust/Go/etc.” when the measurement is load-generator- or machine-limited. **Done:** Nothing public makes the claim.

---

## 21. Release automation and final gate

- [x] CI is green on every production-supported platform. **Done:** ubuntu, macos and windows in `ci.yml`.
- [x] Baseline/compiler tests, runtime stress, HTTP conformance, examples, docs links/snippets and package-resolution tests pass for the exact release candidate. **Done:** `scripts/baseline.sh`: 2,107 tests, conformance, corpus, packages, cache and the Linux runtime through WSL2.
- [x] Release artifacts are produced by automation from the release tag.
- [ ] Documentation deployed to `khoralang.com` is generated from that same release/tag.
- [ ] Checksums/provenance/release notes are published together. **Left:** Checksums yes; provenance and release notes no.
- [x] Known limitations are current and prominent. **Done:** `/docs/limitations`, linked from the docs index.
- [ ] The release candidate has completed the external-user validation above.
- [ ] This document has been scored against the release candidate itself, item by item, and re-scored at every subsequent candidate. A gate nobody scores is a gate nobody passes or fails.

### Definition of public-release ready

Khora is ready for its first public release when a stranger can:

1. understand the language's purpose without reading compiler design records;
2. install a versioned compiler on a supported platform;
3. create, build, test and debug a nontrivial program;
4. use the language's core failure, capability, resource and concurrency model safely;
5. use production-facing facilities such as HTTP, a real database package and tracing;
6. deploy to at least one advertised target through the documented path;
7. obtain version-matched documentation at `khoralang.com`;
8. report a reproducible bug with enough version information for maintainers to investigate it.

If one of those requires direct assistance from somebody who already knows the repository, the public product boundary is not complete yet.

---

## Not required for the first public release

The first public release does **not** require:

- 1.0 source or standard-library stability;
- a large package registry;
- hundreds of third-party packages;
- large-company production adoption;
- perfect feature parity across Windows, Linux, macOS and every wasm/WASI environment;
- every editor to have first-party integration;
- peak HTTP throughput equal to the legacy OS-thread implementation;
- every database engine or observability vendor;
- an editions mechanism before there is evidence that one is needed.

Those are maturity/ecosystem goals. The first release gate is that the language and the boundaries it *does* advertise are real, reliable, documented and independently usable.
