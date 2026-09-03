---
title: Debugging a program
sidebar:
  order: 21
---

Khora compiles to an ordinary native executable with ordinary debug information, so the tools you already have work on it. This page says which parts of that are tested and which are not, because "should work" and "is checked by CI" are different claims and only one of them is worth acting on.

## Backtraces

This is the first thing to reach for and the part that is covered by tests.

A trap — a checked overflow, an index outside its array, a division by zero — prints the Khora source location that caused it:

```text
khora: Int addition overflowed
   6: deep
             at .\main.kh:4
   7: middle
             at .\main.kh:8
   8: main
             at .\main.kh:13
```

The frames below `main` belong to the C runtime that started the process and are not trimmed. Backtraces are off by default, because capturing one costs every well-behaved program a page of stack on the way out and the first thing anybody does with a bug is run it again. A trap without them says how to get them:

```bash
KHORA_BACKTRACE=1 ./build/myapp
```

`RUST_BACKTRACE` is honoured too, so a machine that already exports it for everything is not asked twice.

**A failed assertion is not a trap.** `assert` names the line that failed and
the run carries on to the end, because a suite that stopped at the first
failure would report one problem per run. It prints no backtrace and the
process does not end with status 134. [Traps](/docs/reference/traps/) is the
list of what does.

**The runtime's own frames are not at the top.** The frames belonging to the backtrace machinery are trimmed, because the top of a backtrace is the part anybody reads first and six frames of library internals above the line that trapped make it useless. That trimming is tested.

## Debug information

Debug builds emit DWARF line tables on Linux and macOS, and a PDB beside the executable on Windows. A release build emits neither.

```bash
khora build .                   # debug information
khora build . --release         # none
KHORA_DEBUG=1 khora build . --release   # release code, debug information
KHORA_DEBUG=0 khora build .             # debug code, none
```

`KHORA_DEBUG` overrides the profile in both directions. It is part of the build cache key, so switching it does not hand you the other build's artifact.

**On Windows, debug information costs reproducibility.** Relinking one unchanged object twice gives identical bytes without `-g` and different bytes with it — what varies is inside lld-link's PDB emission, and no linker flag available here fixes it. A build that is reproducible only without debug information is a real limit and it is named here rather than worked around.

## LLDB and GDB

A Khora executable is a native executable with standard debug information, so a debugger loads it, breaks on a symbol, and shows source lines:

```bash
khora build .
lldb ./build/myapp
(lldb) breakpoint set --file main.kh --line 12
(lldb) run
```

Linker symbols carry the module that defines them and a `kh$` prefix — `kh$myapp$main$handle` for `handle` in `myapp::main`. The debug information records the source name as well, which is why a backtrace prints `handle` rather than the mangled form, but breaking by file and line avoids having to find out which one your debugger wants.

**What is not verified.** Nothing in CI drives a debugger, so stepping, frame inspection and variable display are not covered by any test. Line tables are emitted and the linker is told about them; whether every Khora construct produces a frame a debugger renders usefully is unmeasured. Treat this section as a starting point rather than a supported workflow, and prefer a backtrace or a `print` when you need an answer you can rely on.

This is [release-gate item §9](https://github.com/codyspate/khoralang/blob/main/docs/release-readiness.md) and it is not ticked.

## Printing

`print` is not a debugging tool with a bad reputation here; it is the tool with the best coverage. String interpolation calls `Show`, so any type that derives it can be printed whole:

```khora
print("entry ${entry} after ${Int::to_string(applied)} steps");
```

Deriving `Show` on a type you are chasing costs one line and survives the session.

## When a fiber is involved

A trap inside a fiber names that fiber's stack, not the parent's. If a program hangs rather than traps, the usual cause is a fiber waiting on something that will not arrive — a channel nobody sends to, or a nursery whose child is blocked in a call with no cancellation point in it. [Concurrency](/docs/reference/concurrency/) has the rules for where a fiber can be stopped; a `!` on a fallible call is the only place.

## Reporting what you find

```bash
khora --version
```

prints the version, the commit it was built from and the target triple — `khora 0.1.0 (a416574) x86_64-pc-windows-msvc`. Include it. Two builds of the same version from either side of a fix are otherwise indistinguishable in a bug report, and a `-dirty` marker tells both of us that the tree had uncommitted changes.
