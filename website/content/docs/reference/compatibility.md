---
title: Compatibility and stability
sidebar:
  order: 20
---

Khora is `0.x`. It may break. This page says when, how you find out, and what 1.0 is waiting for — because "pre-1.0, anything can change" is not a policy, it is the absence of one.

## What `0.x` promises

**Within one release, everything.** A program that compiles with `khora 0.1.0` compiles with every `0.1.0` build, and the lockfile resolves the same way. Pin the toolchain and a build is reproducible.

```toml
[toolchain]
version = "0.1.0"
```

A pin that cannot be satisfied fails loudly. It never silently runs a different compiler — the toolchain shim hands over before argument parsing, and `khora toolchain which` tells you which build answered and why.

**Between releases, nothing is guaranteed** — but nothing breaks silently either:

- Every breaking change is in the [changelog](https://github.com/codyspate/khoralang/blob/main/CHANGELOG.md), under a **Breaking** heading, before anything else.
- A change that made a program *silently wrong* is listed under Breaking as well as Fixed, because code written around the old behaviour will behave differently now.
- Where a mechanical fix exists, the entry names it.

## What counts as breaking

| Surface | Breaking? |
| --- | --- |
| Language syntax and semantics | Yes |
| A `std` signature, type or behaviour | Yes |
| A `std` item's removal or rename | Yes |
| Lockfile format | Yes |
| Manifest keys the toolchain requires | Yes |
| CLI flags and their meanings | Yes |
| Diagnostic *wording* | No |
| A new lint, or a lint's default level | No |
| Compiler internals, IR, symbol names | No |
| Which fiber implementation is the default | No — a program cannot observe it |
| Anything under `docs/` in the repository | No |

A new lint can make `khora check` report something it did not report before. That is deliberate and is not treated as breaking: a lint tells you about code that was already wrong. Set its level in `[lints]` if you disagree.

## Editions

There is no editions mechanism, and there will not be one until something needs it. An edition is a promise to maintain two languages at once, and a `0.x` with three release candidates has no evidence that it is the right shape of promise. The `edition` key in `khora.toml` records which language a package was written against so that one can be introduced later without a flag day.

## What 1.0 is waiting for

1.0 means the surfaces in the table above stop changing without a major version. It is waiting on four things, none of which is a feature:

1. **A bug-discovery rate that has flattened.** The honest signal is not a feature list; it is how often a session aimed at known issues turns up something new. Recent sessions have still produced silent-wrongness bugs in trait dispatch and structured concurrency. Until that stops, a stability promise would be a promise to keep bugs.
2. **The formal soundness review finished.** All 282 `unsafe` blocks now name the invariant that makes them sound, and a gate step keeps it that way. What is missing is the other half: which *test* protects each invariant. The load-bearing ones say so; most do not. `docs/design/soundness.md` is where that lives.
3. **The scheduler measured on Linux.** Fibers are OS threads by default and the M:N scheduler is opt-in; that choice is settled for 0.1.0 and written up in `docs/design/fibers.md`. What is not settled is whether it should stay that way, and the missing evidence is the density claim on Linux — the reason the scheduler exists. The I/O backends underneath it (`poll` rather than epoll or IOCP) are the other half of that question.
4. **Use by people who did not write it.** Nothing else substitutes for it, and it has not happened yet.

The [known limitations](/docs/limitations/) page tracks the shorter-term version of the same list.

## How language changes are decided

In the open, in an issue, before the code. Changes that alter what a program means are recorded in the roadmap or in a design document under `docs/design/` in the repository, with the argument rather than only the conclusion. [`CONTRIBUTING.md`](https://github.com/codyspate/khoralang/blob/main/CONTRIBUTING.md) has the process.

One maintainer has final say. That is stated plainly rather than dressed as a committee, so you can judge the project's bus factor for yourself.

## Reporting a break

If an upgrade breaks a program and the changelog does not say it would, that is a bug in the changelog and worth reporting on its own. Include `khora --version` — it carries the commit and the target triple, which is usually enough to find the change that did it.
