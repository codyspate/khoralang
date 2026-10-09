---
title: Lints
sidebar:
  order: 21
---

`khora check` runs 21 lints alongside type checking. They are part of the compiler rather than a separate tool, so the editor underlines what the command line reports and there is no second configuration to keep in step.

## The lints

| Name | Default | What it finds |
| --- | --- | --- |
| `bool-comparison` | `allow` | `b == true` or `b == false`, which are `b` and `!b`. In the `idiomatic` group. |
| `concatenated-string` | `allow` | `"a " + x + "!"`, which is `"a ${x}!"`. In the `idiomatic` group. |
| `dangling-expression` | `warn` | A statement that computes something and does nothing with it. |
| `discarded-result` | `warn` | A statement that produces a `Result` and drops it on the floor. |
| `inconsistent-constructor` | `warn` | A constructor whose name disagrees with what it takes — `new`, `empty`, `root` and `of` follow a rule `std` keeps. |
| `method-call` | `allow` | `x.m(a)`, which is `T::m(x, a)`. In the `idiomatic` group. |
| `misplaced-main` | `warn` | A `main` in a file that is not an entry point. |
| `module-path` | `allow` | `module main;` in a package, where `khora new` writes `module <package>::main;`. In the `idiomatic` group. |
| `needless-return` | `allow` | `return e;` as a function's last statement, where the tail `e` says it. In the `idiomatic` group. |
| `nested-verdict` | `warn` | A `Result` handed to something that decides on the outer tag alone, so the inner one's failure passes as success. |
| `parenthesized-parameter` | `allow` | `fn (x) =>` with one untyped parameter, which is `fn x =>`. In the `idiomatic` group. |
| `reference-cycle` | `warn` | A cycle that reference counting cannot collect. |
| `subtraction-from-zero` | `allow` | `0 - 1` on `Int` literals, which is `-1`. In the `idiomatic` group. |
| `undocumented-export` | `allow` | A `pub` item nobody described in one line. |
| `unknown-allow` | `warn` | A `// @klint allow` naming something that is not a lint. |
| `unlabeled-flag` | `allow` | A `true` or `false` passed without a label to a parameter declared `Bool` that is not the first. In the `idiomatic` group. |
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
| `idiomatic` | One way to write Khora: `bool-comparison`, `concatenated-string`, `method-call`, `module-path`, `needless-return`, `parenthesized-parameter`, `subtraction-from-zero` and `unlabeled-flag`, each at `warn`. |

None of the `idiomatic` lints finds a mistake. Each finds correct code written in a second form where Khora has a first one, so each is `allow` until the group is switched on. `level = "deny"` makes the first form the only one a build accepts, and `khora check --fix` rewrites the rest, except `unlabeled-flag`, which is reported with no fix: which label to write is the reader's call.

A `// @klint allow` names one lint. Naming a group there is reported by `unknown-allow`, which says it is a group and lists its lints.

## The lints that are off

Two lints are off by default for reasons of their own, below. The eight `idiomatic` lints are off too, until the group is switched on; see [Groups](#groups). `unlabeled-flag` is one of them, and why it is off is below as well.

**`undocumented-export`** is off for the reason Rust's `missing_docs` is: a young package gets forty warnings on its first build, and the answer to forty warnings is not forty doc comments. Switch it on when a package decides its surface is a promise. This repository sets it to `deny`, because `khora doc` regenerates the reference from `///` comments and the gate fails on a stale page — so a *documented* export cannot drift, and nothing else checked that an export was documented at all.

**`useless-allow`** is off because it fires on exactly the lines somebody is already editing to satisfy a new lint, so turning it on while lints are still being added produces churn in the files under the most pressure. Turn it on once they have settled; a stale suppression hides the next finding on that line.

**`unlabeled-flag`** is off because the call it reports is correct. `reply(connection, "ok", false)` compiles, and a reader cannot tell what `false` switches off without opening `reply`; the lint asks for `reply(connection, "ok", keep_alive: false)`, a [labeled argument](./expressions/#labeled-arguments) the compiler checks against the declaration. It reports only a parameter *declared* `Bool` that is not the first: `fold(true, step)` passes a value of a type variable, not a switch, and `assert_that(false, "unreachable")` is about its first argument, which a label would not explain. A call through a function value is never reported, because a function type has no parameter names to write. Switch it on with the `idiomatic` group or on its own.

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

## Fixing what they find

```sh
khora check --fix
```

rewrites every finding that carries a fix, in place, then checks again. It prints each file it changed and how many fixes it made, or `nothing to fix`.

It fixes exactly what `khora check` reports: only lints at `warn` or `deny` under the manifest, never a line a `// @klint allow` covers, and only the package's own files, never the standard library or a dependency. A file with a parse or type error is left alone. Two fixes where one sits inside the other are made one after the other, so `--fix` runs until nothing is left to fix.

Before it writes anything, `--fix` checks the fixed program: a fix that would leave a file that does not parse, or an error anywhere in the package, is not made. It prints each one as `not fixed`, with the lint and the file it would have broken, and the finding stays for you to rewrite by hand. If any file in the package does not parse, nothing is fixed until it does: `--fix` says which file, and to fix the parse errors first.

Each pass is written whole or not at all. The new text of every file goes to a temporary file beside it, and only when all of them are written are they moved into place, so a full disk or a file you may not write leaves every file as it was, and `--fix` says that nothing was written. A file with a second hard link is split: the name `--fix` wrote gets the new text, and the other name keeps the old. If `--fix` is killed while it writes, your sources are untouched, and a `.<name>.khora-fix-<number>` file may be left beside one of them; it is not read as source, and can be deleted.

The lints with a fix are the `idiomatic` group's. Each is made only where the rewritten program means the same thing, and where it would not, the finding is not made or is made without a fix:

| Lint | Rewrite | Left alone |
| --- | --- | --- |
| `concatenated-string` | `"a " + x + "!"` becomes `"a ${x}!"`. A `$` that meets a `{` across a join is written `\$`, so `"$" + "{a}"` still prints `${a}`. | A chain across lines; a piece that is already interpolated; a backtick string; a character literal inside a piece; a piece holding an interpolated string; a comment inside the chain. Reported with no fix when a piece holds a call, or uses a local with no type written on it -- a `let` without `: T`, a binding in a `match`, `for` or `catch` pattern, a lambda parameter -- because `s + "!"` may be what makes `s` a `String`, and `todo() + "!"` what makes `todo()` one. |
| `needless-return` | `return e;` in last place becomes `e`; a last `return;` is deleted. | A `return` inside a lambda or a nested block, which is an early exit. Reported with no fix when the statement before it has no `;`, when `e` starts with `{`, or when there is a comment in the statement. |
| `subtraction-from-zero` | `0 - 1` becomes `-1`. | `Float`, because `0.0 - 0.0` is `+0.0` and `-0.0` is not; the fixed-width integers; `0 - x`, which differs from `-x` when `x` is the smallest `Int`. Reported with no fix on a line that starts right after a `}` with no `;`, because `-1` there subtracts from the value before it. |
| `parenthesized-parameter` | `fn (x) =>` becomes `fn x =>`. | A typed parameter, which needs its brackets; more than one parameter. |
| `bool-comparison` | `b == true` becomes `b`; `b == false` becomes `!b`, or `!(a < c)` for an operator. | `!=`. `b == true` is reported with no fix when `b` holds a call or uses a local with no type written on it, because the `==` may be what makes it a `Bool`; `b == false` is still fixed there, because `!b` says the same. Reported with no fix when the result would start with `(` on a line right after a `}` with no `;`, because `(c)` there calls the value before it. |
| `module-path` | `module main;` becomes `module <package>::main;` in `src/main.kh`, and `module <package>::<name>;` in `src/bin/<name>.kh`. | Any other file is reported without a fix, because renaming a module breaks every file that imports it; so is an entry file that another file imports, such as a test file with `import main::{helper}`. |
| `method-call` | `x.m(a, b)` becomes `T::m(x, a, b)`, where `T` is the type that declares `m`, or the trait for a trait's method (a type parameter's method is its bound's trait). Labels are kept, and a piped value keeps its slot: `v \|> x.m(a)` becomes `v \|> T::m(x, _, a)`. A chain `a.f().g()` nests, `G::g(F::f(a))`. Where the file does not have `T` in scope, an `import` of it is added. | A field holding a function, `r.f(x)`, which is not a method call. Reported with no fix where `T::m` in this file would reach something else -- a type parameter named `T`, another type named `T`, a constructor `T::m` -- or where importing `T` would clash with a name the file already has; where the receiver's type is not settled at the call; and where a comment sits between the receiver and the `(`. |

The language server offers the same fixes as one code action, "Apply idiomatic fixes", of kind `source.fixAll.khora`. An editor runs it on save when asked for `source.fixAll`; it is not offered in the lightbulb menu. It fixes what `khora check --fix` would, one pass at a time.

## Where they run

- `khora check` and `khora build`, against the manifest nearest the file.
- The language server, at the same levels, so the editor and the command line agree.

There is no `khora lint`. A separate command is a second thing to run and a second answer to disagree with the first.
