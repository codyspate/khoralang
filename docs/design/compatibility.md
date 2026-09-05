# Compatibility

The decision for D12: what Khora promises not to break, and when it starts
promising it.

> **Before 1.0, Khora promises to change carefully and say so. After 1.0, it
> promises that a program which compiles keeps compiling and keeps meaning the
> same thing, within an edition, for the life of a major version.**
>
> **Khora has no stable binary interface and does not intend to have one.** The
> only ABI it promises is C's, at the `extern` boundary.

**Deciding this does not declare the implementation stable.** Khora is pre-1.0
and everything in the second half of this document is a description of a
promise not yet being made. The reason to write it now is that a promise is
made by accident long before it is made on purpose: every version somebody
builds against is a promise they think they have, and the way to avoid
discovering the list at 1.0 is to write it while breaking things is still free.

## Why this comes before Phase 9 rather than before publication

The roadmap used to put D12 in Phase 10, on the argument that publishing a
package is the first act that makes a promise. That is wrong by one phase.

Phase 9 is reuse analysis and FBIP: making a `map` over a uniquely-owned list
allocate nothing. Its whole purpose is to change *when memory is allocated and
freed* without changing what any program computes. That is only a safe thing to
do if "when memory is allocated and freed" was never something a program was
entitled to observe — and nobody had written down whether it was.

It is not, and this document is where that is said. Optimising first and
deciding afterwards is how an accident becomes a promise.

## What is observable

The list is short on purpose. Everything on it is something a correct program
may depend on and a minor release may not change.

- **What a program computes.** Values, and the order of effects that reach the
  outside world.
- **That an integer operation traps rather than wraps.** `docs/design/numbers.md`
  decides this and programs are written against it — a checksum that relies on
  wrapping is not slow here, it is a crash, and that is the point.
- **That `Float` formatting round-trips.** The shortest text that reads back as
  the same number, which is a specification and not an implementation detail.
- **Failure that a program can catch.** If a call `raises DbError`, it raises
  `DbError`; the row in the signature is the promise.
- **What a capability row requires.** Adding a requirement to a public function
  is a breaking change, because every caller's row grows with it.
- **Diagnostics being present.** That a wrong program is *refused* is
  observable. The wording of the refusal is not.

## What is not observable

Equally deliberate, and this is the half that matters for Phase 9.

- **When memory is allocated or freed**, and how much of it. Perceus frees at
  the last use rather than at the end of a scope, reuse analysis will free even
  less, and a program that behaves differently because an allocation did not
  happen was already relying on something it was never told.

  `khora_live_count()` exists and the test suite asserts exact object counts.
  Those are tests of the *compiler*, written from inside the repository against
  a specific build. They are not a public interface and a released program has
  no way to ask the question.

  **One exception, and it is a real one.** Releasing is unobservable when
  releasing does nothing but free. A `Region` runs the finalizers deferred into
  it when it is released, which is user code with output of its own — so *when*
  a region is released is observable, and this paragraph does not cover it.

  Found by moving a binding's reference to its last use and watching a
  finalizer print before the line above it. Whether a region ends with its
  scope or with its last reference is a language decision nobody has taken;
  until it is, the optimizer treats a region's release as observable and leaves
  it where the scope puts it. `docs/design/reuse.md`, and the "Not decided
  here" list below.

- **How long anything takes**, or how much memory it uses. Performance is a
  reason to choose Khora and not a term of the contract.
- **The order a `Map` or a `Dict` yields its entries.** Unspecified today,
  unspecified on purpose, and the way to depend on an order is to sort.
- **Hash values.** `Hash` is consistent within one run of one program and
  nothing more. Persisting one is a bug that will find you at the next release.
- **The text of a diagnostic**, its span, or how many diagnostics one mistake
  produces. Improving a message must never be a breaking change; the day it is,
  messages stop improving.
- **Anything a program reaches through `extern fn`.** That is the other side's
  contract, not Khora's. `docs/design/ffi.md`.
- **Generated symbol names, object layout, and the contents of the binary.**
  See the next section.

## There is no Khora ABI

Khora monomorphizes the whole program and passes no dictionaries
(`docs/design/typeclasses.md`). A generic function does not exist as code until
something calls it at a type, so there is nothing for a separately-compiled
caller to link against. This is not a gap to be filled later — it is the
consequence of a decision made for other reasons, and it is worth stating
plainly because it removes an entire category of promise:

- **A package ships source.** `khora-pkg` (roadmap 10.2) resolves and builds
  source, and the content-addressed cache holds build products keyed by the
  inputs that produced them, not artefacts other people's compilers may link.
- **A Khora library cannot be dynamically loaded into a Khora program.**
- **Two versions of the compiler need not produce compatible objects**, and no
  effort will be spent making them.
- **The C boundary is the exception and the only one.** `extern fn` follows the
  platform C ABI, and errata 35 already fixes what may cross it: scalars and
  pointers. That contract is stable, because it is not ours to change.

If a stable Khora-to-Khora ABI is ever wanted, it needs dictionary passing at
the boundary and is a language change, not a packaging one. It is not planned.

## Versioning

Semantic versioning, with the usual reading and one clarification.

- **Major** may break anything in "What is observable".
- **Minor** may add. New items, new modules, new trait implementations for
  existing types, new optional parameters to a manifest.
- **Patch** fixes behaviour that disagreed with this document.

The clarification is that **a bug fix is not automatically a patch release.**
If a program could reasonably have been written against the old behaviour, and
the old behaviour was not documented as unspecified, correcting it is a major
change however wrong it was. The alternative is a policy that permits any
change on the grounds that the old behaviour was a mistake.

### What a minor release may not do

Three of these are Khora-specific and would be easy to get wrong.

- **Add a case to a public sum type.** Matches are exhaustive, so a new case
  breaks every `match` on it. Rust answers this with `#[non_exhaustive]`;
  Khora has no such marker and inventing one is out of scope here — see "Not
  decided here". Until it exists, no public sum type grows in a minor release.
- **Add a field to a public record**, for the same reason in the other
  direction: a record literal names every field.
- **Add a requirement to a public function's `with` row, or an error to its
  `raises` row.** Both propagate to every caller. Adding a capability
  requirement to a `std` function is exactly as breaking as changing its
  parameters, and it does not look like it.
- **Add a function to a public trait**, unless it has a default body and the
  default is not a behaviour change for existing implementations.
- **Tighten a bound**, including adding `Share`.

Widening is fine in all the same places: removing a requirement, removing an
error from a row, loosening a bound.

## Editions

An edition is a **per-package** opt-in, declared in `khora.toml`. A package on
one edition may depend on a package on another, in both directions, and the
compiler builds both — otherwise an edition is a fork rather than a migration
path, because the first widely-used package that does not move pins everyone.

- **An edition may change syntax, keywords, and defaults.** It may make a
  warning an error, reserve a word, or change what an unannotated declaration
  means.
- **An edition may not change what an existing, unchanged program computes.**
  If the same source is valid in two editions, it means the same thing in both.
  A migration that silently alters behaviour is worse than one that refuses to
  compile, because only one of the two can be reviewed.
- **An edition may not fork the standard library's semantics.** One `std` per
  compiler version, shared by every edition in the build. An edition that
  changed what `Map::insert` does would make two packages unable to exchange a
  `Map`.
- **`khora fix` migrates mechanically or not at all.** An edition whose
  migration cannot be automated is an edition that should not ship. This is a
  constraint on what edition changes are permitted, not a promise about the
  tool.

Pre-1.0 there are no editions, because there is nothing to migrate *from*.

**There is no `edition` key either, as of 0.2.0.** There was one, holding
`"2026"`, reserved against the day this section becomes real. Reserving it cost
more than it saved: it named a year rather than a compiler, so people read it as
the answer to *which Khora builds this?* and wrote it expecting the effect that
`[toolchain] version` actually has. Two fields for one question, and the one
nothing enforced was the one that was going to be wrong. When editions arrive
they get a key then, named for what they are.

## Before 1.0

The promise is procedural rather than substantive.

- **Anything may change**, including the meaning of existing programs.
- **Every change that alters what a valid program does is named in the release
  notes**, with the old behaviour and the new one. A change nobody wrote down
  is a bug in the release, separately from whether the change was right.
- **A change that breaks source gets a migration note**; if the migration is
  mechanical, it gets an edition instead, and the edition machinery lands with
  the first change that needs it rather than in advance of any.
- **`docs/errata.md` is the record of what was wrong**, and stays that way
  after 1.0. It is not a changelog — it is the list of things that were
  believed and turned out to be false, which is a different and more useful
  document.

## What 1.0 requires that does not exist yet

Stating the policy is not meeting it. 1.0 is blocked on at least:

- **Package identity** (roadmap 10.2). Without it, "the same type from the same
  package" cannot be said, the orphan rule cannot be enforced, and the
  `extern` allow-list that makes `[permissions]` a guarantee rather than an
  account cannot be written. `docs/design/permissions.md`.
- ~~**Declaration identity.**~~ Done: a type carries the module that declares
  it, so "a public type" is now a thing that can be said. Roadmap 8.5.2,
  errata 46.
- ~~**The `std` audit.**~~ Done: 390 items reviewed, 94 undocumented ones
  written and held by a test, and `export` made to mean something inside an
  `impl` — a method without it is its module's, so the 24 helpers that were
  promises by accident are no longer. Roadmap 13.11,
  `docs/design/std-surface.md`.
- ~~**A rule for what `std` may contain.**~~ Done, and it is not the one that
  was in use: `docs/design/std-admission.md`. The floor is mechanical -- what
  the compiler names, and what the runtime implements -- and everything above it
  answers "would two independent packages have to *agree* on this, or could each
  bring its own". The first removal it implies, `std::ai`, is done.
- **A way to say "not settled yet".** There is none: no `unstable`, no
  `preview`, nothing. So at 1.0 every item in `std` freezes at the same moment,
  on a library whose oldest line is weeks old, and every admission question is
  binary when the honest answer is usually "ask again in a year".
  `std-admission.md` proposes two shapes. **This is the largest thing this
  document lists and does not have.**
- **Editions**, if anything before 1.0 needs one.

## Not decided here

- **How a type opts into growing.** `#[non_exhaustive]`'s equivalent: a way for
  a public sum type to gain cases, or a record to gain fields, without a major
  release. Needed before `std` can evolve comfortably, and it is a language
  surface change rather than a policy one.
- **What "public" means across packages.** `export` is module-level visibility
  — on an item, and since 13.11 on a method too. Whether a *package* has a
  narrower public surface than the union of its exported modules is still
  10.2's question, and nothing about it changed.
- **How long a major version is supported**, and whether more than one is. That
  is a project-management commitment and needs a project, not a design.
- **Minimum supported compiler version for a package**, and how the resolver
  reads it.
- **When a `Region` ends.** Its scope, or its last reference? The finalizers it
  runs make the answer observable, so it is the one place where "freeing is not
  observable" needs a rule rather than a shrug. Blocks widening the last-use
  optimization past `String`.
