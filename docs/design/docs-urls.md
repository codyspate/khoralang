# The documentation site's URL contract

`docs/release-readiness.md` §17 ends with a sentence that decides what this
document is for:

> The frontend framework is not part of the language contract. URL structure,
> content ownership and versioning are.

Astro and Starlight can be replaced. `khoralang.com/docs/reference/traps/`
cannot, once somebody has linked to it from a Stack Overflow answer.

## The decision

**A documentation tree per stable major version, plus `next` for the version
being written, each under `/docs/<id>/`.** `/docs/` redirects to the newest
stable one — or to `next` while there is none, which is now.

`website/versions.mjs` is the list, and it is the only place the set is
written down: `scripts/sync-docs.mjs` copies each tree into its segment and
`astro.config.mjs` builds the sidebar and the redirects from the same entries.
Two files that disagreed about which versions exist would pass the link checker
and 404 the site.

### Why major versions, and not every release

A section per stable *major*. Not per patch: `0.1.1` fixing a typo does not give
a reader a different language, and a switcher listing forty entries is a
switcher nobody uses. A major is the granularity at which the answer to "how do
I do X" actually changes, which is what somebody is switching versions to find
out.

Before v1 there is no stable section at all. Everything ships from `next`, which
is what `/docs/reference/compatibility/` already promises: the language may
change, so there is nothing yet whose documentation is worth pinning.

### This reverses an earlier decision, and why

The first version of this document said `/docs/` and *not* `/docs/<version>/`,
on the grounds that versioned paths solve a problem needing two versions to
have, and that building the machinery early meant choosing between a branch per
release and a directory per release without the evidence to decide.

That argument was about *serving old versions*, and it was right about that. It
missed the half that bites first: **a reader cannot tell which version they are
reading.** The site is built from `main`; the only compiler anybody can install
is the newest release. Four independent evaluators, given nothing but these
pages and a released toolchain, each concluded the toolchain was broken rather
than that the pages were ahead of it — because six APIs the documentation
described did not exist in the compiler they had. One of them said it plainly:
*there is a good language here behind a documentation set that describes a
different version of it.*

A version segment fixes that on the first day it exists, with no second version
required. `/docs/next/` and a banner saying so is the whole of it. The choice
the earlier decision deferred — branch per release or directory per release —
is still deferred, and is still recorded below.

### The shape, and what is still undecided

`next` is written in `website/content/docs/`, where it has always been, so
`khora doc --out`, `scripts/check-docs.sh` and anybody reading files on disk
keep the path they know. A released tree becomes a copy under
`website/content/versions/<id>/`.

**That is the directory-per-release shape, chosen for the first one only.** It
is the cheaper of the two and it makes a documentation fix for an old release an
ordinary commit. What it costs is that the repository carries every version of
every page for ever, and that cost is not yet real: there are no old versions.
When there are two, and somebody has had to fix a page in the older one, the
branch-per-release question can be answered with evidence rather than taste.

### What is promised now

- `/docs/<id>/…` is the documentation for that version, and keeps resolving
  once it exists.
- `/docs/` redirects to the newest stable version, or to `next` before there is
  one.
- **Every unversioned `/docs/…` path keeps working**, redirecting into the
  current version. Those are the links already in other people's bookmarks,
  issues and answers, written before there was a segment to write; the map is
  generated from the routes that exist, so a page added tomorrow is covered
  without anybody remembering.
- **Every page says which version it is** — `next` carries a banner saying it
  describes the unreleased compiler, and a stable tree that is no longer current
  carries one saying so and linking to the one that is. The current stable tree
  carries none, because a banner on the page everybody is meant to be reading is
  a banner everybody learns to ignore, and then the one that matters is
  invisible too.
- **Every page says which commit it was built from**, linked to that commit on
  GitHub. A reader whose compiler disagrees with a page can tell in one click
  whether the page is older than their compiler or the compiler is wrong.
- The paths under `/docs/<id>/` are stable. A page that moves leaves a redirect.
  The Guide's fifteen pages are the first test of that promise and they all
  redirect: `/docs/guide/data-types` reaches the current version's
  `reference/types`, and so on for the other fourteen. The `guidePages` map in
  `astro.config.mjs` is the list, and a page removed in future joins it rather
  than replacing it.
- The short paths — `/install`, `/guide`, `/reference`, `/stdlib`,
  `/versioning`, `/limitations`, `/releases`, `/source`, `/security`,
  `/contributing`, `/changelog` — are stable and are the ones to paste into a
  chat window. They follow `/docs/` to whichever version is current. `/guide`
  outlived the section it was named for and now reaches the Reference: a short
  path is a promise about where somebody lands, not about what the destination
  is called.

### What happens when v1 ships

- Copy the released tree into `website/content/versions/v1/`.
- Add `{ id: 'v1', label: 'v1', stable: true, from: 'content/versions/v1' }` to
  `website/versions.mjs`.
- `current` becomes `v1` by itself, because it is computed as the newest stable
  entry. `/docs/` and every short path follow it; `next` gains a line in its
  banner pointing at v1; the sidebar grows a second heading.

Nothing else changes, and nothing about the mechanism waits for that day: it is
running now with one version in it, which is the point.

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
