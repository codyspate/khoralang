# What a trap does to a process

`docs/roadmap.md` 12.8 says containing a trap at a fiber boundary is "a real
argument that has not been had". This is that argument, and its answer.

**The decision: a trap ends the process, and that stays true for now.** Not
because containment is wrong in principle — it is what a server wants — but
because the mechanism it requires is phase-sized, taxes the ordinary path, and
buys less than the deployment pattern that already exists. What changes instead
is how much a trap tells you on its way out, which 12.4 and the fiber clause in
`khora-rt/src/trap.rs` have now done.

This is a decision to revisit against evidence, and the last section says what
evidence would overturn it.

## 1. What a trap is, and is not

Two things trap: integer overflow, and an index outside its array. Both are
**bugs** — the program computed something it did not intend. Neither is an error
condition; those go in a `raises` row and come back as a tagged return, and a
caller that does not handle one does not compile.

That distinction is the whole of `docs/design/numbers.md`'s argument for
trapping in every build, and it is what makes the containment question hard.
Containing an *error* is routine. Containing a *bug* means continuing to serve
from a process that has demonstrated it does not understand its own invariants.

## 2. The case for containing it anyway

A server. One malformed request should not take the other nine hundred and
ninety-nine in flight with it. This is not a hypothetical concern and it is not
a small one: it is most of why Erlang is deployed where it is deployed, and
`docs/positioning.md` aims Khora at services.

Phase 11 built the boundary this would use. A fiber has an identity, a parent,
and a nursery that already has a policy for a child that fails. A trap becoming
"this fiber failed" would need no new concept in the language and no new syntax
— which is exactly what makes it look cheap.

## 3. Why it is not cheap: there is no unwinder

`khora-codegen-llvm/src/backend/types.rs` states the design in one line:

> No unwinder, no landing pads, no personality routine: a raise is a return
> with a tag.

That is deliberate. It is why a fallible call costs a compare and a branch
rather than a table lookup, why the FFI boundary in `docs/design/ffi.md` is
simple, and why `khora-rt` links against no unwinding runtime.

**Perceus is what makes removing that a real cost rather than a small one.**
Khora is reference counted, and every live value between the trap and the fiber
boundary holds a count that has to be decremented on the way out. Ending a
fiber without running those decrements does not merely lose them — it leaks
every object the fiber touched. On a server that is memory growth proportional
to the rate of the bug, with no allocation site to blame it on: the worst
possible shape for a production problem.

So containment means unwinding, and unwinding here means:

- landing pads at every call site with live counted values, which is most of
  them;
- `invoke` in place of `call`, which costs the ordinary path;
- a personality routine, and a dependency on an unwinding runtime on every
  target — including `wasm32`, where 12.2 just arrived and exceptions are a
  proposal with uneven support;
- and unwinding **across a coroutine stack switch**, because a fiber is a
  `corosensei` stack. That is the part with no established recipe, and phase
  11's bug list is a fair warning about what that neighbourhood costs.

None of this is impossible. All of it is a phase, not a commit, and it slows
down every program that never traps to help the ones that do.

## 4. The three cheaper answers, and why two fail

**Cancel the fiber and accept the leak.** This was rejected here on the ground
that a trap an input can trigger is a leak an attacker can drive, and **that
argument was wrong** — it compared leaking against nothing. The alternative is
not nothing, it is the process ending. An attacker who can trigger a trap today
gets an immediate and total outage; against leak-containment they get gradual
memory growth. That trade runs in the defender's favour, and the original
reasoning had the comparison backwards.

It is still rejected, for a reason that is specific to what Khora has rather
than general:

**A fiber can be holding a lock over user code.** `khora_shared_update` takes
the cell's mutex and calls the change function *while holding it* — it has to,
because the whole point of `Shared` is that a read-modify-write is atomic
against other fibers. A trap inside that closure, contained by a non-local
exit, skips the guard's destructor and leaves the mutex locked with no owner.
Every other fiber that touches that cell then blocks forever.

**A hung server is worse than a crashed one.** A crash is loud, a supervisor
restarts it, and the failure is over. A deadlock is silent, survives every
health check that only pings a socket, and needs a human. Trading a crash for a
hang is not the direction to move in, and it is what containment here would buy
without an unwinder to run the destructors.

There is a *second* problem behind that one, and it does not go away by fixing
the first: the cell's value is mid-update. Releasing the lock without unwinding
would publish a half-changed value to every other fiber, which is silent
corruption — worse again.

**Region-scoped allocation.** If a request's allocations came from an arena,
containment needs no unwinding at all — free the arena and the counts stop
mattering. This is the genuinely interesting long-term answer, it fits
`Region`, and it sidesteps every cost in §3. It does not work *today* for a
**fiber**, because allocation is not arena-backed and `Shared` values
deliberately outlive the scope that made them.

**It does work at an export boundary, which this document originally missed.**
12.6 constrains an `export extern fn` to scalars and `Ptr` in and out, no
`raises`, and — since `khora-types/src/exports.rs` — no `with` clause. A
function that can be handed no capability can reach no effect, so nothing it
allocates is reachable from anywhere but its own stack: there is no module-level
mutable binding to store a value in, and nothing heap-allocated crosses the
signature in either direction. The escape argument that fails for a fiber holds
here by construction.

So the boundary 12.6 introduced is the one place in the language where §4's
second answer is *available*, and it is available because the ABI is narrow
rather than because anything was built for it. **It is now built** — a
per-call allocation registry and a `setjmp` landing point, opt-in per process,
2.6% on the allocation path of programs that never use it. `docs/design/
c-export.md` §8.

**This does not change the decision below.** A trap still ends the process
everywhere else, because everywhere else the escape argument fails: a server
fiber holds capabilities, reaches `Shared` cells, and outlives its own
allocations. §3's unwinder is still what containing *that* would need. What
changed is that the one case where a trap took down a process nobody owned now
has an answer.

**A supervisor outside the process.** A trap kills the process, something
restarts it. This needs no language change, it is what a container runtime,
systemd, and every serverless platform already do, and it is what Cloudflare
Workers does per isolate — which matters, since 12.2 made that a target. It has
a real cost: in-flight requests on that process are lost, not just the bad one.

That cost is the honest weakness of this decision, and it is the whole of it.

## 5. The decision, and what it obliges

A trap ends the process. In exchange it must be **maximally diagnosable**,
because a crash you can read in one pass is worth far more than one you cannot
and is the only compensation on offer:

- what happened, always (`Int addition overflowed`);
- **where**, from the line tables 12.4 added — the function, the file, the
  line, and its callers;
- **which fiber**, when it is not the root, so a trap on a server can be matched
  against a request log rather than guessed at.

`docs/positioning.md` promises no fault isolation, and this decision means it
must not start. Khora is not Erlang and should not imply that it is. A language
that says "one request cannot take the others down" and then does is worse than
one that never said it.

## 6. What would overturn this

Deliberately falsifiable, in the style of `docs/vision.md`'s non-negotiables:

1. **A real service shows traps are frequent enough that process restart is an
   availability problem.** Frequency is the whole question — the argument in §3
   is a cost/benefit, and it flips when the benefit is measured rather than
   imagined.
2. **Region-backed allocation lands for other reasons**, at which point §4's
   second answer becomes cheap and the calculation is different.
3. **A target makes process restart unavailable.** An embedded deployment with
   no supervisor has no fallback, and §4's third answer stops existing.

   **`docs/design/c-export.md` found one, and it is not embedded.** A Khora
   library exported to C lives inside a process it does not own — a Python
   interpreter, a Node runtime, somebody's editor. A trap there does not restart
   a service; it takes down a host belonging to somebody who never agreed to run
   a supervisor. §4's third answer, the one this decision leans on hardest, does
   not apply to that use at all.

   This did not overturn the decision, because §3's mechanism is still the
   blocker for a fiber. It removed a support — and then §4 removed the need for
   one, because containment at *that* boundary turned out not to need §3's
   mechanism at all. A library no longer takes its host down when a host asks
   it not to.

   The decision below therefore stands on its original ground, narrowed to
   where that ground actually holds: a trap ends the process wherever the
   escape argument fails, which is everywhere except an export.

4. **`Shared::update` stops holding a lock over user code.** This is the one
   with a concrete shape, and it is not far-fetched: a transactional update —
   compute the new value against a copy, then swap it in under a lock that
   never spans the change function — would leave a trapped fiber holding
   nothing. The `Held` mutex is the specific obstacle §4 names, so removing it
   is the specific thing that would make fiber containment worth costing again.
   It is not free: a swap-on-success update copies, and `Shared` exists partly
   so that a large structure does not have to be.

Absent one of those, more diagnosability is the better spend. 12.4's other half
— variable-level debug info — has since landed, so a frame now lists its locals
and prints the scalar ones; the next increment after that is describing the
heap layout so a debugger can follow a pointer into an object.
