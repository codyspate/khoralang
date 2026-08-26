# Generated API documentation

Roadmap 13.15's "standard-library API reference". `khora doc` reads the
comments already in the source and writes one markdown page per module.

**This is what was decided while building it.** `docs/design/auto-documentation.md`
is the plan for the parts that are not built — a JSON output for consumers
other than this site, per-symbol records with source locations and
cross-references, and compiled examples. Where the two disagreed, that page now
carries the correction.

## Decided

**`///` documents the declaration below it. `//!` documents the module.**
Everything else stays a `//` comment and is never published.

```khora
module std::decimal;

//! Exact decimal arithmetic, for the numbers that are counted
//! rather than measured.

// NOTE: ten_to is a table and not a loop, because scale is
// bounded at 18.        <- a note to a maintainer; stays private

/// A number counted in steps of `10^-scale`.
export type Decimal = { units: Int, scale: Int };
```

`khora doc std --out website/content/docs/stdlib/api` writes the pages;
`--check` writes nothing and fails if they are stale, which the baseline runs.

## Why a marked module comment was needed

`///` was already the convention and needed no decision. Module-level prose was
the problem: seventeen of eighteen `std` files opened with a `//` block
explaining what the module is, and those blocks are the best writing in the
project — but a generator cannot tell them from the note two lines further down
saying which function is slow.

Three ways to resolve it, and the trade is the same one every time:

| | Cost | What it gets wrong |
| --- | --- | --- |
| Adopt `//!` | Rewrite seventeen file headers | Nothing |
| Publish the first `//` block | None | Cannot distinguish documentation from an internal note, and several headers mixed both |
| Skip module prose | None | Every module page opens with no explanation, which is the part a newcomer reads |

`//!` was chosen. It needed no lexer change -- `//!` already lexes as a line
comment -- so the whole cost was the rewrite, and the distinction it buys is
permanent: **`//!` and `///` are published, `//` is not**, and a person editing
a file can see which they are writing.

## Why the syntax tree and not the HIR

Because the HIR does not contain the API. `collect_decl` returns early on
`Decl::Impl` -- an impl has no name of its own, and recording one would give
two impls for the same type a spurious duplicate-name error -- so **every
method in `std` is absent from `item_map`**. `std/core.kh` declares nine
top-level functions and two hundred and thirty-seven methods. A reference built
on the HIR would document the nine.

The syntax tree also has the thing the HIR discards on purpose, which is the
comments. Rowan keeps every byte, so a `///` block is still in the tree as
trivia in front of the declaration it belongs to.

## What is documented

Exported items, and the members of exported types and traits.

**`export` means nothing inside an `impl`.** Nothing in the compiler reads it
there: `std` never writes it and `packages/postgres` always does, and both
compile. Rather than pick a side of an inconsistency that is really roadmap
13.11's to settle, an impl's methods are documented when the impl's *self type*
is exported, and the keyword on the method is ignored. That answers correctly
for both files today, and will keep answering correctly if 13.11 gives the
keyword a meaning, because an unexported type's methods are unreachable either
way.

An impl written for a type the file does not declare -- somebody else's trait
for somebody else's type -- is not this module's API and is skipped.

## Three shapes of signature

- A **function** is its signature and never its body, collapsed to one line. A
  wrapped signature is a formatting decision about a source file; a reference
  wants one line to scan.
- A **type**, **effect** or **constant** is printed as written. Its shape *is*
  the documentation, and `khora fmt` has already made that text canonical.
- A **trait** or **impl** is its header only, because what is inside gets its
  own entry.

A field or a variant case with nothing said about it is not given an entry of
its own. It is already in the signature block directly above, and a `Fields`
section restating `units: Int` buries the ones that do have something to say.

## Several files, one module

`std::net::socket` is written three times, once per platform, and exactly one
is ever compiled. Two consequences, both of which had to be handled or the
published reference would depend on who ran the command:

**Documentation reads every platform's files**, unlike a build, which reads
only the host's. Otherwise the page for `std::net::socket` would be whichever
platform generated it, and `--check` would fail in CI on Linux against pages
generated on Windows.

**Every distinct `//!` block is kept.** Each variant's block describes that
variant -- "Sockets, on Linux", "Sockets, on Windows" -- so publishing the
first and dropping the rest would present one platform's notes as the module's,
with alphabetical order deciding which. The `std` files were rewritten to suit:
the alphabetically-first variant carries the module's own opening and a
`# On Linux` section, and the others are `# On macOS` and `# On Windows`.
Identical blocks are not repeated, so a module whose variants agree reads as
one file.

Items are keyed by name, earliest wins, and a function only one platform has
still appears -- it is part of the API on that platform, and silence would be
worse than a line saying so.

## The output is a pure function of the input

No timestamp, no compiler version, no path from the machine that ran it.
Regenerating after a change produces a diff that is exactly the change, and
regenerating after no change produces no diff at all.

Both halves of that matter. A generated file that always differs from itself
cannot be reviewed, and `--check` would fail on every commit and be turned off
within a week. The output directory is also *owned* by the command: a page for
a module somebody deleted is removed, because a page describing code that no
longer exists is worse than no page.

## What this defers

| Deferred | Why, and what would force it |
| --- | --- |
| **Compiled examples** | The next slice. Today a ` ```khora ` block in a doc comment is prose, and an example can be wrong without anything noticing. This is the one that matters: it is the difference between documentation that rots and documentation that cannot |
| Cross-links | A signature mentioning `Decimal` names it but does not link to it. Wants a resolved index, which is `module_api` plus the impl methods it currently drops |
| Documenting private items | `--private`, for reading a module rather than using it |
| Packages | `khora doc packages/postgres` already works; nothing publishes the result yet, which is 13.13's question rather than this one |
| A search index | Starlight builds its own from the pages |

Compiled examples are the one to do next, and the reason is the reason this
document opens with: a reference is only worth having if a reader can trust it,
and prose that claims to be code is the part nothing currently checks.
