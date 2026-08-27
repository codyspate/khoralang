# Releasing

Roadmap 14.20: "Which members changed, bump them and their dependents, write
the notes. `publish = true` already exists per package; this is the part that
decides what to do with it."

**This document does not decide it.** Four questions need an answer that is
policy rather than engineering, and this lays them out with a recommendation
each. Everything else is already settled and is listed first, because the
undecided part is smaller than it looks.

## What is already decided

**The version policy**, in `docs/design/compatibility.md`: semantic versioning,
with the clarification that a bug fix is not automatically a patch release —
if a program could reasonably have been written against the old behaviour,
correcting it is major however wrong it was. That document also lists the five
things a minor release may not do, three of which are Khora-specific.

**The pre-1.0 procedure**, same document: anything may change, and *every*
change that alters what a valid program does is named in the release notes. A
change nobody wrote down is a bug in the release, separately from whether the
change was right.

**The mechanics**, in `.github/workflows/release.yml`: create a draft release
with a new tag, the workflow builds a toolchain per platform and uploads to the
draft, a person looks at it and presses Publish. Candidates go out the same way
with the pre-release box ticked, as versions of their own — `v0.2.0-rc.1`, then
`v0.2.0` cut from the same commit as the last candidate. Nothing is ever
promoted in place.

**The errata rule**: `docs/errata.md` is not a changelog. It is the list of
things that were believed and turned out to be false. Release notes are a
different document and neither substitutes for the other.

So what is missing is not a release process. It is the part that answers "what
should the next version be, and what changed" without somebody reading the log.

## Question 1 — one version, or one per member?

Today `[workspace.package] version = "0.1.0"` is inherited by every member, so
the repository has exactly one version and the toolchain shares it.

**Recommendation: keep lockstep, and write down why.**

Independent per-member versions need a registry to be worth anything. Khora has
none by design — a dependency is a git URL and a rev — so a consumer pins a
commit or a tag, and **one repository tag already names exactly one state of
every member**. Independent versions would mean tags like `postgres-v0.3.0`,
which the resolver, the installer and every consumer's `rev` would have to
learn, in exchange for a distinction nothing can currently observe.

Revisit when there is a registry, which is when the distinction starts to mean
something.

## Question 2 — what does the tool actually do?

The range runs from "prints a report" to "edits manifests, writes notes, tags,
pushes and opens the draft".

**Recommendation: it reports, and on request it writes the version. It never
tags and never pushes.**

That matches the shape `release.yml` already chose deliberately: a person looks
at the draft before anything is visible. A tool that tags is a tool that can
publish a mistake at three in the morning, and the existing flow exists
precisely so that cannot happen.

```
$ khora release --since v0.1.0
8 member(s), 3 changed since v0.1.0

  changed        examples/ledger_service   (12 commits)
                 packages/postgres         (4 commits)
                 bench/service             (1 commit)
  reached by     examples/ledger_service   depends on packages/postgres

  version        0.1.0
  next           you choose: --major, --minor or --patch

  6 commits touch `std`, which is every member's dependency.
```

`--minor` then rewrites `[workspace.package] version` and stops. Tagging stays
a `git tag` somebody types.

## Question 3 — where do the notes come from?

The pre-1.0 rule is demanding: *every* change that alters what a valid program
does must be named, with the old behaviour and the new one. That is prose, and
no tool writes it.

**Recommendation: the tool drafts a skeleton from commit subjects and refuses
to pretend it is finished.**

Commit subjects in this repository are already written to be read — they lead
with the roadmap item and say what changed. Grouping them under the members
they touched produces a usable first draft. What the tool must not do is emit
that draft as if it were the notes: the compatibility rule is about *behaviour
changes described in both directions*, which a subject line does not contain.

So: write `NOTES-<version>.md` with the grouped subjects and a required section
per behaviour change, left empty. An empty required section is the tool saying
"you are not done", which is the only honest thing it can say.

## Question 4 — which commits count as a change to a member?

14.16 already answers the hard half. `khora check --since` selects members a
diff can reach, exactly, using the resolver's own dependency directories rather
than a heuristic — including the rule that a change nothing in the workspace
owns selects *everything*.

**Recommendation: reuse it unchanged**, including that rule. A commit that
touches the compiler is a change to every member, and a release tool that
quietly decided otherwise would be wrong in the most expensive direction.

The one addition: a change to `std` is a change to every member, and should be
called out by name rather than folded into "everything", because it is the
common case and the reader wants to know which it was.

## What this does not cover

- **Signing and provenance.** Roadmap 13.21; unrelated to deciding a version
  number and worth keeping separate.
- **Publishing a package to anything.** There is nothing to publish to. When a
  registry exists, `publish = true` starts meaning something more than "this is
  a library rather than an application", and question 1 reopens with it.
- **Yanking.** Requires a registry for the same reason.
- **Editions.** `docs/design/compatibility.md` says the edition machinery lands
  with the first change that needs it rather than in advance of any, and no
  change has needed it.
