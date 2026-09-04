---
title: The manifest
sidebar:
  order: 19
---

Every Khora project has a `khora.toml` at its root. It names the package, says which compiler builds it, and declares what the code is allowed to reach. `khora new` writes one:

```toml
[package]
name = "hello_khora"
version = "0.1.0"

# Which Khora builds this project. Required.
[toolchain]
version = "0.2.0"
```

Tables may appear in any order. A key the compiler does not recognise is a warning rather than an error, so a manifest written for a newer Khora still builds with an older one — you are told what was ignored instead of being stopped.

## `[toolchain]` — which Khora builds this

**Required.** Without it, every command stops and tells you what to add.

```toml
[toolchain]
version = "0.2.0"
```

| Key | Value |
| --- | --- |
| `version` | An exact version, or `latest`, or `latest.rc`. Required. |

The version selects the compiler. Run `khora build` in a project pinned to `0.2.0` while `0.3.0` is on your path, and `0.3.0` hands the whole command over to the `0.2.0` you have installed — the build, the tests, the formatter and the editor's language server all follow the pin. A pinned version that is not installed stops the command and names it, rather than quietly building with something else.

There are no ranges. A range needs a resolver, and a resolver reintroduces the thing a pin exists to remove: two machines agreeing on a constraint and disagreeing on a compiler.

### The two channels

`latest` means the newest release installed on this machine; `latest.rc` includes release candidates.

**They are deliberately not reproducible.** They resolve when they are read, so the same commit builds under a different compiler the moment anything new is installed, and under a different one again on a colleague's machine. Write a version for a project you want built the same way twice; the channels are for testing against whatever you have.

Both resolve against installed toolchains and never over the network. Asking a server which release is newest would put a request in front of every command, including the ones your editor makes while you type. `khora update` is what makes a new toolchain available; a channel only decides which of the ones already present to run.

### Where the pin is written

The pin is found by walking up from wherever you are to the nearest manifest that has one, so in a workspace it belongs at the root and members inherit it. Two members of one workspace pinning different compilers is not a thing anybody means, and a member that repeats the root's pin has written the same answer twice in two places that can drift apart.

## `[package]` — what this is

```toml
[package]
name = "orders"
version = "0.1.0"
authors = ["A Name <a@example.com>"]
publish = true
```

| Key | Value |
| --- | --- |
| `name` | Required. The package's name, and the first segment of every module path in it, so it is an identifier: letters, digits and underscores. Not hyphens. |
| `version` | Required. A semantic version, such as `0.1.0`. |
| `authors` | A list. Defaults to empty. |
| `publish` | Whether the package is offered for others to depend on. Absent means no. |

`publish` is an intent marker rather than a permission: anybody can write a `[dependencies]` entry by hand whatever it says, and a `path` dependency ignores it because that is your own working copy. What it prevents is depending on somebody's application, or their half-finished experiment, by accident.

A manifest with no `[package]` is a workspace root, which is a normal thing to be — see below.

:::note[`edition` is gone]
It named a year rather than a compiler, nothing read it, and `[toolchain]` answers the question it was pretending to. A manifest that still has the line gets a warning saying so, and builds.
:::

## `[workspace]` — several packages, built together

A root manifest with no `[package]` of its own:

```toml
[workspace]
members = ["packages/*", "examples/*"]
exclude = ["packages/scratch"]

[workspace.package]
version = "0.4.0"
authors = ["A Name <a@example.com>"]

[toolchain]
version = "0.2.0"
```

| Key | Value |
| --- | --- |
| `members` | Globs matching member directories. A directory matches only if it has a `khora.toml`. |
| `exclude` | Globs removed from what `members` matched. |
| `package` | Values members may inherit — `version`, `authors`, `publish`. |
| `permissions` | A grants table members may take whole. |
| `fmt`, `lints` | Shared formatting and lint settings. |
| `policy` | A cap on what any member may grant. See below. |

A root does not have to declare a package, and forcing it to would mean inventing a name for something that does not exist — a name that then turns up in error messages.

### Inheriting

Nothing is inherited implicitly. A member that wants a shared value says so:

```toml
[package]
name = "alpha"
version.workspace = true

[fmt]
workspace = true
```

`workspace = true` on a whole table takes that table entire, and grants written beside it are an error rather than being silently dropped. A member that asks to inherit something the root does not define is an error too, naming the field and the table it should be in.

### `[workspace.policy]` — a cap on grants

```toml
[workspace.policy]
network = ["*.internal:5432"]
fs = ["data/**"]
```

A member may grant what it likes within the policy and nothing outside it. This is the one place a workspace overrules a member rather than offering it something.

## `[permissions]` — what the code may reach

Khora has no ambient authority: a function that touches the network says so in its type, and this table is where a package's grants are written down.

```toml
[permissions]
default = "deny"
network = ["api.example.com:443", "*.internal:5432"]
fs = ["data/**", "logs/*.log"]
env = ["HOME", "DATABASE_URL"]
extern = ["libsqlite3"]
```

| Key | Value |
| --- | --- |
| `workspace` | `true` to take the root's table whole. |
| `default` | What an unlisted category grants: `allow` or `deny`. `allow` is the default, so a program that has never heard of permissions compiles. `deny` is the strict posture: one line, set once, and every capability after it is a deliberate edit. |
| `network` | Hosts, as `name` or `name:port`. `*` matches one name segment. |
| `fs` | Paths. `*` matches within a path segment, `**` across them. |
| `env` | Environment variable names. |
| `extern` | Native libraries this package may link against. |

## `[dependencies]` — other packages

```toml
[dependencies]
serde = { version = "1.2.0" }
shared = { path = "../shared" }
tools = { git = "https://example.com/tools.kh", tag = "v1.4.0", subdir = "core" }
```

Exactly one of `version`, `path` and `git` says where a package comes from. A `path` is resolved relative to this manifest and needs no version, because the source is right there. A `git` dependency takes `rev` or `tag`, and `subdir` when the package is not at the repository root.

## `[fmt]` — how `khora fmt` writes

```toml
[fmt]
indent-style = "space"
indent-width = 2
```

| Key | Value |
| --- | --- |
| `workspace` | `true` to take the root's table whole. |
| `indent-style` | `space` or `tab`. |
| `indent-width` | A number. |

## `[lints]` — turning findings up and down

```toml
[lints]
undocumented-export = "deny"
unused-import = "allow"
```

Each key is a lint name and each value is `allow`, `warn` or `deny`. The names are in [Lints](/docs/reference/lints/). `workspace = true` takes the root's table whole.

## `[build]` — what to produce

```toml
[build]
target = "x86_64-unknown-linux-gnu"
```

| Key | Value |
| --- | --- |
| `target` | The triple to compile for. Defaults to the machine running the compiler. |
| `plugin` | A build plugin, named and versioned — `protobuf-compiler@2.1`. It names a plugin rather than pointing at a script. |

## `[tasks]` — project commands

```toml
[tasks.migrate]
description = "Bring the development database up to date"
run = "khora run --bin migrate"

[tasks.ci]
description = "What the pipeline runs"
depends_on = ["fmt", "check", "test"]
```

`khora task migrate` runs one; `khora task` with no argument lists them with their descriptions.

A task with no `run` is a grouping, which is what `ci` above is — unless its name is one of the toolchain's own verbs, in which case it runs that. So `depends_on = ["fmt", "check", "test"]` works without declaring three tasks that only say what `khora fmt`, `khora check` and `khora test` already do.

**This is not a build script.** A task runs only when somebody types `khora task <name>` in a manifest they are standing in. Nothing reaches it during resolution, fetching or building, and a dependency's tasks are never even read.

| Key | Value |
| --- | --- |
| `description` | Shown in the listing. |
| `run` | The command line. |
| `depends_on` | Tasks to run first, in order. |
