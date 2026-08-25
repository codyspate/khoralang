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

**Cancel the fiber and accept the leak.** Bounded if the bug is rare. But a
trap that an input can trigger is a leak an attacker can drive: the trap becomes
a denial-of-service primitive, and a security property is a bad thing to trade
for an availability one. Rejected.

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
rather than because anything was built for it. What it still needs is a way to
know which allocations belong to the call — the runtime counts live objects and
does not list them — and a non-local exit back to the wrapper. Neither is free
and neither is phase-sized; see `docs/design/c-export.md` §8.

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

   This does not overturn the decision, because §3's mechanism is still the
   blocker and is unchanged. It removes a support, and the honest way to hold
   that is to say the decision now rests on two arguments rather than three —
   and that an export boundary is a *smaller* containment problem than a fiber
   is, with one entry, a scalar return, and no counted values live across it
   from the caller's side. If containment is ever built, that is where it would
   be cheapest to start.

Absent one of those, more diagnosability is the better spend, and the next
increment of it is variable-level debug info — 12.4's other half.
