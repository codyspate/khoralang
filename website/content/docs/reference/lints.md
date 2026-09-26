---
title: Lints
sidebar:
  order: 21
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

## Groups

A group is a named set of lints, switched on together:

```toml
[lints.idiomatic]
```

A group's table present means the group is on, and **each lint in it runs at the level the group gives that lint**, which can differ from lint to lint. The group's table absent means the group is off, and its lints are at their own defaults from the table above.

`level` applies one level to every lint in the group, and is optional:

```toml
[lints.idiomatic]
level = "deny"     # allow | warn | deny
```

`level = "allow"` switches the group's lints off explicitly, each of them, including one another enabled group also holds (see below).

### Which level wins

Most specific first:

1. a `// @klint allow` on the line;
2. the lint's own entry in `[lints]`;
3. an enabled group's `level`;
4. the level an enabled group gives the lint;
5. the lint's default.

The order of tables in the file never matters. A group that turns `unused-binding` up to `deny` is overridden by `unused-binding = "warn"` under `[lints]`, above or below the group's table.

Steps 3 and 4 each look at every enabled group that holds the lint. If any of them writes `level`, step 3 decides: the written levels settle the lint, and the other groups' own levels for it are not consulted. Only when none of them writes `level` does step 4 decide, from the levels the groups give the lint. Two groups conflict only when they disagree at the step that decides: two written `level`s that differ, or, with no `level` written, two groups that give the lint different levels. A conflict is an error naming both groups and the lint, and one line under `[lints]` settles it.

`level = "allow"` sets every lint in that group to `allow`, including a lint another enabled group holds. With these two groups:

```toml
# lints/a.toml holds unused-binding = "deny"
# lints/b.toml holds unused-binding = "warn"
[lint-groups]
a = "lints/a.toml"
b = "lints/b.toml"

[lints.a]

[lints.b]
level = "allow"
```

`unused-binding` is `allow`, with no error: `b`'s written level decides at step 3, ahead of `a`'s `deny` at step 4. To keep `a`'s `deny`, write it for the lint itself:

```toml
[lints]
unused-binding = "deny"
```

### What a group's table refuses

Each of these stops `khora check` and `khora build`, with a message naming the manifest and the key:

| Written | Why |
| --- | --- |
| `idiomatic = "warn"` | A group is always a table. The message shows the table form. |
| `enabled = true` | Writing the table switches the group on; `level = "allow"` switches it off. |
| `exclude`, `rules`, `extends` | Reserved for a later version. |
| any other key | A group's table takes `level` and nothing else. |
| `"acme::strict"` | Groups published in packages are not yet supported. |

A lint's own table still needs its `level`: `[lints.unused-binding]` with nothing under it is an error.

### The group file

A group is a TOML file:

```toml
[group]
name = "strict"
description = "What this project holds itself to."

[group.lints]
unused-binding = "deny"
unused-import = "warn"
undocumented-export = "warn"
```

`[group.lints]` lists each lint in the group with the level it runs at when the group is on. `name` must match the name the group is used by. There is nothing else in the file: a group combines lints and never defines one.

The toolchain ships its groups as files of this kind in `lints/` beside the standard library. A project adds its own under [`[lint-groups]`](/docs/reference/manifest/#lint-groups--this-projects-own-lint-groups) in its manifest:

```toml
[lint-groups]
strict = "lints/strict.toml"
```

Both kinds are used the same way. The rules for both:

- every entry in `[group.lints]` must be a lint from the table above; a group inside a group is refused;
- a project's group cannot take the name of a lint or of a group the toolchain ships;
- a file that is missing or does not parse is an error naming the file, never an empty group.

### The groups that ship

| Group | What it holds |
| --- | --- |
| `idiomatic` | One way to write Khora. It holds no lints in this release, so switching it on changes no level. |

A `// @klint allow` names one lint. Naming a group there is reported by `unknown-allow`, which says it is a group and lists its lints.

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

**The receiver of a trait method is the exception.** `self` is part of the
signature: it cannot be deleted, and renaming it to `_self` is refused with
``error: the receiver of `tag` is `?` here, but `Codec` declares `A``. A trait
method that ignores its receiver — common in a witness trait — takes the pragma:

```khora
impl Codec for IntCodec {
  // @klint allow unused-binding
  fn encode(self, value: Int) -> String { Int::to_string(value) }
}
```

## Where they run

- `khora check` and `khora build`, against the manifest nearest the file.
- The language server, at the same levels, so the editor and the command line agree.

There is no `khora lint`. A separate command is a second thing to run and a second answer to disagree with the first.
