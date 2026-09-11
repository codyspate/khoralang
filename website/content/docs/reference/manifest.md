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

**The values are member names.** A policy says which members may ask for a
category at all, not which hosts or paths they may reach:

```toml
[workspace.policy]
network = ["gateway"]
fs = ["reports"]
env = ["gateway", "reports"]
extern = ["sqlite"]
```

Only `gateway` may write a `[permissions] network` entry; only `reports` may
write `[permissions.fs]`. Neither is told anything about *which* host or path —
`network = ["*.internal:5432"]` here is not a narrower cap, it is a member name
that does not exist, and the root is refused with `` `*.internal:5432` is not a
member of the workspace ``. A typo in a cap is a cap that does not apply, so it
fails loudly at the root rather than quietly wherever it should have bitten.

A member that asks for a capped category without being named is refused, and
the message says where the cap is and what to do about it:

```text
`cli` is not allowed to grant `fs`. The workspace at .../khora.toml caps it to
reports. Add `cli` to `[workspace.policy] fs` if it should be, or drop the grant
```

A category the policy does not mention is uncapped. This is the one place a
workspace overrules a member rather than offering it something.

**`process` is not one of the categories a policy caps.** The key parses, and a
name in it that is not a member is refused like any other, so it reads as though
it works. It does not: the check that refuses a member for granting a capped
category runs over `network`, `fs`, `env` and `extern`, and never over
`process`. A root that writes
`process = ["gateway"]` does not stop any other member writing
`[permissions] process`. Leave it out rather than relying on it, and cap the
grant in the member's own manifest.

Capping the *values* — "no member may reach anything outside `*.internal`" — is
a different feature and is not here: it needs a rule for when one glob is
narrower than another, and a version of that rule that is subtly wrong is a cap
that looks enforced and is not.

### `--since` — building only what changed

`khora check`, `khora fmt` and `khora task` take `--since <REV>` at a workspace
root, and `khora release` requires it:

```bash
khora check --since main
khora task test --since origin/main
khora release --since v0.3.0
```

`<REV>` is anything `git diff` takes — a branch, a tag or a commit. The
selection is exact rather than heuristic: the resolver already knows which
packages each member compiles, so it takes the changed files, finds the members
they belong to, and adds every member that depends on one of those. A workspace
of thirty packages where two changed checks two.

A changed file that belongs to no member, and to nothing a member depends on —
the root `khora.toml`, a CI script — selects *everything*, and the output says
which file did it. That is the safe direction: a build tool that guesses a file
does not matter is a build tool that skips the check that would have caught it.

## `[permissions]` — what the code may reach

Khora has no ambient authority: a function that touches the network says so in its type, and this table is where a package's grants are written down.

```toml
[permissions]
default = "deny"
network = ["api.example.com:443", "*.internal:5432"]
env = ["HOME", "DATABASE_URL"]
process = ["git", "docker"]
extern = ["sqlite_sys"]

[permissions.fs]
read = ["data/**", "logs/*.log"]
write = ["logs/**"]
```

| Key | Value |
| --- | --- |
| `workspace` | `true` to take the root's table whole. |
| `default` | What a category nobody wrote down grants: `allow` or `deny`. `allow` is the default, so a program that has never heard of permissions compiles. `deny` is the strict posture: one line, set once, and every capability after it is a deliberate edit. |
| `network` | Hosts the program may **connect out to**, as `name` or `name:port`. `*` spans dots, so `*.internal` covers `db.eu.internal`; a grant with no port covers every port. **Outbound only**: a port the program *binds* is not a host it reaches, so `Router::listen` and the rest of the server side are not covered by this key, or by any other — see [known limitations](/docs/limitations/#inbound-connections-are-not-permissioned). |
| `fs` | **A table, not a list**: `[permissions.fs]` with `read` and `write`. `*` stops at a separator and `**` crosses one, and neither covers the directory being described. |
| `env` | Environment variable names. `*` spans everything, since a name has no segments. |
| `process` | Program names, as written at the call: `run("git", ..)` names `git`. `*` spans everything, so `git*` covers `git` and `gitk`. |
| `extern` | **Package names**, not library names: which packages may declare `extern fn`. `std` always may. |

**`fs` is the one key that is a table**, because reading and writing are not the
same grant and a single list cannot say which one it is. The rest are lists. `[workspace.policy]` above takes `fs` as a *list*, because there it names
which members may grant filesystem access at all rather than which paths they
may reach.

**`process` is what stops the rest of the table being advisory.** A program
that may run another program can ask it to do anything the program itself may
not: with `read = ["data/**"]` and no `process` grant, both
`read_text("/etc/hostname")` and `checked_output("cat", ["/etc/hostname"])` are
refused. A refused program raises `ProcessError::Denied`, which is a separate
case from `NotStarted` for the reason `IoError::Denied` is separate from
`Failed`: one sends the reader to their `PATH` and the other to a line in a
file they own.

**A shell line is checked on its first word, which is a weaker promise** and is
said here rather than left to be found. `Process`'s `shell` runs a string
nobody has parsed, so `sh -c 'a; b'` runs two programs and the grant sees one.
That is the bound; `run` is the operation for a command built out of anything
that came from outside the program, and it takes its arguments as a list so
nothing re-parses them.

**`default` applies to a category you did not write down, not to one you wrote
down empty.** `network = []` grants no host; leaving `network` out entirely
takes `default`. So `default = "deny"` on its own denies every category, and
`default = "deny"` beside one `[permissions.fs]` grant allows exactly that grant
and nothing else.

## `[dependencies]` — other packages

```toml
[dependencies]
serde = { version = "1.2.0" }
shared = { path = "../shared" }
tools = { git = "https://example.com/tools.kh", tag = "v1.4.0", subdir = "core" }
```

Exactly one of `version`, `path` and `git` says where a package comes from. A `path` is resolved relative to this manifest and needs no version, because the source is right there. A `git` dependency takes `rev` or `tag`, and `subdir` when the package is not at the repository root.

**`version` has nothing to resolve against yet.** There is no public registry,
so the key is accepted and reserved rather than usable; today a dependency comes
from a `path` or a `git` URL. [Modules and
packages](/docs/reference/modules-and-packages/#dependencies) is the same point
from the other side.

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
| `target` | The triple to compile for. **Not read yet** — see below. |
| `plugin` | A build plugin, named and versioned — `protobuf-compiler@2.1`. It names a plugin rather than pointing at a script. **Not read yet** — see below. |

**Neither key does anything today, and the toolchain says so.** Both are
recognized, both are documented here because the decisions behind them are
made, and setting either gets you a warning rather than silence:

```
warning: khora.toml: 6:1: nothing reads `build.target`: cross-compilation is
not supported yet, so the build is for the host either way.
```

`target` waits on cross-compilation, which needs a linker and sysroot story
rather than code generation — see [Supported targets](/docs/deployment/supported-targets/).
`KHORA_TARGET` makes the compiler *emit* for another triple, which checks code
generation and does not produce a runnable artifact. `plugin` waits on the
sandboxed WASM plugin mechanism; until it exists, [`[tasks]`](#tasks--project-commands)
is the thing that runs commands, and it runs only what you wrote in a manifest
you are standing in.

## `[tasks]` — project commands

```toml
[tasks.migrate]
description = "Bring the development database up to date"
run = "khora run src/bin/migrate.kh"

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
