# Releasing

Roadmap 14.20: "Which members changed, bump them and their dependents, write
the notes. `publish = true` already exists per package; this is the part that
decides what to do with it."

**Decided and built**, as `khora release`. All four recommendations below were
taken. The reasoning is kept because it is what makes the answers reviewable,
and because question 1 has a known expiry date — see the note on it.

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

**Decided: lockstep, with a known expiry date.**

There *will* be a registry, and per-package versions come with it. Until then this
stays one number.

Independent per-member versions need a registry to be worth anything. Khora has
none by design — a dependency is a git URL and a rev — so a consumer pins a
commit or a tag, and **one repository tag already names exactly one state of
every member**. Independent versions would mean tags like `postgres-v0.3.0`,
which the resolver, the installer and every consumer's `rev` would have to
learn, in exchange for a distinction nothing can currently observe.

So the shape to revisit is not "should members version independently" — it is
"a registry exists, and now they can". `khora release` refuses a workspace with
no `[workspace.package] version` rather than inventing per-member behaviour, so
the day that changes it will be a deliberate change and not a drift.

## Question 2 — what does the tool actually do?

The range runs from "prints a report" to "edits manifests, writes notes, tags,
pushes and opens the draft".

**Decided: it reports, and on request it writes the version. It never tags and
never pushes.** There is a test that asserts no tag exists afterwards, because
that is the behaviour worth being certain about.

That matches the shape `release.yml` already chose deliberately: a person looks
at the draft before anything is visible. A tool that tags is a tool that can
publish a mistake at three in the morning, and the existing flow exists
precisely so that cannot happen.

```
$ khora release --since v0.4.0
2 member(s), 1 changed since v0.4.0

  changed
    packages/alpha                           1 commit(s)

  unchanged
    packages/beta

  version   0.4.0
  next      you choose: --major, --minor or --patch

  Which one is a judgement about observable behaviour, so this does not
  guess. `docs/design/compatibility.md` has the rule, including that a
  bug fix is not automatically a patch.
```

`--minor` then rewrites `[workspace.package] version` and stops, printing
`git tag v0.5.0` as the next thing for a person to type.

The rewrite is **textual**, replacing exactly one `version = "..."` and
refusing if there is not exactly one. Re-serializing the manifest would
reformat a file full of comments written to be read, and reflowing the
reasoning in `khora.toml` would be a bad trade for one number.

## Question 3 — where do the notes come from?

The pre-1.0 rule is demanding: *every* change that alters what a valid program
does must be named, with the old behaviour and the new one. That is prose, and
no tool writes it.

**Decided: the tool drafts a skeleton from commit subjects and refuses to
pretend it is finished.**

Commit subjects in this repository are already written to be read — they lead
with the roadmap item and say what changed. Grouping them under the members
they touched produces a usable first draft. What the tool must not do is emit
that draft as if it were the notes: the compatibility rule is about *behaviour
changes described in both directions*, which a subject line does not contain.

`--notes FILE` writes the grouped subjects under a **Behaviour changes**
heading that is left empty, with a comment saying that an empty section means
the release is not ready rather than that there were none — and that "none" is
what to write if there were none. An empty required section is the tool saying
"you are not done", which is the only honest thing it can say about prose it
cannot write.

## Question 4 — which commits count as a change to a member?

14.16 already answers the hard half. `khora check --since` selects members a
diff can reach, exactly, using the resolver's own dependency directories rather
than a heuristic — including the rule that a change nothing in the workspace
owns selects *everything*.

**Decided: reuse it unchanged**, including that rule. A commit that touches the
compiler is a change to every member, and a release tool that quietly decided
otherwise would be wrong in the most expensive direction. The report names the
file that did it:

```
  every member, because .../build.sh is outside all of them
  and outside anything they depend on.
```

Naming `std` changes separately is not built. It falls out of the same rule —
`std` is outside every member — so it currently reads as "everything, because
`std/core.kh`", which already says which it was.

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
