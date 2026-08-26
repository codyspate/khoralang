# How a Khora package is offered, found and consumed

Roadmap 13.13. Phase 10.2 built the machinery — resolution, `khora.lock`, a
content-addressed store — and left the question this answers untouched: how
does a package *become available*, and how does somebody get it?

## Decided

**A git URL is the address, and there is no registry.** `khora.toml` names a
repository, a revision and, when the package is not at the repository's root, a
subdirectory. `khora install <url>` writes that entry. Nothing is uploaded
anywhere, and no name resolves to anything without a URL beside it.

```toml
[dependencies]
postgres = { git = "https://github.com/khora-lang/khora", rev = "main", subdir = "packages/postgres" }
```

**A package says whether it is offered, with `publish = true`.** Absent means
no, and a git dependency on a package that has not said it is refused.

```toml
[package]
name = "postgres"
version = "0.1.0"
publish = true
```

The rest of this document is why each of those, and what they deliberately do
not do.

## Why a URL and not a registry

A registry is not a file format; it is an operated service with an owner, a
namespace policy, an availability commitment, a takedown process, and a
security boundary that becomes the most attractive target the language has. It
is the right thing to build eventually and the wrong thing to build now, for a
reason worth stating plainly: **a registry is a promise about the future, and
this project cannot yet keep it.** Names handed out today are permanent. An
index published today is one nobody can turn off. There is no version solver
either, so the parts of a registry that earn their cost — versions, ranges,
resolution — have nothing to resolve.

What git gives instead is not a compromise on availability or on integrity. A
package is pinned twice: to a commit id, and to the SHA-256 of the tree that
commit produced (`lock.rs` explains why the second is not redundant). Fetching
inherits whatever git the person already has configured, which is more
authentication and proxy handling than this project would write.

What it genuinely gives up is **discovery**. There is no `khora search`, and
there cannot be until something has a list. That is the cost, and it is
accepted for now rather than worked around; a curated list in the
documentation is honest, and a fake index would not be.

## Why `publish` defaults to no

This is the opposite of Cargo's default, and for the opposite reason.

Publishing to crates.io is an **act somebody performs**. You run a command, it
uploads, and the package exists. Opting *out* is the right shape there, because
the default state is "not published" and reaching the other one takes work.

Publishing here is **passive**. Push a repository and it is already fetchable
by anyone who knows the URL — there is no upload, so there is no moment at
which somebody decided. The active choice is the one that should have to be
written down, and by that reasoning the flag has to default to no.

The second reason is repositories that are not only a package. Khora's own
holds a compiler, `std`, three examples, four benchmarks and exactly one
library. `publish = true` is how a repository says which of the things in it
are meant to be depended on; defaulting to yes would advertise the lot,
including the half-finished directory somebody pushed on a Friday.

### It is an intent marker, not a permission

Worth being plain about, because the flag could be mistaken for access control
and it is not one:

- Anybody can set it on their own package.
- Anybody can write a `[dependencies]` entry by hand whatever it says — the
  refusal is in Khora's resolver, not in git.
- A `path` dependency ignores it entirely. That is your own working copy, and
  asking you to publish to yourself would be nonsense.

What it prevents is depending on somebody's *application*, or their unfinished
experiment, because it happened to be in a repository you fetched. That is a
real and common mistake, and a one-line declaration is a fair price for it.

## Why `subdir` had to exist

**A git URL names a repository, and a repository is not a package.** The two
coincide only in the simplest layout, and Khora's own does not: `packages/
postgres` sits beside a compiler. Without `subdir` the resolver reads the
`khora.toml` at the checkout root, finds a different package or none, and the
one you wanted is simply unreachable — there is no way to spell it.

The lockfile records it in the existing `path` field rather than a new one. For
a git package that field has no other meaning, and the pair (`source`, `path`)
already reads as "where, and where inside". Two packages from one repository at
one revision differ only by it, which is why it has to be recorded rather than
recomputed.

## What `khora install` is

A convenience over editing `khora.toml`, and only that. It fetches before it
writes, so a URL that does not offer a package leaves the manifest untouched.
Three things it knows that a person editing by hand does not:

- **The package's real name.** A dependency's key has to match what the package
  calls itself. Guessing from the last segment of a URL is right often enough
  to be a trap.
- **Whether it is offered.** Better found before the entry is written than
  after the build fails.
- **Whether `subdir` is needed.** Forgetting it otherwise produces a confusing
  error about the wrong package.

With no URL it fetches and locks what the manifest already declares, which is
what to run after cloning a project. A **bare name is refused**, with a message
saying there is no registry — rather than a built-in table of well-known names,
which would be a registry with none of the parts that make one trustworthy.

The manifest edit is textual, not a TOML round trip: a manifest is a file a
person wrote, with their comments and ordering in it, and reformatting the
whole thing to add one line is not a fair trade. Installing over an existing
entry replaces it in place — two entries of one name is a TOML error, so
appending would produce a manifest that no longer parses — and installing the
identical entry twice reports that nothing changed rather than claiming a
change.

## What this defers, and what would force it

| Deferred | What would force it |
| --- | --- |
| A registry | Enough packages that a curated list stops being one, or a demand for names independent of hosting |
| Versions and a solver | Two packages in real use wanting different revisions of a third. Today that is an error naming both askers, which is the honest answer with nothing to solve |
| `khora search` | A registry. There is nothing to search until something holds a list |
| Yanking, signing, provenance | A registry, and 13.21 |
| Namespacing | A registry. A URL is already globally unique, which is most of what namespacing buys |

The version question is the one to watch. `publish` and `version` are already
recorded, and a package that has been offered under a version number for a
while is a package whose users will eventually want ranges over it. That is
where a solver arrives, and `resolve` is where it goes.
