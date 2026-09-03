# Production Release Readiness

This document is the release gate for the first public Khora release.

It is intentionally stricter than “the compiler works” or “trusted developers can try it.” A public release means a developer with no prior knowledge of the project can discover Khora, understand why it exists, install a supported build, write and debug a nontrivial program, use the core production facilities, deploy it on a supported target, and report a problem without requiring direct help from the language's author.

The first public release may still be `0.x`. It does not need a mature package ecosystem, 1.0 stability, or proven Fortune 100 adoption. It does need an honest, coherent, supportable product boundary.

A section is complete only when its behavior is implemented, documented, tested, and exercised through the public surface. A roadmap heading that says “done” does not override a known semantic gap listed here.

---

## Current state

Scored against the tree, item by item, against what is in the repository rather
than against the roadmap's account of itself. **200 of 222**, and re-scored
whenever a section moves.

**The number is counted, not typed.** `scripts/check-readiness.sh` counts the
boxes and fails the baseline when this line disagrees with them. It said 153
while the boxes said 150, for long enough that the wrong figure was quoted in a
status report before anybody added them up. It understated in the other
direction too: five items sat open whose `**Left:**` described conditions fixed
commits earlier. Arithmetic is now checked; the annotations still need reading.

**Last read, rather than last counted: 2026-09-03.** Every open item was
checked against the repository, and against the compiler wherever a claim could
be run rather than read. Four notes had drifted since the last pass: two of
Phase 12's named remainders were closed, the unresolved-name rendering was
fixed, and a cookbook page one item said was missing turned out to exist. None
of that changed the count by much and all of it changed what was worth doing
next, which is the whole use of the gate — a score nobody re-reads decays
silently, and the annotations are where the decay lives. Add the date when you
read them.

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
| 2. Runtime soundness and structured concurrency | 10 / 16 |
| 3. Resource, database and cancellation semantics | 5 / 6 |
| 4. HTTP, overload and server behavior | 8 / 10 |
| 5. Observability | 4 / 7 |
| 6. Database ecosystem proof | 6 / 7 |
| 7. Cross-compilation and deployment | 11 / 11 |
| 8. FFI and C interoperability | 4 / 8 |
| 9. Traps, debugging and production diagnosis | 7 / 7 |
| 10. Compiler performance and scale | 5 / 6 |
| 11. Tooling and editor experience | 8 / 10 |
| 12. Installation, toolchains and release artifacts | 6 / 9 |
| 13. Package ecosystem | 7 / 7 |
| 14. Supply chain and security | 6 / 7 |
| 15. Compatibility, governance and contribution policy | 8 / 8 |
| 16. Public documentation | 44 / 46 |
| 17. khoralang.com production documentation site | 8 / 12 |
| 18. Reference applications and end-to-end proof | 6 / 6 |
| 19. External-user validation | 0 / 5 |
| 20. Public positioning and benchmark integrity | 7 / 7 |
| 21. Release automation and final gate | 5 / 8 |

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

- [ ] Phase 12 is complete, including all implementation work that remains in its entries rather than only the currently landed subset. **Left:** three entries are partial, and they are not the ones this said. Both of #140's named remainders are closed: `khora run src/a.kh` inside a package refuses and names `src/bin/` rather than silently running the package's program, and an unused *type* import warns like any other. What is actually open is in the roadmap's own headings -- 12.2 cross-compilation is "step one done", 12.4 debug information has line tables and locals but not heap layout, and 12.9 supply chain has the SBOM and not the signature.
- [x] Every known silent-miscompile, silently ignored annotation, unresolved-name hole, and misleading diagnostic discovered during Phase 12 has either been fixed or promoted to a release-blocking issue. **Done:** #143 fixed (errata 62); #142 and #108 are tracked and listed under Known limitations.
- [x] The compiler rejects unresolved type names, unresolved trait bounds, contradictory annotations, and unsupported constructs at the source location that caused the problem. **Done:** the rendering complaint this carried is fixed. An unresolved name no longer collides with the real type -- a mismatch between `a::Widget` and an unimported `Widget` reads ``expected `a::Widget`, found `Widget` ``, qualified exactly where it would otherwise say the same word twice, and `khora-types`' `an_ordinary_mismatch_is_not_qualified` keeps the qualification from spreading to mismatches that do not need it. Checked against the compiler: an unresolved type, an unresolved bound and a return that disagrees with its body each report once, with a sentence naming both sides, and the caret is under the *body* rather than the signature.
- [x] Type inference and lowering have regression coverage for closures, generics, traits, effect rows, handlers, capabilities, higher-kinded types, ADTs, pattern matching and annotations. **Done:** 2,107 tests; `khora-types/tests` and `khora-codegen-llvm/tests` carry a file per feature.
- [x] Common invalid programs produce diagnostics that describe the programmer's problem rather than an internal compiler phase.
- [x] A deliberate invalid-program corpus tests diagnostic text, ranges and recovery for common mistakes. **Done:** `crates/khora-diagnostics/tests` and `khora-codegen-llvm/tests/errors.rs`.
- [x] The formatter is stable enough that a public project can use `khora fmt` in CI without routine semantic churn. **Done:** `khora fmt --check` runs over `std` and all ten corpus members in `scripts/baseline.sh`.
- [x] The linter's supported checks are documented, deterministic and free of known high-confidence false positives. **Done:** `/docs/reference/lints/` lists all fifteen checks with a default level each, and documents the `[lints]` table that overrides them. The last known false positive was #164 — a name used only inside a `${}` hole — and it is fixed and tested.
- [x] The language's grammar, precedence and user-visible semantics have one canonical public reference. **Done:** `/docs/reference/grammar`, `/lexical-structure`, `/expressions`.
- [x] A `Char` type and a character-boundary string API exist, or their absence is recorded as a deliberate limitation with the byte-oriented alternative documented. **Done:** `Char` is a builtin scalar written `'a'`; `is_char_boundary`, `next_boundary`, `previous_boundary`, `char_at`, `chars` and `char_length` are the API that makes `String::slice` safe to reach for.
- [x] `attempt` discharges a `raises` row holding more than one error type, or the one-type limit is documented and `catch` is presented as the way to handle a wider row. **Done:** the second half. `attempt` still takes one `E`, and `/docs/reference/failures/#one-error-type` says so, quotes the compiler's own message, explains that `Result<A, E>` needs one `E` and Khora has no anonymous sum type to collapse a two-type row into, and sends the reader to `catch` — which matches per type and never has to name the union.
- [x] A diagnostic never renders two different types with the same text. **Done:** #148. Two modules each declaring an `Entry`, passed one to the other, now reports ``expected `audit::b::Entry`, found `audit::a::Entry``` — checked against the compiler for this re-score, not read off the task.

### Decimal

- [x] Exact decimal literal syntax is complete (`0.01d` or the final equivalent), documented and tested alongside ordinary floating-point literals. **Done:** `0.01d`, documented in `/docs/reference/lexical-structure`.
- [x] Decimal arithmetic has adversarial coverage for large magnitudes, large scale differences, negative values, rescaling, equality, ordering, addition, subtraction, multiplication and division.
- [x] Intermediate calculations cannot overflow merely because two representable Decimal values need scale alignment; where a wider intermediate is required, the implementation uses one or rejects the operation deliberately. **Done:** 128-bit significand; roadmap 13.x widened it for exactly this.
- [x] Rounding behavior, overflow behavior, parsing and formatting are specified rather than inferred from tests. **Done:** `/docs/stdlib/api/decimal` — "What it does when it cannot answer".

### Time

- [x] `Date`, `Time`, `DateTime`, `Offset` and instant/clock concepts have public documentation that clearly separates wall time from an instant. **Done:** `/docs/stdlib/api/time`.
- [x] The supported calendar range, overflow behavior and invalid-date behavior are specified.
- [x] Time-zone database support, if provided by a package rather than `std`, has a documented integration path and is not implied to be built into `std`. **Done:** `/docs/stdlib/api/time/` says the database is not in `std` and cannot be — it is a dataset that IANA cuts several times a year, and nothing behind the compatibility promise can move that often. The seam is named and typed: `std` owns `Offset`, and a package or the host owns the rules that produce one.

---

## 2. Runtime soundness and structured concurrency

Khora's runtime is part of the language contract. The release cannot rely on “works in ordinary tests” for ownership, cancellation or fiber migration.

- [x] The M:N scheduler is a supported default runtime path rather than an experimental mode that ordinary users are expected to opt into manually. **Done, by settling the question rather than by switching the default:** threads are 0.1.0's default and the scheduler is a documented, supported opt-in. The argument, the measurements and their limits are in `docs/design/fibers.md`; the user-facing half is in `/docs/reference/concurrency` and `/docs/limitations`. A program cannot observe which it has, so this is not a compatibility commitment.
- [x] The remaining scheduler/I/O work has been measured after Phase 12 and either completed or explicitly shown not to justify further architecture work before release. **Done:** it could not be measured because the generator was reporting one connection's rate times the number of connections -- errata 77 -- and with `bench/loadgen.exe` it measures cleanly. `bench/service` answers 180,715 / 175,908 / 178,510 req/s on threads against 145,095 / 143,916 / 144,135 on the scheduler at 32 connections, three sittings each, spread 1.03x and 1.01x, flat from 32 connections to 128. Threads lead by 23 per cent on the median and on the tail, which settles the default the `fibers.md` decision already chose and settles it on a measurement of throughput rather than of single-connection latency. No further architecture work is justified before release; the epoll/kqueue/IOCP item below is the one that would move it and is separately open.
- [ ] Native scalable I/O backends are present for the platforms claimed as production-supported where the existing portable backend would otherwise impose a known scaling ceiling. **Left:** Windows and macOS. Linux is done: `crates/khora-rt/src/epoll.rs` is a backend rather than a rewrite -- the watch list, the deadline riding on a watch, the loopback waker and the one-wake-per-registration contract are all unchanged, and a kernel that will not open an `epoll` falls back to `poll`. Level-triggered and re-described whenever a descriptor's watchers change, because the operations above perform one syscall each and do not drain, which is what edge-triggered requires; four tests cover it under WSL, including that the backend really is `epoll` and not a silent fallback, that two fibers on one socket are both woken, and that a socket left readable is reported once rather than spinning. macOS wants `kqueue`, which is the same shape and is unwritten because nothing here can run it. Windows wants IOCP, which is not a backend swap: it is *completion*-based, and the operations here perform their own syscall and ask the reactor only when it would have blocked, so answering that shape means owning the buffer and the operation -- a different interface, per `docs/design/scheduler.md` §2.
- [ ] The scheduler passes sustained soak and adversarial tests across supported platforms. **Left:** macOS, and only macOS. #108 is resolved -- the flake was `cargo test`, not Linux -- and `khora-rt/src/soak.rs` passes on Windows and, through WSL, on Linux, every baseline. The adversarial half is now genuinely adversarial: `a_hostile_schedule_leaves_nothing_behind` changes the *schedule* rather than the messages -- one CPU, so every interleaving is a preemption at a point the OS chose rather than two threads that never contend; one worker, so every fiber goes through one queue; a saturated blocking pool, so a submitter waits rather than finding a thread free; and cancellation storms, so a fiber is cancelled during a transition rather than between two. The affinity is restored when the test ends, since a plain `cargo test` shares one process. macOS is exercised nowhere, and its `THREAD_AFFINITY_POLICY` is a hint about cache sharing rather than a binding, so the pinning half would not apply there even once it is.
- [x] Fiber cancellation always permits required finalizers/resource cleanup to run. **Done:** `tests/fibers.rs`: a cancelled fiber runs every finalizer and stops only itself.
- [x] Nursery semantics are complete: a failing child has the documented effect on siblings and the parent, with typed failure behavior tested. **Done:** #139: the first failure cancels the siblings, every child is waited for, and the nursery raises `ChildFailed`.
- [ ] No language-visible behavior depends on a fiber staying on one OS thread unless the program explicitly enters a documented thread-affine FFI boundary. **Left:** not assessed. The *rule* is written down — `/docs/reference/ffi/` lists what foreign code may not retain across a suspension, thread identity and thread-affine handles among it — and fibers are OS threads by default, so the M:N path is where this could bite. What would settle it is a test that migrates a fiber between workers and asserts that nothing observable changed.
- [x] Safepoints and cancellation points remain distinct and are documented as such. **Done:** `/docs/reference/concurrency`; `a_loop_in_an_infallible_function_is_not_a_cancellation_point` pins it.
- [ ] Every runnable `Task` has exactly one owner at every instant; wake tokens or backend state never create a second owner. **Left:** not assessed, and nothing in `scheduler.rs` or `coro.rs` states the invariant in those words or asserts it. What would settle it is the invariant written down where the ownership transfer happens, plus a debug assertion or a loom/TSan run that would fail if a wake token produced a second owner.
- [x] Lost-wakeup regressions cover registration during backend wait, cancellation during I/O wait, injected runnable work while a worker is in the backend, and shutdown. **Done:** `khora-rt/src/scheduler.rs` tests cover all four.

### Formal unsafe/soundness review

- [x] Every `unsafe` block and `unsafe impl` in the runtime/compiler boundary is inventoried. **Done:** 282 blocks, every one carrying an argument, and `scripts/no-bare-unsafe.sh` in the gate so the count cannot drift -- it was 41 short when this was measured, having been 28 short at the audit that wrote `docs/design/soundness.md`.
- [ ] Each inventory entry names the invariant that makes it sound and the test or argument that protects the invariant. **Left:** every block now names its invariant; what is not systematic is the second half -- *which test* protects it. The load-bearing ones say so (`#[inline(never)]` on `current::running` names the test that caught its removal); most do not.
- [x] `unsafe impl Send for Task` and equivalent cross-thread/coroutine state are reviewed explicitly. **Done:** three impls -- `Task`, `Migrating`, `Handed` -- each reviewed in `docs/design/soundness.md`, with the residual obligation on Rust bodies named.
- [x] TLS/thread-local state is audited under fiber migration. No thread-local address may survive across a suspension unless the design explicitly proves it safe. **Done:** 46 `.with(..)` closures in `khora-rt`, none containing a suspension, so no reference outlives one; `CURRENT` is the one read by address and is held by `#[inline(never)]` plus the switch's memory clobber, with the test that caught its removal named.
- [x] FFI pointers, callbacks and thread-affine handles have a documented lifetime/thread rule. **Done:** `/docs/reference/ffi/`. `Ptr` is an opaque foreign address; a buffer is lent for the duration of one call and must not be retained after the body returns; and *Blocking and suspension* names the four things foreign code may not carry across a Khora suspension — a thread-local address, native thread identity, borrowed errno-like state, and a thread-affine handle whose contract requires one OS thread. *Libraries may be called from several host threads* covers the other direction.
- [x] Sanitizer and dynamic-analysis coverage appropriate to the implementation is run before release; unsupported analyses and their blind spots are documented. **Done:** `scripts/tsan.sh` under WSL2; the blind spots are recorded in the script's own header.

---

## 3. Resource, database and cancellation semantics

- [x] `Region`/finalizer behavior is reliable under success, typed failure, cancellation and trap boundaries where cleanup is permitted. **Done:** and `Region::open`'s own documentation now says *which* scope ends it — the enclosing block, established by experiment rather than by reading, because the difference between a lease that ends with the call and one that ends with the caller is what made a pool of `n` behave like a pool of `n` uses.
- [x] `std::db::transaction` rolls back not only when its body returns an error but when its fiber is cancelled. **Done:** `a_cancelled_fiber_rolls_back_and_does_not_commit`. This was already true when the section was scored and was left unticked because it had not been looked at, which is the scoring rule working rather than a finding.
- [x] Transaction tests assert begin/commit/rollback ordering for success, typed failure, cancellation, commit failure and rollback failure policy. **Done:** eleven cases in `crates/khora-codegen-llvm/tests/db.rs`, each asserting an exact transcript rather than a count, so any permutation fails. The rollback-failure policy — discard it, because the engine's complaint about a rollback is a worse thing to report than the reason the rollback was needed — was a deliberate `let _ =` with no test until now.
- [x] Database cancellation does not leave a pooled connection holding an open transaction or locks. **Done:** two halves. The ordering — `a_cancelled_lease_is_returned_only_after_the_rollback` asserts the rollback reaches the engine before the lease reaches the idle channel, which is the whole of what `packages/postgres` relies on. And the case where the rollback itself fails: `Db` gained a `broken` operation, `std::db` calls it from both the error path and the cancellation path, and the driver closes the request channel — so the serving fiber stops, the socket shuts, and the next borrower is answered `Disconnected` rather than handed somebody else's uncommitted rows.
- [ ] File, socket, TLS and process resources have cancellation tests that prove cleanup rather than merely absence of a crash. **Left:** TLS and process. **Files are proved:** `a_cancelled_fiber_closes_the_file_it_was_reading` cancels a fiber mid-`fold_lines` and then *deletes* the file, which Windows refuses while a handle is open. **Sockets are proved and were leaking:** `std::net::socket` registered no release at all, so a socket was closed only by a normal return -- the one exit a server never takes. `Router::held_open` and `Router::served` register theirs with a region now, and `net_cancel.rs` proves both ends: a cancelled fiber's port binds again, and a peer sees the connection close. Both of those were checked by deleting the release and watching the test fail, which is the check the next two need and the reason they are not written. **TLS was attempted and abandoned rather than skipped**, and what was learned is worth having: `khora_tls_close` is correct -- it sends `close_notify`, flushes, and dropping the box closes the socket -- so a leak would have to be the release not running. A test that cancelled a fiber holding a session left the client waiting five seconds, but it could not tell that from a handshake that never completed, because the server's own report was not read. Two blind alleys were ruled out on the way: a cancellation absorbed at a *root* stops the process, so the cancelled work has to be on a fiber that something waits for; and an outer `Scope::root()` does *not* stop an inner region running, which was the suspicion and is wrong. **Process is unstarted:** `khora_spawn_status` waits on the child and reaps it before returning, and the wait is not interruptible, so the claim to prove is that a cancelled fiber's child still finishes and is reaped rather than abandoned.
- [x] Bounded concurrency primitives are documented as the default way to protect externally driven resources. **Done:** `/docs/cookbook/bounded-concurrency`, and `bounded_nursery`'s own documentation.

---

## 4. HTTP, overload and server behavior

Peak requests per second alone is not a release gate. A production runtime must remain healthy when offered work exceeds sustainable throughput.

- [x] The HTTP server has a documented distinction between connection capacity and actively executing/request-processing capacity. **Done:** by saying that they are one number and why, rather than by inventing a second. An accepted connection is a fiber and that fiber is inside the handler for as long as the handler runs, so 256 is both the most connections served at once and the most handlers running at once. `http_native.kh` says so where the bound is written and `/docs/cookbook/http-service/` says it where somebody writing a server will meet it, with the measurement that matters: the server saturates by about 16 connections, so past that the bound governs queueing rather than capacity, and a handler waiting on something scarcer wants its own smaller bound rather than a lower connection limit.
- [x] The current connection/nursery limits are intentionally tuned for scheduled fibers rather than inherited from the old OS-thread implementation. **Done:** measurable now, and measured. Throughput is flat from 16 connections to 128 -- 176k, 180k, 177k, 176k -- while median latency rises in proportion to the queue, 70us to 724us, which is a saturated server rather than one that has run out of capacity. The server saturates well below 256 connections, so the bound is not what limits throughput at any concurrency this rig can offer, and raising or lowering it would change queueing rather than capacity. 256 is now a number with a measurement behind it rather than an inherited one.
- [x] Sustained overload tests cover at least 100%, 125% and 200% of sustainable offered load. **Done:** `khora-codegen-llvm/tests/load.rs`: overload, recovery and shutdown.
- [x] Under overload, RSS remains bounded within the configured operating model. **Done:** `loadgen --watch-pid` samples the server's resident set through a run. `bench/service` peaks at **8.4 MB** at 32 connections and does not grow with connections, and the ladder from 16 to 128 is flat throughput with latency proportional to the queue, which is a saturated server rather than one losing ground. For scale: Go's `net/http` holds 21.8 MB doing the same work, Node 86.8 MB, Kestrel 240 MB and the JDK's server 699 MB. The first version of this measurement said 576 KB and was wrong -- `tasklist` prints memory with a thousands separator and the sampler split on the last comma -- which is errata 77's second half.
- [x] Runnable queues and admission queues remain bounded or have explicitly documented limits. **Done:** `bounded_nursery` turns the ceiling into backpressure; the listening backlog absorbs the rest.
- [x] Latency degrades predictably instead of entering overload collapse. **Done:** `overload_becomes_latency_rather_than_loss`.
- [x] Controlled rejection uses appropriate HTTP semantics such as 503 for service saturation and 429 for policy/rate limits where relevant. **Done:** the server answers 503 under backpressure, and the whole of 5xx carries a real reason phrase — a service that answered 503 and put `HTTP/1.1 503 Unknown` on the wire is what put them there. 429 is available for a policy layer; there is no rate limiter in `std`, so the *where relevant* half is not relevant yet.
- [x] The service recovers promptly after offered load falls. **Done:** `a_service_recovers_after_the_burst`.
- [x] Slow, half-open and maliciously quiet connections cannot occupy unbounded server resources. **Done:** true over plain HTTP and now over TLS. `serve_connection` sets a 10-second receive deadline before reading, so a client that goes quiet has its socket closed rather than parking a fiber for the life of the process. The `https` half was missing because `set_receive_timeout` takes a *socket* and a TLS session owns its socket rather than handing it back; `std::net::tls::set_receive_timeout` sets the deadline on the socket underneath, where the reactor's timer lives, and `dial` uses it for `https` exactly as it does for `http`.
- [x] The supported HTTP feature set is documented honestly, including any remaining body-size, transfer-encoding, WebSocket or HTTP/2 limitations. **Done:** `/docs/limitations` names the HTTP surface limits.

---

## 5. Observability

- [x] Trace context is carried automatically across fiber spawn, scheduling, stealing, wake and cancellation according to the documented model. **Done:** `tests/trace.rs`: the sampled flag is carried.
- [x] W3C `traceparent` parsing/formatting is covered by conformance-style tests.
- [x] A no-op tracer remains cheap enough that disabled tracing is a viable production configuration. **Done:** `the_default_tracer_records_nothing_and_stays_out_of_the_way`.
- [x] At least one real exporter/integration exists, preferably OTLP/OpenTelemetry, outside `std`. **Done:** `packages/otlp` is a `Tracer` that batches finished spans and posts them as OTLP/HTTP JSON to a collector, outside `std` for the reason `std::trace` gives -- a wire protocol with its own release cadence does not belong in a library that promises not to break. The queue is `dropping` and a failed POST is swallowed, because a tracer that can stall the service it measures is one that takes production down. Twelve tests assert against the rendered bytes rather than the values going in; one of them found the resource's `service.name` being sent as a bare string where OTLP wants an `AnyValue`, which a collector drops without saying so. Two limits are `std::trace`'s rather than the protocol's and are written in the package's README: spans have no parents, because the effect has no operation that says "inside this one", and attributes given to `start` are dropped.
- [ ] A reference service demonstrates an incoming HTTP trace flowing through application work, spawned fibers and database operations with correct parent/child relationships. **Left:** the third of it that is about parent/child, and it is blocked on `std::trace` rather than on the example. `examples/ledger_service` takes a `Tracer` through its handlers and wraps each database operation in `around_result` -- `entries.create`, `entries.list` -- so a request does flow through application work into the database with a span around it. **But no span in Khora has a parent today.** `Span` carries a `parent` field, `Tracer::none` sets it to zero, and nothing anywhere sets it to anything else: `start` takes a name and attributes, so a handler cannot know what span it is inside, and `around` does not tell it. A nested `around` therefore starts a second trace rather than a child span, which `packages/otlp`'s README records from the exporter's side. Spawning fibers in the example would demonstrate nothing until the effect can express the relationship -- which wants a design decision about how a tracer learns its current span, and is a change to a published `std` API.
- [x] Logging guidance explains how logs correlate with traces and fiber/request context. **Done:** the blocker was a decision rather than a page -- there was no logging capability for logs to correlate *with*, and no way to write to standard error at all. `std::log` is both: `eprint` as the primitive, and a `Log` effect over it emitting one JSON object per line with `timestamp`, `level` and `message` in a fixed order, attributes as typed fields, and five levels filtered by the handler rather than the caller. It is a capability for the reason everything reaching outside is one, which also makes it testable -- a test installs a logger that collects into a list, with no global to reset. `/docs/cookbook/logging/` covers all of it, including correlation: attributes are `std::trace`'s `Attribute`, so a line carries `trace_id` and `span_id` in the field names OpenTelemetry expects. **The span has to be passed in**, because `std::trace` still has no notion of a current span -- the same limitation item 177 is blocked on, recorded on the page rather than papered over.
- [x] Metrics/exporter responsibilities are clearly separated between `std` vocabulary/runtime context and external packages. **Done:** `/docs/stdlib/api/trace` — "Why this is `std`'s and the exporter is not".

---

## 6. Database ecosystem proof

The neutral `Db` capability is not enough by itself to prove the production database story.

- [x] At least one production-grade database package exists; PostgreSQL is the preferred first proof. **Done:** `packages/postgres`, tested in the gate.
- [x] The package exercises network I/O, pooling, query execution, result decoding, cancellation and transactions through public Khora APIs.
- [x] Pool saturation is bounded and documented.
- [x] Database numeric types preserve exact values; `NUMERIC`/money-like values do not silently pass through `Float`. **Done:** `numeric` decodes to `Cell::Money(Decimal)` and `packages/postgres/src/conn_test.kh` holds the scale against a trailing zero, keeps a value too wide for the significand as the server's own digits rather than a truncation, and pins `float4`/`float8` as `Text` — `Cell` has no float variant and this is where that is enforced.
- [x] Schema/type mismatches are visible rather than silently coerced. **Done:** `Cell::text`/`number`/`money`/`flag` answer `None` for the wrong variant rather than rendering it, tested by `cells_do_not_coerce`; and a column whose text does not match its OID stays `Text` instead of being guessed at, tested in `conn_test.kh`.
- [ ] Connection and transaction failure behavior is tested under cancellation and network loss. **Left:** cancellation is covered thoroughly and network loss is not covered at all. `crates/khora-codegen-llvm/tests/db.rs` proves a cancelled fiber rolls back and does not commit, that a failing rollback does not hide the reason for it, that a failed rollback during cancellation reaches the handler, and that a cancelled lease returns only after the rollback. What no test does is drop the connection mid-transaction.
- [x] A reference application uses the package rather than a test-only handler. **Done:** `examples/ledger_service` depends on `packages/postgres` and the gate builds it.

SQLite or additional engines are useful but not required for the first public release if the package story and `Db` abstraction have already been validated by a serious driver.

---

## 7. Cross-compilation and deployment

A target is “supported” only when the toolchain produces something users can actually run or deploy. Object emission alone is not target support.

- [x] The public supported-target matrix distinguishes code-generation support, build/link support and production-supported deployment targets. **Done:** `/docs/deployment/supported-targets` defines supported / experimental / emission-only and lists no triples yet, which is the honest state.
- [x] Cross-built `khora-rt` artifacts exist for every target advertised as buildable. **Done:** Vacuously: no target is advertised as buildable yet.
- [x] Required linker/sysroot assets are obtained automatically or through a documented, repeatable installation path. **Done:** for the three triples 0.1.0 supports, which is the only claim being made. `/docs/getting-started/installation/` names the toolchain per platform in a table -- `xcode-select --install`, `apt install clang`, `dnf install clang`, Visual Studio Build Tools or LLVM -- and the installer checks for a usable linker and says so when there is not one. LLVM itself needs nothing: it is linked into the compiler rather than invoked. Cross-compilation would need a sysroot story and is explicitly out of scope, which is the item below.
- [x] At least Linux x86-64 and Linux arm64 have end-to-end build-and-run validation if they are listed as supported. **Vacuously, for arm64, and really for x86-64.** `/docs/deployment/supported-targets/` now lists the supported triples, and Linux arm64 is not among them precisely because nothing builds or tests it -- an unchecked platform in a supported table is the claim that page exists to prevent. The three that are listed each produce a release artifact, unpack it elsewhere and compile and run a program with it before anything is published, and x86-64 Linux is additionally exercised through WSL2 in the gate.
- [x] Static/musl/container deployment is either supported and tested or explicitly excluded from the first release. **Done, by excluding it explicitly.** `/docs/deployment/supported-targets/` says static and musl builds are not produced or tested and that the published Linux artifact is dynamically linked against the system C library, which is what `/docs/deployment/containers/` assumes. The container page is guidance for building an image around that artifact rather than a claim that a static one exists.
- [x] Cross-platform CI builds the same release-facing examples used in documentation. **Done:** `.github/workflows/ci.yml` runs the backend job on ubuntu, macos and windows.

### WebAssembly / Cloudflare

- [x] `wasm32-unknown-unknown` has its own correct platform/std surface and does not inherit Linux sockets or filesystem bindings. **Vacuously: no wasm target is advertised in 0.1.0.** `/docs/deployment/supported-targets/` says so and says what has not been built -- there is no Worker-shaped platform surface in `std`. LLVM emits wasm and `tests/targets.rs` checks that the runtime's symbols resolve for it, which is emission and is documented as emission rather than as a deployment path.
- [x] The no-fibers wasm execution model is explicit, tested and documented until native wasm stack switching becomes a supported runtime basis. **Vacuously: no wasm target is advertised in 0.1.0**, so there is no execution model to be explicit about. `/docs/deployment/supported-targets/` states that plainly rather than leaving it to be inferred.
- [x] Host-provided networking/filesystem/database capabilities are modeled intentionally rather than emulated through nonexistent Unix APIs. **Vacuously: no wasm target is advertised in 0.1.0.** Nothing emulates a Unix API on a host that lacks one because nothing targets such a host. `/docs/deployment/cloudflare/` lists what would have to be modelled and tells a reader not to choose Workers in the meantime.
- [x] A real Cloudflare deployment example builds and runs from the public toolchain. **Vacuously: Cloudflare Workers is not a target of this release.** `/docs/deployment/cloudflare/` says Khora does not advertise it, says what would have to exist before that page becomes a deployment guide, and tells a reader to use a supported target instead.
- [x] “Cloudflare Workers support” is not claimed until the deployment example works end to end. **Done:** `/docs/deployment/supported-targets` and `/cloudflare` describe it as the motivating target, not as shipped.

---

## 8. FFI and C interoperability

- [x] Phase 12's C export/import surface is complete enough for a small Khora library to be called from an ordinary C-compatible consumer. **Done:** `tests/exporting.rs` builds a C host with clang and runs it.
- [x] The supported C ABI types and ownership rules are documented precisely. **Done:** `/docs/reference/ffi`; the "only scalars and pointers cross" rule is enforced in the backend.
- [x] Strings, buffers, records/structs and error results have an explicit allocation/freeing contract. **Done:** mostly by exclusion, which is a contract. `/docs/reference/ffi/` says the boundary carries C-compatible scalars and `Ptr` and nothing else — no Khora `String`, no algebraic data type, no record, no closure, no typed-failure return — so an exported function reports failure as a scalar status. Buffers are the one aggregate that crosses, caller-owned: C allocates and passes `(Ptr, capacity)`, Khora fills and returns a length. Neither side ever frees the other's memory.
- [ ] Thread-affine foreign libraries are tested against fiber migration rules.
- [x] Blocking FFI calls have a documented interaction with the scheduler/blocking pool. **Done:** the rule said to use "an API/runtime boundary intended for blocking work" and named one that did not exist -- `khora-rt`'s pool is used by `std`'s own file, process and socket operations and exposes only counters. `/docs/reference/ffi/` now gives the boundary that does exist, with the code: `Fiber::join(Fiber::spawn(fn () => native_thing(handle)))!`. A fiber is an OS thread, so the blocking call holds a thread running nothing else and the caller suspends as it would for a socket. It also records *why* there is no `blocking(body)` helper in `std` to reach for instead, which is a property of the language rather than an omission: a closure's captures are not in its type, so nothing at a spawn can tell whether what it captured may cross to another fiber -- which is why `Fiber::spawn` requires the closure to be written where it is spawned and why the wrapper is refused. A helper would have to be a compiler intrinsic, as `Fiber::spawn` is. The two costs a caller has to plan for are stated: a thread and a round trip, and no cancellation on the far side.
- [x] Foreign callbacks into Khora either have a supported contract or are explicitly unsupported. **Done:** `/docs/reference/ffi/` has a `Callbacks C keeps and calls later` section answering the three questions that were open. The pointer lasts as long as the process, because a `pub extern fn` is an exported symbol rather than an allocated value. A Khora *closure* cannot be exported at all -- it is a code pointer and its captures, and there is no C type for the pair -- so a foreign API wanting user data takes a `Ptr` the caller passes back. The runtime must already be loaded, which is the library's own initialisation. Re-entrancy is an ordinary nested call on the same thread: a separate exported boundary with its own error handling, since a `raises` row cannot travel out through the foreign frames in between, and process-fatal on a trap by default exactly as anywhere else. The two rules a retained callback is most likely to break -- any host thread may enter it, and a borrowed pointer is valid for one call -- are named where they apply.
- [x] FFI failures cannot silently cross a boundary in an ABI-undefined representation. **Done:** `foreign_signature_obstacle` refuses at the call site with the rule quoted.
- [x] At least one external-language integration test (for example Python, Node or a small C host) validates the public C surface. **Done:** `tests/exporting.rs` compiles and runs a C host against the exported surface.

---

## 9. Traps, debugging and production diagnosis

- [x] Debug information is emitted for supported native targets with source file and line mappings. **Done:** DWARF line tables; `tests/debugging.rs`.
- [x] A documented LLDB/GDB workflow can set breakpoints and inspect ordinary Khora stack frames where supported. **Done:** #151. `/docs/reference/debugging/` has six sections — backtraces, debug information, LLDB and GDB, printing, what changes when a fiber is involved, and how to report what you find — and the debugger section is a worked session rather than a description: `lldb ./build/myapp`, `breakpoint set --file main.kh --line 12`, `run`.
- [x] Runtime traps identify the Khora source location that triggered them. **Done:** `a_bounds_failure_says_which_line_indexed`.
- [x] Stack traces are meaningful enough to diagnose a production failure rather than exposing only runtime/compiler internals. **Done:** the runtime's own frames are trimmed from the top, so the first frame is the line in the program that trapped, and `the_runtimes_frames_are_not_at_the_top` in `tests/debugging.rs` holds it there. A trap with no backtrace prints the note that says how to get one, tested by `a_trap_without_the_switch_says_how_to_get_more`. What is exposed below `main` is the C runtime that started the process, and `/docs/reference/debugging/` says so rather than trimming what it cannot name.
- [x] The policy for overflow, bounds failure and other unrecoverable bugs is explicitly documented. **Done:** `/docs/reference/traps`.
- [x] The Phase 12 trap-containment decision is complete: it is clear whether a trap terminates a fiber, a request boundary or the whole process, and why. **Done:** the whole process, with one opt-in exception, and both halves are implemented and documented. `trap.rs` ends the process with status 134 from whichever fiber trapped; there is no catch in the scheduler and the nursery's failure policy handles raises rather than traps. The exception is the export boundary -- a C host may call `khora_set_trap_policy`, and containment disarms itself if the guarded call spawned a fiber, because a child may outlive the call and hold allocations it made. `docs/design/traps.md` argues the decision and `/docs/reference/traps/` states it.
- [x] If some traps deliberately terminate the process, server guidance explains the operational consequence rather than pretending request isolation exists. **Done:** `/docs/reference/traps/#what-this-means-for-a-server` says plainly that a trap in a handler ends the server rather than the request, why the `catch` around a handler does not stop it, and what to do about it -- validate request-shaped integers at the boundary, run more than one process, and expect a restart. `/docs/cookbook/http-service/` carries the short form and links to it. `tests/traps_in_a_server.rs` holds the claim: a raise is a 500 and the server carries on, and a trap in the next handler ends the process with 134.

---

## 10. Compiler performance and scale

- [x] Build-time measurements use a release-built Khora compiler before public performance claims are made. **Done, and enforced rather than remembered:** `scripts/compiler-perf.py` builds `khora` with `--release` on every run instead of using one it finds. The first version did use one it found, and the binary on the machine it was written on was four days old -- it failed to parse a character escape the current lexer accepts, and would otherwise have quietly produced build times for a compiler nobody has. It builds the release runtime archive too, because a release compiler with a debug runtime fails in the linker in a way that looks nothing like a missing build step.
- [x] The corpus includes at least one substantially larger application than the current small reference programs. **Done:** #152. `examples/khq` is 3,643 lines across ten modules with 34 tests — a query language with a lexer, a parser, an evaluator, builtins and a renderer. It builds in the baseline and its tests run there.
- [x] Cold build time, warm/repeated developer workflow, peak compiler memory, monomorphization cost and link time are measured separately enough to identify regressions. **Done:** `KHORA_TIMINGS=1` splits a build into check, monomorphize, lower, optimize, object and link, and `scripts/compiler-perf.py` reads them. For `examples/khq`, the largest program in the corpus: cold build 12.12s, warm 0.33s, check-only 0.60s, peak compiler memory 187 MB. The split is the finding -- **monomorphization is 8.0 of the 11.5 seconds**, with the object file 2.5 and the linker 0.5. A regression now points at a phase instead of at a total.
- [x] Whole-program monomorphization is tested at a size capable of exposing superlinear behavior. **Done, and it is linear:** a generated package instantiating one generic at 10, 50, 200 and 400 distinct types -- a 40x range -- moves the monomorphize phase from 3,004 ms to 3,930 ms, which is 1.31x. The marginal cost is about 2.4 ms per instantiation on a fixed cost of about 3.0 seconds, and **the fixed cost is `std`**: every build monomorphizes the whole standard library whole-program before it reaches the program. That is why the totals looked flat until the phase was isolated, and it is the same finding as roadmap 14.35 from the other side.
- [x] A documented budget/regression baseline exists for future compiler changes. **Done:** `docs/compiler-perf-baseline.json`, written by `compiler-perf.py --write-baseline` and compared by `--check`, which exits non-zero when cold, warm or check-only build time has moved by more than 1.5x. The tolerance is wide on purpose: this runs on whatever machine somebody has, and a gate that cries wolf is a gate that gets skipped, so it catches a doubling rather than a drift. It says so when the baseline was taken on a different platform rather than comparing wall-clock numbers across machines silently.
- [x] Any public comparison to Rust/Go/another compiler uses equivalent workloads and records tool versions/hardware. **Done:** `bench/README.md` records hardware, versions and method, and says a number only travels within one sitting.

---

## 11. Tooling and editor experience

- [x] `khora build`, `khora check`, `khora test`, `khora fmt` and package/toolchain commands work through one documented CLI without repository-internal invocation knowledge. **Done:** One CLI; `khora --help` covers it and the getting-started path uses nothing else.
- [x] The LSP provides reliable diagnostics, hover and go-to-definition at minimum. **Done:** Measured over the protocol: 15 capabilities, diagnostics including missing-import, hover and definition.
- [x] Completion is good enough for ordinary standard-library and project symbols. **Done:** 34 items for `List::` over the wire.
- [x] Formatting integrates with the editor and CI.
- [x] A maintained VS Code extension or an equivalently accessible editor integration exists for the first public audience. **Done:** `editors/vscode`, built by `.github/workflows/extension.yml`, tagged `vscode-v0.3.0`.
- [x] Syntax highlighting covers the complete current grammar. **Done, and the audit found six gaps.** The keyword axis was already airtight -- both lists are checked against the lexer by `editor_grammar.rs` on every run -- and nothing checked the literals or the operators, which is where the grammar had fallen behind. Fixed: backtick strings had **no rule at all**, so their contents were scanned as code and an embedded `"` mis-colored the rest of the file (`std/core.kh`, `std/json.kh` and `std/schema.kh` each contain one); decimal literals `1d` and `0.01d` went uncolored because both numeric rules end in `\b`; bare `<` and `>` had no rule, so every type bracket and every `a < b` was uncolored while `a <= b` was not; `${}` holes read as string rather than as the code they are; `..` fell into the separator class; and `///` had no scope of its own. Postfix `!` no longer reads as logical negation. Six new tests assert each, including one that fails if any literal the lexer makes has no rule -- which is the test whose absence let three of these through.
- [x] The editor extension and compiler report their versions in bug reports/repro instructions. **Done:** The status bar runs `khora toolchain which` and shows the answering toolchain and its reason.
- [x] The language's MCP support is documented as optional tooling rather than required to write correct Khora. **Done:** `/docs/getting-started/editor/` puts it under *AI coding tools*, after the paragraph that says what an editor actually needs, and states outright that it is optional — the compiler and the language server behave identically whether or not an agent is connected.
- [x] `khora doc` works in an ordinary user package rather than only over `std`. **Done:** #174 fixed both halves. The defaults are package-relative -- the nearest `khora.toml` decides, sources come from its `src` and pages go to its `docs/api` -- so the command means the same thing in every package, and outside a package it refuses and says what to type. The stale sweep no longer claims the output directory: a `.khora-doc` record beside the pages lists what the generator owns, so a page whose module was deleted still goes and a file the command did not write is left alone and reported. Five tests in `khora-cli/tests/doc.rs`, including a hand-written page surviving a run and a rerun.
- [x] A package may declare more than one executable, or the `src/bin` convention the linter enforces is documented as the only supported shape. **Done:** #162 implemented both halves. `khora build .` builds `src/main.kh` and every file under `src/bin/`, each as its own compilation so two `main`s never meet, and each named after its file. `khora run .` runs the package's own program and names the others when there is none. `misplaced-main` now allows `src/bin/*.kh`, and `/docs/reference/modules-and-packages/` documents the layout.

---

## 12. Installation, toolchains and release artifacts

“Clone the compiler repository and run Cargo” is not the public installation story.

- [ ] A tagged public release exists with an explicit semantic version such as `0.1.0`. **Left:** Three release candidates are tagged (`v0.1.0-rc.1` … `rc.3`); no final `v0.1.0`.
- [x] Supported platforms have downloadable compiler/toolchain artifacts or a single documented automated installer. **Done:** `install.sh` / `install.ps1`; `.github/workflows/release.yml` packages on three OSes.
- [x] Artifacts include checksums. **Done:** The installer verifies against the published checksum.
- [x] `khora --version` identifies the exact compiler release and enough build metadata for bug reports. **Done:** #150. It prints `khora 0.1.0 (6baa7df-dirty) x86_64-pc-windows-msvc` — version, the commit it was built from with a dirty marker when the tree was not clean, and the target triple.
- [x] Projects can pin a compiler version and obtain it without manually linking a locally compiled checkout. **Done:** `[toolchain]` in `khora.toml`; the shim hands over before argument parsing.
- [x] A missing pinned compiler fails loudly rather than silently substituting another version. **Done:** `khora toolchain which` reports it and the editor status bar shows it as a warning.
- [x] Release notes and a changelog describe breaking language/std/tooling changes. **Done:** #149. `CHANGELOG.md` groups by what a reader needs first — **Breaking**, then **Fixed**, then the rest — and says so at the top. A bug that produced a silently wrong answer is listed under Breaking as well as Fixed, because code written around it behaves differently now.
- [x] The fresh-machine installation path is tested in CI or release validation. **Done:** `release.yml` compiles a program with the packaged artifact before attaching it.
- [x] Installation instructions never require knowledge of the Rust implementation unless building Khora itself. **Done:** `/docs/getting-started/installation` mentions a linker and never Cargo.

---

## 13. Package ecosystem

A large registry is not required, but dependency use must be coherent and reproducible.

- [x] Public documentation explains dependency declarations, exact resolution behavior, lockfiles and the content-addressed store. **Done:** `/docs/reference/modules-and-packages`.
- [x] A developer can consume a third-party package without repository-specific manual setup. **Done:** `a_package_from_a_git_repository_is_resolved_compiled_and_run` fetches a package from a repository outside the build, compiles the application against it and runs it — a generic, a method and an impl of a `std` trait all crossing the boundary. `khora build` resolves what it needs, so there is no fetch step to remember, and `khora install <url>` writes the manifest entry after checking the package's real name and whether it offers itself at all.
- [x] The policy for source packages versus binary artifacts is explicit. **Done:** `/docs/reference/modules-and-packages` — dependencies are source, fetched and compiled, and there are no binary artifacts to publish or to trust.
- [x] Version/compatibility expectations for packages are documented even if full version solving is deferred. **Done:** the same page says there is no registry, so `version = "…"` has nothing to resolve against; `git` for what you did not write and `path` for what you did; and a branch name resolves to the commit it pointed at, so `rev = "main"` is a convenience when the dependency is added rather than a moving target afterwards.
- [x] The first-party packages used by reference applications are published/consumable through the same mechanism available to users. **Done:** `packages/postgres` was consumed from a project outside this repository by `git` + `subdir`, compiled and run. `a_package_in_a_subdirectory_of_a_larger_repository_compiles_and_runs` keeps that shape honest — a package three directories inside a checkout whose root is a different, unpublished package, which is this repository's layout and most repositories that hold a library. The earlier note read the reference application's path dependency as evidence about the mechanism; inside one repository a path dependency is the correct choice, and it says nothing about whether a user can fetch the package.
- [x] Package integrity is verified from the lockfile/store as documented. **Done:** every resolution hashes what arrived and refuses the build if it disagrees with the lockfile — `resolve.rs`, tested by the tampering case in `khora-pkg`'s own tests, and now said on `/docs/reference/modules-and-packages` so that "as documented" is true as well.
- [x] If no public registry exists at first release, that limitation and the supported git/package workflow are prominent rather than hidden. **Done:** `/docs/limitations` — "Package ecosystem".

---

## 14. Supply chain and security

- [x] `SECURITY.md` defines how vulnerabilities should be reported privately.
- [x] Release artifacts have provenance/signing or the chosen equivalent appropriate to the release infrastructure. **Done:** `release.yml` attests every archive with `actions/attest-build-provenance`, signed by GitHub's OIDC identity for that workflow in this repository, so a downloader runs `gh attestation verify <file> --repo <repo>` and learns which workflow at which commit produced those exact bytes. There is no maintainer key to trust or to leak, which is the same reasoning that already keeps publication off a workstation. `/docs/getting-started/installation/` documents the check beside the checksum, and says what a checksum does not tell you.
- [x] An SBOM can be produced for the compiler/toolchain and, where practical, Khora application dependencies. **Done:** both halves, in one format. `khora sbom` already rendered a package's resolution as CycloneDX 1.5; `scripts/toolchain-sbom.py` does the same for the compiler from `cargo metadata --locked` -- 192 components with versions and licences, plus the pinned LLVM and the Rust toolchain that built it, neither of which is a Cargo dependency and both of which are in the artifact. No timestamp and everything sorted, so two runs over an unchanged tree produce identical bytes, which is checked. `release.yml` attaches it as `khora-<version>.cdx.json` with a checksum, and attests it.
- [x] Package hashes and lockfile guarantees are documented in security terms rather than only implementation terms. **Done:** `/docs/reference/modules-and-packages/` states the guarantee as a guarantee — *the checksum is verified, not merely recorded*. Every resolution hashes what arrived and compares it against the lockfile, and if the same commit id ever produces different bytes the build stops rather than compiling what turned up. It also says a branch name resolves to a commit and the commit is what is recorded, so `rev = "main"` is not a moving target.
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

**The Guide was dissolved into the Language Reference.** Fourteen of its
fifteen pages were a second telling of a Reference page, so each subject below
is now scored against the Reference page that absorbed it — the material is
where a reader looks it up, rather than in two places that have to be kept in
step. `/docs/guide/*` redirects.

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
- [x] The linter's checks have a public reference page listing each check, its default level and how to configure it in `khora.toml`. **Done:** #151. `/docs/reference/lints/` — fifteen checks, a default level each, and the `[lints]` table that overrides them.

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
- [ ] Release documentation is versioned and remains addressable after newer releases ship. **Left:** The machinery is built and serving — `website/versions.mjs` is the one list both `sync-docs.mjs` and `astro.config.mjs` read, every page lives under `/docs/next/` rather than at the site root, and `/docs/` follows the newest stable entry by computation rather than by somebody remembering to edit it. What is not done is the thing the item actually claims: no tree has been pinned yet, because a section is cut per stable *major* and there is not one. The earlier decision recorded here — that versioned paths should wait until two versions existed — was reversed, and `docs/design/docs-urls.md` now carries the contract instead of the argument for deferring it.
- [x] `/docs/` points at the current stable release. **Done:** it redirects to the newest entry in `website/versions.mjs` marked stable, computed rather than written, so it follows the list without anybody editing a second place. Before v1 there is no stable entry and it redirects to `next`, which is the documentation for the only compiler anybody can install.
- [ ] `/docs/<version>/` resolves pinned documentation for supported historical releases. **Left:** there are no historical releases to pin, so the list has one entry. The mechanism that will serve them is running now rather than waiting: adding one is a directory under `website/content/versions/<id>/` and a line in `website/versions.mjs`, after which `current`, the sidebar, the banners and every short path follow by themselves. Sections are per stable *major*, since a patch release does not give a reader a different language.
- [x] `/docs/next/` may expose development documentation but must be visibly marked unstable. **Done:** it is where everything lives today, and every page carries a banner saying it describes the unreleased compiler and that the language is unstable before v1. A stable tree that is no longer current gets its own banner pointing at the one that is; the current stable tree gets none, because a banner on the page everybody is meant to read is one everybody learns to ignore.
- [x] Site search covers the language reference, standard library and cookbook. **Done:** Starlight's Pagefind index, over all 100 pages including the generated `stdlib/api` tree. This was already true when the section was scored — a build prints `Found 100 HTML files` — and was unticked because nobody had run one.
- [x] Code snippets are syntax highlighted and, where feasible, checked against the matching Khora compiler during the docs build. **Done:** all 580 hand-written examples are compiled by `scripts/check-docs.sh`, which is a step of the gate — so an example and the compiler cannot disagree without the build saying so. A fragment is *parsed*, which is what catches syntax that no longer exists; the 17 that declare their own `module` are fully checked, and those are the ones that caught a `handler for Db` gone stale two commits earlier. `khora doc --check` owns the 993 generated ones.
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

- [x] The homepage explains in the first screen what Khora is, who it is for and why it exists. **Done:** what it is and why it exists were already in the hero -- a statically typed native-compiled language that makes failures, capabilities, resource lifetimes and concurrency visible, because the important parts of a program belong in its model. Who it is for was the missing third and is now stated in the same screen: people writing services and tools that have to keep running, where a dropped error or a leaked handle is found in production rather than in review.
- [x] The language is presented as general-purpose; finance remains a proving ground rather than the language's identity. **Done:** checked rather than assumed. Neither the homepage nor `README.md` mentions finance, trading, ledgers or payments at all; the framing in both is reliable systems and services. Finance appears only where it is a worked example -- `examples/ledger_service`, `examples/risk_analyzer` and the `Decimal` documentation -- which is the proving-ground role this item asks for.
- [x] Claims distinguish shipped functionality from planned functionality. **Done:** `/docs/limitations` and `/docs/deployment/supported-targets` both do this deliberately.
- [x] Benchmark pages state hardware, operating system, compiler mode/version, workload, connection count, duration, number of runs and control methodology. **Done:** `/docs/performance/` states all of them in the sentence above the table -- 16-core Windows desktop, release builds, 32 connections, generator on the same machine, six-second runs, mean of five, dated -- and `loadgen` prints the machine and the date itself so a figure cannot be separated from its circumstances by being copied. The control methodology is the four conditions, checked by the script on every run, with a failing server reported as what failed instead of as a number.
- [x] Cross-sitting absolute numbers are not presented as controlled comparisons. **Done:** and it was not, when this was first ticked. `README.md` published **538,000 requests a second** for `std::net::http` under a heading that called it "one measurement, so it can be argued with", while `bench/README.md` marked every figure in that table as a measurement of the harness rather than of the servers, and the site said no requests-per-second figure is published. The README now says what the site says, keeps only the within-sitting ratios, and states the four conditions a published number would have to meet.
- [x] Scheduler performance is described together with latency, memory and overload behavior, not only peak request rate. **Done:** every row of the table on `/docs/performance/` carries p50, p99 and peak resident memory beside the rate, because `loadgen` measures all four in the same run through a probe connection competing with the load. The page says plainly that Go answers the median request faster than Khora and the slowest one nearly three times more slowly, which is the sort of thing a peak-rate table hides. Overload behaviour is the ladder: flat throughput with latency proportional to concurrency, printed under every figure.
- [x] Khora does not market a benchmark as “beats Rust/Go/etc.” when the measurement is load-generator- or machine-limited. **Done:** Nothing public makes the claim.

---

## 21. Release automation and final gate

- [x] CI is green on every production-supported platform. **Done:** ubuntu, macos and windows in `ci.yml`.
- [x] Baseline/compiler tests, runtime stress, HTTP conformance, examples, docs links/snippets and package-resolution tests pass for the exact release candidate. **Done:** `scripts/baseline.sh`: 2,107 tests, conformance, corpus, packages, cache and the Linux runtime through WSL2.
- [x] Release artifacts are produced by automation from the release tag.
- [ ] Documentation deployed to `khoralang.com` is generated from that same release/tag. **Left:** it is generated from `main`. `.github/workflows/docs.yml` triggers on `push` to `branches: [main]` under `website/**`, so the site tracks the branch rather than the tag -- which is right while there is no release and wrong the moment there is one. Closing this is a trigger and a checkout, not a rewrite, and it belongs with the tagging in the item above.
- [x] Checksums/provenance/release notes are published together. **Done:** all three in the job that uploads. Checksums were already there; provenance is the attestation above; the notes are cut from `CHANGELOG.md` by `scripts/release-notes.sh` rather than written a second time, and a version with no entry stops the release rather than shipping a blank body. Notes are applied only when nobody has written one, so an edited draft is a decision rather than something to overwrite.
- [x] Known limitations are current and prominent. **Done:** `/docs/limitations`, linked from the docs index.
- [ ] The release candidate has completed the external-user validation above.
- [ ] This document has been scored against the release candidate itself, item by item, and re-scored at every subsequent candidate. **Left:** the tag, and only the tag. Scored against the tree twice now -- #173, and again on 2026-09-03 with every open item checked against what is in the repository and the compiler run wherever a claim could be run. The second pass found four notes that had drifted from the tree: two of Phase 12's named remainders were fixed, the unresolved-name rendering was fixed, and the tracing cookbook it said was missing exists. That is the argument for the item rather than against it -- a gate scored once decays, and the decay is invisible until somebody re-reads it. What is still owed is a pass against a `v0.1.0` tag rather than against `main`.

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
