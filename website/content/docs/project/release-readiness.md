# Production Release Readiness

This document is the release gate for the first public Khora release.

It is intentionally stricter than “the compiler works” or “trusted developers can try it.” A public release means a developer with no prior knowledge of the project can discover Khora, understand why it exists, install a supported build, write and debug a nontrivial program, use the core production facilities, deploy it on a supported target, and report a problem without requiring direct help from the language's author.

The first public release may still be `0.x`. It does not need a mature package ecosystem, 1.0 stability, or proven Fortune 100 adoption. It does need an honest, coherent, supportable product boundary.

A section is complete only when its behavior is implemented, documented, tested, and exercised through the public surface. A roadmap heading that says “done” does not override a known semantic gap listed here.

---

## 1. Language and compiler correctness

- [ ] Phase 12 is complete, including all implementation work that remains in its entries rather than only the currently landed subset.
- [ ] Every known silent-miscompile, silently ignored annotation, unresolved-name hole, and misleading diagnostic discovered during Phase 12 has either been fixed or promoted to a release-blocking issue.
- [ ] The compiler rejects unresolved type names, unresolved trait bounds, contradictory annotations, and unsupported constructs at the source location that caused the problem.
- [ ] Type inference and lowering have regression coverage for closures, generics, traits, effect rows, handlers, capabilities, higher-kinded types, ADTs, pattern matching and annotations.
- [ ] Common invalid programs produce diagnostics that describe the programmer's problem rather than an internal compiler phase.
- [ ] A deliberate invalid-program corpus tests diagnostic text, ranges and recovery for common mistakes.
- [ ] The formatter is stable enough that a public project can use `khora fmt` in CI without routine semantic churn.
- [ ] The linter's supported checks are documented, deterministic and free of known high-confidence false positives.
- [ ] The language's grammar, precedence and user-visible semantics have one canonical public reference.

### Decimal

- [ ] Exact decimal literal syntax is complete (`0.01d` or the final equivalent), documented and tested alongside ordinary floating-point literals.
- [ ] Decimal arithmetic has adversarial coverage for large magnitudes, large scale differences, negative values, rescaling, equality, ordering, addition, subtraction, multiplication and division.
- [ ] Intermediate calculations cannot overflow merely because two representable Decimal values need scale alignment; where a wider intermediate is required, the implementation uses one or rejects the operation deliberately.
- [ ] Rounding behavior, overflow behavior, parsing and formatting are specified rather than inferred from tests.

### Time

- [ ] `Date`, `Time`, `DateTime`, `Offset` and instant/clock concepts have public documentation that clearly separates wall time from an instant.
- [ ] The supported calendar range, overflow behavior and invalid-date behavior are specified.
- [ ] Time-zone database support, if provided by a package rather than `std`, has a documented integration path and is not implied to be built into `std`.

---

## 2. Runtime soundness and structured concurrency

Khora's runtime is part of the language contract. The release cannot rely on “works in ordinary tests” for ownership, cancellation or fiber migration.

- [ ] The M:N scheduler is a supported default runtime path rather than an experimental mode that ordinary users are expected to opt into manually.
- [ ] The remaining scheduler/I/O work has been measured after Phase 12 and either completed or explicitly shown not to justify further architecture work before release.
- [ ] Native scalable I/O backends are present for the platforms claimed as production-supported where the existing portable backend would otherwise impose a known scaling ceiling.
- [ ] The scheduler passes sustained soak and adversarial tests across supported platforms.
- [ ] Fiber cancellation always permits required finalizers/resource cleanup to run.
- [ ] Nursery semantics are complete: a failing child has the documented effect on siblings and the parent, with typed failure behavior tested.
- [ ] No language-visible behavior depends on a fiber staying on one OS thread unless the program explicitly enters a documented thread-affine FFI boundary.
- [ ] Safepoints and cancellation points remain distinct and are documented as such.
- [ ] Every runnable `Task` has exactly one owner at every instant; wake tokens or backend state never create a second owner.
- [ ] Lost-wakeup regressions cover registration during backend wait, cancellation during I/O wait, injected runnable work while a worker is in the backend, and shutdown.

### Formal unsafe/soundness review

- [ ] Every `unsafe` block and `unsafe impl` in the runtime/compiler boundary is inventoried.
- [ ] Each inventory entry names the invariant that makes it sound and the test or argument that protects the invariant.
- [ ] `unsafe impl Send for Task` and equivalent cross-thread/coroutine state are reviewed explicitly.
- [ ] TLS/thread-local state is audited under fiber migration. No thread-local address may survive across a suspension unless the design explicitly proves it safe.
- [ ] FFI pointers, callbacks and thread-affine handles have a documented lifetime/thread rule.
- [ ] Sanitizer and dynamic-analysis coverage appropriate to the implementation is run before release; unsupported analyses and their blind spots are documented.

---

## 3. Resource, database and cancellation semantics

- [ ] `Region`/finalizer behavior is reliable under success, typed failure, cancellation and trap boundaries where cleanup is permitted.
- [ ] `std::db::transaction` rolls back not only when its body returns an error but when its fiber is cancelled.
- [ ] Transaction tests assert begin/commit/rollback ordering for success, typed failure, cancellation, commit failure and rollback failure policy.
- [ ] Database cancellation does not leave a pooled connection holding an open transaction or locks.
- [ ] File, socket, TLS and process resources have cancellation tests that prove cleanup rather than merely absence of a crash.
- [ ] Bounded concurrency primitives are documented as the default way to protect externally driven resources.

---

## 4. HTTP, overload and server behavior

Peak requests per second alone is not a release gate. A production runtime must remain healthy when offered work exceeds sustainable throughput.

- [ ] The HTTP server has a documented distinction between connection capacity and actively executing/request-processing capacity.
- [ ] The current connection/nursery limits are intentionally tuned for scheduled fibers rather than inherited from the old OS-thread implementation.
- [ ] Sustained overload tests cover at least 100%, 125% and 200% of sustainable offered load.
- [ ] Under overload, RSS remains bounded within the configured operating model.
- [ ] Runnable queues and admission queues remain bounded or have explicitly documented limits.
- [ ] Latency degrades predictably instead of entering overload collapse.
- [ ] Controlled rejection uses appropriate HTTP semantics such as 503 for service saturation and 429 for policy/rate limits where relevant.
- [ ] The service recovers promptly after offered load falls.
- [ ] Slow, half-open and maliciously quiet connections cannot occupy unbounded server resources.
- [ ] The supported HTTP feature set is documented honestly, including any remaining body-size, transfer-encoding, WebSocket or HTTP/2 limitations.

---

## 5. Observability

- [ ] Trace context is carried automatically across fiber spawn, scheduling, stealing, wake and cancellation according to the documented model.
- [ ] W3C `traceparent` parsing/formatting is covered by conformance-style tests.
- [ ] A no-op tracer remains cheap enough that disabled tracing is a viable production configuration.
- [ ] At least one real exporter/integration exists, preferably OTLP/OpenTelemetry, outside `std`.
- [ ] A reference service demonstrates an incoming HTTP trace flowing through application work, spawned fibers and database operations with correct parent/child relationships.
- [ ] Logging guidance explains how logs correlate with traces and fiber/request context.
- [ ] Metrics/exporter responsibilities are clearly separated between `std` vocabulary/runtime context and external packages.

---

## 6. Database ecosystem proof

The neutral `Db` capability is not enough by itself to prove the production database story.

- [ ] At least one production-grade database package exists; PostgreSQL is the preferred first proof.
- [ ] The package exercises network I/O, pooling, query execution, result decoding, cancellation and transactions through public Khora APIs.
- [ ] Pool saturation is bounded and documented.
- [ ] Database numeric types preserve exact values; `NUMERIC`/money-like values do not silently pass through `Float`.
- [ ] Schema/type mismatches are visible rather than silently coerced.
- [ ] Connection and transaction failure behavior is tested under cancellation and network loss.
- [ ] A reference application uses the package rather than a test-only handler.

SQLite or additional engines are useful but not required for the first public release if the package story and `Db` abstraction have already been validated by a serious driver.

---

## 7. Cross-compilation and deployment

A target is “supported” only when the toolchain produces something users can actually run or deploy. Object emission alone is not target support.

- [ ] The public supported-target matrix distinguishes code-generation support, build/link support and production-supported deployment targets.
- [ ] Cross-built `khora-rt` artifacts exist for every target advertised as buildable.
- [ ] Required linker/sysroot assets are obtained automatically or through a documented, repeatable installation path.
- [ ] At least Linux x86-64 and Linux arm64 have end-to-end build-and-run validation if they are listed as supported.
- [ ] Static/musl/container deployment is either supported and tested or explicitly excluded from the first release.
- [ ] Cross-platform CI builds the same release-facing examples used in documentation.

### WebAssembly / Cloudflare

- [ ] `wasm32-unknown-unknown` has its own correct platform/std surface and does not inherit Linux sockets or filesystem bindings.
- [ ] The no-fibers wasm execution model is explicit, tested and documented until native wasm stack switching becomes a supported runtime basis.
- [ ] Host-provided networking/filesystem/database capabilities are modeled intentionally rather than emulated through nonexistent Unix APIs.
- [ ] A real Cloudflare deployment example builds and runs from the public toolchain.
- [ ] “Cloudflare Workers support” is not claimed until the deployment example works end to end.

---

## 8. FFI and C interoperability

- [ ] Phase 12's C export/import surface is complete enough for a small Khora library to be called from an ordinary C-compatible consumer.
- [ ] The supported C ABI types and ownership rules are documented precisely.
- [ ] Strings, buffers, records/structs and error results have an explicit allocation/freeing contract.
- [ ] Thread-affine foreign libraries are tested against fiber migration rules.
- [ ] Blocking FFI calls have a documented interaction with the scheduler/blocking pool.
- [ ] Foreign callbacks into Khora either have a supported contract or are explicitly unsupported.
- [ ] FFI failures cannot silently cross a boundary in an ABI-undefined representation.
- [ ] At least one external-language integration test (for example Python, Node or a small C host) validates the public C surface.

---

## 9. Traps, debugging and production diagnosis

- [ ] Debug information is emitted for supported native targets with source file and line mappings.
- [ ] A documented LLDB/GDB workflow can set breakpoints and inspect ordinary Khora stack frames where supported.
- [ ] Runtime traps identify the Khora source location that triggered them.
- [ ] Stack traces are meaningful enough to diagnose a production failure rather than exposing only runtime/compiler internals.
- [ ] The policy for overflow, bounds failure and other unrecoverable bugs is explicitly documented.
- [ ] The Phase 12 trap-containment decision is complete: it is clear whether a trap terminates a fiber, a request boundary or the whole process, and why.
- [ ] If some traps deliberately terminate the process, server guidance explains the operational consequence rather than pretending request isolation exists.

---

## 10. Compiler performance and scale

- [ ] Build-time measurements use a release-built Khora compiler before public performance claims are made.
- [ ] The corpus includes at least one substantially larger application than the current small reference programs.
- [ ] Cold build time, warm/repeated developer workflow, peak compiler memory, monomorphization cost and link time are measured separately enough to identify regressions.
- [ ] Whole-program monomorphization is tested at a size capable of exposing superlinear behavior.
- [ ] A documented budget/regression baseline exists for future compiler changes.
- [ ] Any public comparison to Rust/Go/another compiler uses equivalent workloads and records tool versions/hardware.

---

## 11. Tooling and editor experience

- [ ] `khora build`, `khora check`, `khora test`, `khora fmt` and package/toolchain commands work through one documented CLI without repository-internal invocation knowledge.
- [ ] The LSP provides reliable diagnostics, hover and go-to-definition at minimum.
- [ ] Completion is good enough for ordinary standard-library and project symbols.
- [ ] Formatting integrates with the editor and CI.
- [ ] A maintained VS Code extension or an equivalently accessible editor integration exists for the first public audience.
- [ ] Syntax highlighting covers the complete current grammar.
- [ ] The editor extension and compiler report their versions in bug reports/repro instructions.
- [ ] The language's MCP support is documented as optional tooling rather than required to write correct Khora.

---

## 12. Installation, toolchains and release artifacts

“Clone the compiler repository and run Cargo” is not the public installation story.

- [ ] A tagged public release exists with an explicit semantic version such as `0.1.0`.
- [ ] Supported platforms have downloadable compiler/toolchain artifacts or a single documented automated installer.
- [ ] Artifacts include checksums.
- [ ] `khora --version` identifies the exact compiler release and enough build metadata for bug reports.
- [ ] Projects can pin a compiler version and obtain it without manually linking a locally compiled checkout.
- [ ] A missing pinned compiler fails loudly rather than silently substituting another version.
- [ ] Release notes and a changelog describe breaking language/std/tooling changes.
- [ ] The fresh-machine installation path is tested in CI or release validation.
- [ ] Installation instructions never require knowledge of the Rust implementation unless building Khora itself.

---

## 13. Package ecosystem

A large registry is not required, but dependency use must be coherent and reproducible.

- [ ] Public documentation explains dependency declarations, exact resolution behavior, lockfiles and the content-addressed store.
- [ ] A developer can consume a third-party package without repository-specific manual setup.
- [ ] The policy for source packages versus binary artifacts is explicit.
- [ ] Version/compatibility expectations for packages are documented even if full version solving is deferred.
- [ ] The first-party packages used by reference applications are published/consumable through the same mechanism available to users.
- [ ] Package integrity is verified from the lockfile/store as documented.
- [ ] If no public registry exists at first release, that limitation and the supported git/package workflow are prominent rather than hidden.

---

## 14. Supply chain and security

- [ ] `SECURITY.md` defines how vulnerabilities should be reported privately.
- [ ] Release artifacts have provenance/signing or the chosen equivalent appropriate to the release infrastructure.
- [ ] An SBOM can be produced for the compiler/toolchain and, where practical, Khora application dependencies.
- [ ] Package hashes and lockfile guarantees are documented in security terms rather than only implementation terms.
- [ ] CI/release credentials and publication flow do not require a developer's local workstation to be the root of trust.
- [ ] Dependencies used to build release artifacts are pinned/reproducible to the extent claimed.
- [ ] The `[permissions]` model is described accurately: compile-time authority control is not presented as a runtime sandbox unless a real sandbox is implemented.

---

## 15. Compatibility, governance and contribution policy

A `0.x` release may break. It may not be ambiguous about when and how it breaks.

- [ ] A public compatibility policy defines guarantees for compiler releases, source syntax, `std`, lockfiles and packages before 1.0.
- [ ] The policy states what 1.0 is waiting for.
- [ ] Breaking releases provide migration notes.
- [ ] The project defines how language changes are proposed and accepted.
- [ ] The boundary between `std`, first-party packages and third-party ecosystem packages is documented.
- [ ] `CONTRIBUTING.md` explains build/test expectations and the review path for compiler, runtime, stdlib and documentation changes.
- [ ] Maintainer/governance responsibility is explicit even if one person remains final decision-maker.
- [ ] The public project communicates a credible maintenance plan and does not imply organizational backing that does not exist.

---

## 16. Public documentation

All public documentation lives under `website/content/docs/`; repository-internal design documentation remains under `docs/`.

### Getting Started

- [ ] Install Khora on a clean supported machine.
- [ ] Create a project.
- [ ] Build and run it.
- [ ] Run tests.
- [ ] Add a dependency.
- [ ] Use editor integration.
- [ ] The complete path can be followed in roughly one sitting without private project knowledge.

### Language Guide

- [ ] Values, bindings and functions.
- [ ] Modules/imports and packages.
- [ ] Records, tuples and algebraic data types.
- [ ] Pattern matching and destructuring.
- [ ] Collections and strings.
- [ ] Pipelines and call syntax.
- [ ] Generics, traits/typeclasses and higher-kinded abstractions at the level ordinary users need.
- [ ] Typed failure and `raises`.
- [ ] Effects, handlers and `with` capabilities.
- [ ] Resource scopes/regions and finalization.
- [ ] Fibers, nurseries, cancellation and bounded concurrency.
- [ ] Shared state and the rules for crossing fiber boundaries.
- [ ] Testing and common project structure.

### Language Reference

- [ ] Grammar and lexical rules.
- [ ] Precedence and associativity.
- [ ] Type system and inference rules at a precise user-facing level.
- [ ] Effect/failure/capability row semantics.
- [ ] Trait/typeclass lookup/import rules.
- [ ] Pattern/exhaustiveness behavior.
- [ ] Memory/resource semantics users can observe.
- [ ] Concurrency and cancellation semantics.
- [ ] FFI and trap behavior.

### Standard library

- [ ] Searchable API documentation exists for public `std` modules and exported symbols.
- [ ] API docs are generated or validated from the source of the corresponding compiler release so they cannot drift silently.
- [ ] Important APIs include examples, not only signatures.

### Cookbook

- [ ] HTTP service.
- [ ] JSON API.
- [ ] Database transaction.
- [ ] Bounded concurrency/backpressure.
- [ ] Cancellation-safe resource use.
- [ ] Tracing/observability.
- [ ] Configuration/environment access.
- [ ] Testing an effect/capability with a handler/test double.
- [ ] Deployment to at least one native target.
- [ ] Deployment to Cloudflare if wasm support is part of the release claim.

### Migration/on-ramp guides

- [ ] Khora for TypeScript/Effect developers.
- [ ] Khora for Go developers.
- [ ] Khora for Rust developers.

These guides should translate mental models, not market against other languages.

---

## 17. khoralang.com production documentation site

`khoralang.com` is the canonical public home for the language.

- [ ] The site is built from the repository's `website/` tree.
- [ ] Deployment through Cloudflare is reproducible from CI rather than dependent on an author's workstation.
- [ ] The deployed site records the Git revision/release it was built from.
- [ ] Release documentation is versioned and remains addressable after newer releases ship.
- [ ] `/docs/` points at the current stable release.
- [ ] `/docs/<version>/` resolves pinned documentation for supported historical releases.
- [ ] `/docs/next/` may expose development documentation but must be visibly marked unstable.
- [ ] Site search covers the language guide, reference and standard library.
- [ ] Code snippets are syntax highlighted and, where feasible, checked against the matching Khora compiler during the docs build.
- [ ] Broken internal links and stale symbol references fail CI.
- [ ] The site contains direct paths to installation, releases, documentation, GitHub/source, security reporting and contribution information.
- [ ] Benchmarks shown publicly link to reproducible methodology rather than presenting context-free numbers.

The frontend framework is not part of the language contract. URL structure, content ownership and versioning are.

---

## 18. Reference applications and end-to-end proof

Before release, Khora must have applications that use the public product rather than compiler-internal shortcuts.

- [ ] A polished CLI/data application demonstrates ordinary native use outside HTTP servers.
- [ ] A production-style HTTP service uses JSON, configuration, typed failures, capabilities, structured concurrency, database access and tracing.
- [ ] If Cloudflare is advertised, an edge/wasm application deploys through the documented public path.
- [ ] At least one application is large enough to expose compiler/tooling friction beyond toy examples—preferably several thousand lines.
- [ ] Reference applications build using released package/toolchain commands, not repository-only harnesses.
- [ ] CI continuously builds/tests the reference applications against the release candidate.

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
- [ ] Claims distinguish shipped functionality from planned functionality.
- [ ] Benchmark pages state hardware, operating system, compiler mode/version, workload, connection count, duration, number of runs and control methodology.
- [ ] Cross-sitting absolute numbers are not presented as controlled comparisons.
- [ ] Scheduler performance is described together with latency, memory and overload behavior, not only peak request rate.
- [ ] Khora does not market a benchmark as “beats Rust/Go/etc.” when the measurement is load-generator- or machine-limited.

---

## 21. Release automation and final gate

- [ ] CI is green on every production-supported platform.
- [ ] Baseline/compiler tests, runtime stress, HTTP conformance, examples, docs links/snippets and package-resolution tests pass for the exact release candidate.
- [ ] Release artifacts are produced by automation from the release tag.
- [ ] Documentation deployed to `khoralang.com` is generated from that same release/tag.
- [ ] Checksums/provenance/release notes are published together.
- [ ] Known limitations are current and prominent.
- [ ] The release candidate has completed the external-user validation above.

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
