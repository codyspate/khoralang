---
title: Lints
sidebar:
  order: 20
---

`khora check` runs twelve lints alongside type checking. They are part of the compiler rather than a separate tool, so the editor underlines what the command line reports and there is no second configuration to keep in step.

## The lints

| Name | Default | What it finds |
| --- | --- | --- |
| `dangling-expression` | `warn` | A statement that computes something and does nothing with it. |
| `discarded-result` | `warn` | A statement that produces a `Result` and drops it on the floor. |
| `inconsistent-constructor` | `warn` | A constructor whose name disagrees with what it takes — `new`, `empty`, `root` and `of` follow a rule `std` keeps. |
| `misplaced-main` | `warn` | A `main` in a file that is not an entry point. |
| `reference-cycle` | `warn` | A cycle that reference counting cannot collect. |
| `undocumented-export` | `allow` | A `pub` item nobody described in one line. |
| `unknown-allow` | `warn` | A `// @klint allow` naming something that is not a lint. |
| `unreachable-code` | `warn` | A statement that cannot run, because the one before it left the block. |
| `unused-binding` | `warn` | A binding nothing reads — locals, parameters, and the names a pattern binds. |
| `unused-capability` | `warn` | A capability a signature asks for that its body cannot be using. |
| `unused-import` | `warn` | An imported name the file never mentions. |
| `useless-allow` | `allow` | A `// @klint allow` that suppressed nothing. |

## Levels

```toml
[lints]
unused-import = "deny"
undocumented-export = "warn"
unreachable-code = "allow"
```

| Level | Effect |
| --- | --- |
| `allow` | Not reported. |
| `warn` | Reported; `khora check` still succeeds. |
| `deny` | Reported as an error; `khora check` and `khora build` fail. |

The table form carries an option alongside the level, for a lint that takes one:

```toml
[lints]
some-lint = { level = "warn", max = 15 }
```

No shipped lint takes an option yet. The manifest accepts the form so that adding one does not need a manifest change.

In a workspace, `[workspace.lints]` sets the defaults a member inherits with `lints.workspace = true`.

Naming a lint that does not exist is a warning, and the message lists the ones that do — a typo would otherwise configure nothing and say nothing, which is the worst direction for a setting to fail in, because a setting you have written down is one you have stopped thinking about. It is a warning rather than an error because a manifest may be older or newer than the toolchain reading it.

## The two that are off

**`undocumented-export`** is off for the reason Rust's `missing_docs` is: a young package gets forty warnings on its first build, and the answer to forty warnings is not forty doc comments. Switch it on when a package decides its surface is a promise. This repository sets it to `deny`, because `khora doc` regenerates the reference from `///` comments and the gate fails on a stale page — so a *documented* export cannot drift, and nothing else checked that an export was documented at all.

**`useless-allow`** is off because it fires on exactly the lines somebody is already editing to satisfy a new lint, so turning it on while lints are still being added produces churn in the files under the most pressure. Turn it on once they have settled; a stale suppression hides the next finding on that line.

## Suppressing one line

```khora
// @klint allow unused-binding
let width = measure(shape);
```

The pragma applies to the line that follows it. `unknown-allow` is what makes it safe to have: a misspelled lint name in a comment would otherwise suppress nothing and say nothing, and the reader would believe the line was handled.

Prefer the naming escape where one exists. A binding whose name starts with `_` is deliberately unused, and `_` alone binds nothing — both are quieter than a pragma and neither goes stale.

## Where they run

- `khora check` and `khora build`, against the manifest nearest the file.
- The language server, at the same levels, so the editor and the command line agree.

There is no `khora lint`. A separate command is a second thing to run and a second answer to disagree with the first.
