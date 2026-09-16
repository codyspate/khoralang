---
title: Packages
description: What ships alongside the language, and how a manifest names it.
---

Khora's standard library holds the contracts that two pieces of software must
agree on: `Db`'s shape, the trace vocabulary, the row and cell types a result
set is made of. **The things that speak a protocol are packages**, because a
protocol is versioned by somebody else and a release schedule that is not
Khora's should not be able to hold up a compiler release.

That split is settled in [the ecosystem design
note](https://github.com/codyspate/khoralang/blob/main/docs/design/ecosystem.md),
and it means a real program reaches for packages early. These pages document
the ones maintained in the Khora repository.

## Naming one in a manifest

**There is no registry yet.** A dependency names where it lives — a
repository, a revision, and the directory the package's own manifest sits in:

```toml
[dependencies]
postgres = { git = "https://github.com/codyspate/khoralang", rev = "main", subdir = "packages/postgres" }
```

`khora.lock` records the commit each dependency resolved to, so a build stays
reproducible even when the revision is a branch name. The full set of
dependency keys is in [the manifest
reference](/docs/reference/manifest/).

A registry is a 1.0 question. Until there is one, `git` and `subdir` are the
whole mechanism, and a monorepo of packages works because `subdir` exists.

## What is here

- **[`postgres`](/docs/packages/postgres/)** — a PostgreSQL client that speaks
  the wire protocol directly, with no `libpq` to install.
- **`ai`** — the effect a caller names when it wants model inference, so the
  provider stays the caller's choice.
- **`otlp`** — an exporter for the trace vocabulary in `std::trace`.

## These are not covered by the language's compatibility promise

[Compatibility](/docs/reference/compatibility/) is a statement about the
language and `std`. A package in this repository is versioned with the
compiler for convenience, not as a promise: it can break between releases when
the protocol underneath it changes, and the reason to keep it out of `std` is
exactly that it might need to.

Read each package's own page for what it does and does not do yet. The
honest list of gaps is at the bottom of each one.
