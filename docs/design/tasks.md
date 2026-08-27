# Tasks

`khora task <name>` runs a task from `[tasks]`, and everything it depends on
first. Roadmap 14.18.

**It was `khora run` for a day.** `run` is what every other toolchain means by
"build this program and start it", and it is the first command a newcomer
types; a language that spent it on a task runner would be surprising in the
worst possible place. `khora run` is now the program runner and this is
`khora task`. Renamed while the cost was one commit.

```toml
[tasks.migrate]
description = "Apply pending database migrations"
run = "khora build . && ./src/migrate"

[tasks.ci]
description = "Run the full CI pipeline"
depends_on = ["lint", "test", "build"]
```

The table has been in the manifest since §4.1 was written, and
`khora_pkg::tasks::plan` has ordered it and refused cycles for as long as it
has existed. Until now nothing ran it.

## `run` is not `build.rs` coming back

`docs/project.md` §4.1 replaces arbitrary build-time host code with sandboxed
WASM plugins, and the whole point of that decision is that **fetching a
dependency must not run its code on your machine**. A task runner that shells
out looks, at a glance, like the thing that decision was against. It is not,
and the difference is worth being exact about:

- A task runs only when somebody types `khora task <name>`.
- It runs the `[tasks]` table of the manifest they are standing in, or of the
  members of the workspace they are standing in.
- **A dependency's `[tasks]` table is never read.** `khora_pkg::resolve` looks
  at `[dependencies]` and nothing else; nothing in resolution, fetching,
  checking, building or testing reaches a task.

So the trust boundary is unchanged: code you fetched still cannot run, and code
you wrote in a file you are looking at still can. What `[build] plugin` does is
different work — it runs *during* a build, for everybody who builds, and that
is why it is sandboxed.

## What a name runs

Three clauses, in order:

1. A declared task with a `run` runs it, through the platform shell — `cmd /C`
   on Windows, `sh -c` elsewhere. A task meant to be portable should invoke
   `khora` rather than shell built-ins.
2. A task with no `run` whose name is one of the toolchain's own verbs runs
   that verb, as *this* executable rather than whatever `khora` is on the
   `PATH` — somebody running a freshly built compiler out of `target/debug`
   should get that one.
3. Anything else is a grouping and runs nothing of its own. `ci` exists to
   depend on three other things, and the run says so rather than looking like
   it did something.

Clause 2 has two edges worth naming.

**`lint` runs `khora check`.** The lints run inside the check —
`khora_lint::findings` is called from it — and there is no `khora lint`. §4.1's
own example depends on `lint`, so the name has to mean something, and "the
lints pass" is what it means. The substitution is printed rather than done
quietly:

```
$ khora check   (`lint` runs inside it)
```

**`fmt` formats.** It does not check. A task that wants the check writes
`run = "khora fmt . --check"`. A verb that quietly does something other than
what its name says is worse than one that surprises you into a diff you can
see and revert.

## Across a workspace

A task the root manifest itself declares is a **workspace** task and runs once,
at the root. `ci` at the top of a monorepo means "run the pipeline", not "run
something called `ci` in each of eight members".

Otherwise the task runs in every member that has something to run for it, and
in **dependency order**: a member another member depends on goes first.

That order is not inferred from imports. `khora_pkg::resolve` already reports
the directories each member compiles, because a build needs them, so a member
whose directory appears among another member's dependencies is one that has to
go first. It is the same fact 14.16 uses to answer "what does this diff
affect", read the other way round.

`--since <rev>` narrows the members the same way `khora check --since` does,
including the rule that a change nothing in the workspace owns selects every
member.

A member with nothing to run for the goal is skipped silently; a goal that no
member has anything to run for is an error, because a workspace command that
did nothing and exited zero makes "nothing to do" and "everything passed" look
the same.

The run stops at the first member that fails. A monorepo *check* should carry
on and report everything (14.13 does), but a task DAG should not: the later
members are the ones that depend on the earlier, so running them after a
failure produces cascading noise from one cause.

## What this does not do yet

- **No parallelism.** The plan is a DAG and independent branches could run at
  once. Sequential first, because a task runner whose interleaved output cannot
  be read is worse than a slow one, and the answer to that is per-task output
  capture, which is its own piece of work.
- **No caching.** Re-running `khora task ci` re-runs everything, even the parts
  nothing changed under. `--since` narrows to affected *members*, which is the
  coarse half of the same idea; 14.17's content-addressed cache is the fine
  half, and it wants a sound key more than it wants to exist.
