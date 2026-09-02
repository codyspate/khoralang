# The documentation site's URL contract

`docs/release-readiness.md` §17 ends with a sentence that decides what this
document is for:

> The frontend framework is not part of the language contract. URL structure,
> content ownership and versioning are.

Astro and Starlight can be replaced. `khoralang.com/docs/reference/traps/`
cannot, once somebody has linked to it from a Stack Overflow answer.

## The decision

**One documentation tree, at `/docs/`, describing the release the site was
built from — and a footer on every page saying which commit that was.**

Not `/docs/<version>/`. Not yet.

### Why not versioned paths yet

Versioned documentation solves a problem that requires two versions to have.
A reader on `0.1.0` needs `/docs/0.1.0/` when `0.2.0` has shipped and `/docs/`
has moved on without them. Before the first release there is no *them*, and the
machinery to serve a version that does not exist would be machinery nobody
could test against a real second version.

Building it now would also mean choosing, without evidence, between the two
shapes it can take:

- **A branch per release**, built from a tag. Correct, and it means a
  documentation fix for an old release is a commit on a branch nobody is
  working on.
- **A directory per release in the tree.** Cheaper to build and it makes the
  repository carry every version of every page for ever.

The right answer depends on how often documentation is fixed *after* a release,
which is a fact this project does not have yet.

### What is promised now

- `/docs/…` is the documentation for the release named in the footer.
- **Every page says which commit it was built from**, linked to that commit on
  GitHub. A reader whose compiler disagrees with a page can tell in one click
  whether the page is older than their compiler or the compiler is wrong.
- The paths under `/docs/` are stable. A page that moves leaves a redirect.
  The Guide's fifteen pages are the first test of that promise and they all
  redirect: `/docs/guide/data-types` reaches `/docs/reference/types`, and so on
  for the other fourteen. The `guidePages` map in `astro.config.mjs` is the
  list, and a page removed in future joins it rather than replacing it.
- The short paths — `/install`, `/guide`, `/reference`, `/stdlib`,
  `/versioning`, `/limitations`, `/releases`, `/source`, `/security`,
  `/contributing`, `/changelog` — are stable and are the ones to paste into a
  chat window. `/guide` outlived the section it was named for and now reaches
  the Reference: a short path is a promise about where somebody lands, not
  about what the destination is called.

### What is promised at the first release that is not the last

When `0.2.0` ships:

- `/docs/` becomes the newest release, as now.
- `/docs/0.1.0/` starts resolving, and keeps resolving.
- `/docs/next/` starts resolving from `main`, with a banner on every page
  saying it describes unreleased work.

Until then `/docs/` *is* the development documentation, and the pre-1.0 banner
that already appears on every page says so.

## Content ownership

The pages under `website/content/docs/` are written by hand and are owned by
whoever changes the behaviour they describe. The pages under
`website/content/docs/stdlib/api/` are **generated** by `khora doc` and are
owned by the `///` comments they came from; `scripts/baseline.sh` fails when
they are stale, so editing one by hand is a change that gets reverted by the
next build.

## What enforces this

`website/scripts/sync-docs.mjs` runs before every build and:

- copies `website/content/docs/` into the Astro collection, which is what makes
  the tree above the source of truth rather than a mirror of it;
- **fails the build on a broken internal link**, including a link written to a
  `.md` source file rather than to the route it renders as;
- writes `src/generated/build.js` with the commit, the release and the time,
  which the footer reads.

The link check has caught exactly the mistake it is for and has also once been
wrong about it: it rejected `https://github.com/…/CONTRIBUTING.md`, which is an
external link to a file meant to be read as a file. Three of those were added
in 13.14 and 13.15 and the site did not build for a week, because CI runs on a
push and `scripts/baseline.sh` did not build the site at all. It does now.
