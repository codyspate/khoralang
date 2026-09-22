---
title: Environment variables
sidebar:
  order: 24
---

Khora reads a handful of `KHORA_*` variables. This page is the complete list of the ones you may set: what each does, what it does when unset, what values it accepts, and what it costs where that is not obvious.

**A variable not on this page is not a knob.** The compiler and the runtime read a few more names — test scaffolding, and one or two things that reach a program by accident rather than by design. They are not listed here because setting them is not supported and they may change or disappear without notice. A check in the repository fails when a name read by shipped code is neither documented here nor recorded internally, so this page cannot quietly fall behind the source.

Every variable here is read once, when the thing that reads it starts. Changing one mid-run changes nothing.

## Where Khora keeps things

### `KHORA_HOME`

The one directory Khora owns on the machine. Everything that persists between builds lives under it:

| Under `KHORA_HOME` | What it holds |
| --- | --- |
| `toolchains/` | Installed compilers, one directory per version |
| `bin/` | The `khora` the installer puts on your `PATH` |
| `cache/` | Build artifacts, keyed on the inputs that produced them |
| `store/` | Extracted package sources |

**Default:** `~/.khora` — `$HOME/.khora`, or `%USERPROFILE%\.khora` on Windows.

**Value:** an absolute path. It is used as given, and directories under it are created as needed; it does not have to exist beforehand.

```bash
export KHORA_HOME=/data/khora
khora build .
```

Moving it is an ordinary thing to do. The build cache is the part that grows — see `KHORA_CACHE_BUDGET` below — and a machine whose home directory is on a small root disk will want `KHORA_HOME` pointed somewhere with room. Everything above moves together: a toolchain installed under one `KHORA_HOME` is invisible under another, and so is every cache entry, so the first build after a move rebuilds from scratch.

**What it costs.** Setting it per-shell rather than per-machine is the way to get two of everything. `khora` resolves it at the moment it runs, so a build in a shell that exports it and a build in a shell that does not are using two different caches and possibly two different compilers.

Khora needs to know its home before it can do anything. If neither `KHORA_HOME` nor a home directory is set, it says so and stops rather than guessing.

### `KHORA_STD`

Where the standard library's sources are.

**Default:** unset, and the compiler looks beside its own executable and one directory up. A toolchain unpacked from a release archive, and a compiler built in its own source tree, are both found this way.

**Value:** a path to a directory. It is ignored if no directory is there, so a stale setting falls back to the search rather than failing.

Set it only for a layout the search does not cover — a standard library relocated away from the binary. It is not a way to substitute a modified `std`: the compiler is entitled to assume `std` matches the version it was built from.

### `KHORA_RT_LIB`

The runtime archive that generated executables are linked against.

**Default:** unset, and the same search as `KHORA_STD`: beside the executable, one directory up, then the compiler's own build tree.

**Value:** a path to the archive file itself, not the directory holding it. Ignored if no file is there.

The same caveat applies, and more sharply: the runtime archive and the compiler agree about object layout, so an archive from a different version produces a program that links and then misbehaves.

## Choosing what gets built

### `KHORA_PROFILE`

Which build profile to use.

**Default:** `debug`.

**Values:** `release` selects the optimized, reproducible profile. Anything else — including a typo — is `debug`. It is read in the middle of a build where there is nowhere to report an error, and a misspelling that silently optimized would be worse than one that silently did not.

```bash
KHORA_PROFILE=release khora test .
KHORA_PROFILE=release khora bench .
```

`khora build . --release` is the flag for the same thing and is what a build should use. The variable exists because `khora test` and `khora bench` have profiles too and a flag on every subcommand is three ways to say one thing.

### `KHORA_DEBUG`

Whether the build emits debug information, overriding what the profile would decide.

**Default:** unset — debug builds emit it, release builds do not.

**Values:** `1`, `on` or `true` to emit it; `0`, `off` or `false` to suppress it. Anything else is treated as unset.

```bash
KHORA_DEBUG=1 KHORA_PROFILE=release khora build .   # optimized, with line tables
KHORA_DEBUG=0 khora build .                         # unoptimized, without
```

It overrides in both directions on purpose: profiling wants an optimized build it can attribute to source lines. It is part of the build cache key, so switching it does not hand you the other build's artifact. [Debugging a program](/docs/reference/debugging/) has what the debug information is good for and what it is not.

### `KHORA_TARGET`

What to generate code for, instead of the machine you are on.

**Default:** unset — the host.

**Values:** one of `linux`, `macos`, `windows`, `wasm`, or a full target triple such as `aarch64-unknown-linux-gnu`. A triple selects its own family, so one setting moves both halves of a cross build: which platform's `std` files are read, and which target the code generator emits for. Any other value is rejected with a message naming what is accepted.

**What it costs, and it is the whole story here.** This does not produce a runnable program for another platform. There is no cross linking: the build stops at generating and verifying a module. What it is for is checking that a combination *compiles* — a `std` surface that only one platform selects, a calling convention only one platform uses — from whichever machine you have. Running the result still needs the real platform. [Supported targets](/docs/deployment/supported-targets/) is what is actually shipped.

### `KHORA_UNBOXED` — not a knob

It changes how values are laid out across the whole program and is not a supported setting. [Variables that are not knobs](#variables-that-are-not-knobs) says why it is reachable at all.

## Dependencies

### `KHORA_LOCKED`

Refuse to change the lockfile — resolve exactly what it pins, and fail rather than update it.

**Default:** unset, and a resolve may update the lockfile.

**Values:** any value, including the empty string. This is a present-or-absent switch; `KHORA_LOCKED=0` turns it **on**, because what is read is whether the name is set at all.

The `--locked` flag does the same thing for one command. Setting the variable is for CI, where every command in the job should behave that way and adding a flag to each is how one gets forgotten.

### `KHORA_RELEASE_REPO`

Where `khora toolchain install` looks for releases.

**Default:** `codyspate/khoralang`.

**Value:** a GitHub `owner/repository`.

For an organisation publishing its own toolchain builds. Checksums are verified against the release regardless of where it came from.

## The build cache

### `KHORA_CACHE_BUDGET`

How large the build cache may grow before the least recently used entries are evicted.

**Default:** 2 GiB (`2147483648`).

**Value:** a size in bytes, as a plain integer. A value that is not an integer is ignored and the default stands.

The default is chosen against the disk rather than the workload: small enough that a build loop left running cannot fill a modest disk, large enough to hold many builds of a large project. Raise it on a machine with room and several large projects; lower it on a small disk. A cache below the size of one build's output evicts on every build and costs you the cache entirely.

### `KHORA_CACHE_EXPLAIN`

Print the cache key and why a lookup missed.

**Default:** unset — a build says whether it reused an artifact, not why it did not.

**Values:** any value except `0` turns it on. `KHORA_CACHE_EXPLAIN=0` is off.

```bash
KHORA_CACHE_EXPLAIN=1 khora build .
```

Reach for it when a build rebuilds something you expected it to reuse. The interesting answer is that the key *moved*: a tree that has not changed should produce the key it produced last time, so a key that moved anyway means an input moved that nobody meant to move, and this names it.

## Watching the compiler work

### `KHORA_TIMINGS`

Print one line per compilation phase to standard error, and a total.

**Default:** unset.

**Values:** any value except `0` turns it on.

It measures phases of the compiler, not of your program. There is no sampling profiler for Khora code; [Profiling your program](/docs/performance/your-program/) is what that question has instead.

### `KHORA_EMIT_LLVM`

Write the generated LLVM IR beside the executable.

**Default:** unset.

**Values:** any value, including the empty string — present or absent is what is read.

The module is written as `.ll` *before* verification, so a module that fails to verify is still there to be read, which is exactly when it is wanted. A release build writes the optimized module as well, as `.opt.ll`, so the two can be diffed.

This is for reporting a compiler bug. The IR is not an interface and its shape changes freely.

## The runtime, inside a compiled program

These are read by the program Khora builds, not by `khora`. Setting them changes how *your* executable behaves.

### `KHORA_BACKTRACE`

Print a backtrace when a trap kills the program.

**Default:** unset — a trap prints the failure and the source location, and says to re-run with this set.

**Values:** any value. `RUST_BACKTRACE` is honoured identically, so a machine that already exports it for everything is not asked twice.

```bash
KHORA_BACKTRACE=1 ./build/myapp
```

Off by default because capturing a backtrace costs every well-behaved program a page of stack on the way out, and the first thing anybody does with a bug is run it again. The frames are symbolized from the debug information the executable carries, so a build made without it gives addresses rather than names — still worth printing, since an address and the binary can be symbolized later. [Traps](/docs/reference/traps/) is the list of what counts as one; a failed `assert` does not.

### `KHORA_FIBERS`

Which fiber backend the program runs on.

**Default:** unset — a fiber is an operating-system thread.

**Values:** `scheduler` selects the M:N scheduler, stackful coroutines on a pool of workers. Any other value, including `threads`, is the default.

```bash
KHORA_FIBERS=scheduler ./build/myapp
```

**This is a supported setting, and it is the one to think hardest about.** A program cannot tell which backend it is on — that is the design, and the operations behave the same — but the two are not equally exercised. Threads are the default because they are faster at the connection counts a service actually runs at, and because they are the better-travelled path. The scheduler's advantage is density: a suspended fiber costs roughly 4 KB against a thread's 33 KB, which matters when tens of thousands of fibers are waiting rather than working.

**What it costs.** The density figure is measured on Windows. On Linux, `vm.max_map_count` and guard pages splitting mappings mean it has not been reproduced, so on the platform most deployments use, the reason to switch is not established. The scheduler is also the less-exercised path and therefore the likelier home of the next runtime bug. It is a real choice with real evidence on one side of it, not a flag to set for luck. [Concurrency](/docs/reference/concurrency/) has what a fiber is either way.

It is also useful the other direction: running a suspected runtime bug under both backends separates a bug in one of them from a bug in what they share.

### `KHORA_BLOCKING_THREADS`

How many threads the pool that runs blocking work may hold.

**Default:** twice the number of cores, and never fewer than four.

**Value:** a positive integer. A value that is not an integer is ignored and the default stands.

Blocking work — anything that would otherwise stall a worker — goes to this pool. The bound exists so the pool does not become one thread per fiber. When a program wants more concurrent blocking work than the pool has room for, the extra waits rather than growing the pool, and the runtime counts how often that happened; a count that climbs with load is what says this is the number to raise. The default is a starting point, not a measurement.

### `KHORA_SCHEDULER_REPORT`

Print the scheduler's counters to standard error on an interval.

**Default:** unset.

**Value:** an interval in milliseconds, as a plain integer. A value that is not an integer is ignored, and the interval is at least 1 ms.

```bash
KHORA_FIBERS=scheduler KHORA_SCHEDULER_REPORT=500 ./build/myapp
```

This is how counters get out of a program that does not end: a server runs until it is killed, so there is no moment to print them at. The difference between two lines is the interesting part. It does nothing without `KHORA_FIBERS=scheduler` — there is no scheduler to report on — and each line is a write to standard error, so a short interval on a busy program is itself load.

## Variables that are not knobs

Two names reach a compiled program and are deliberately not documented above, because setting them is not supported:

- **`KHORA_UNBOXED`** turns off flat layout for small values, across the whole program. Whether a value is laid out inline or behind a header is a compiler decision; it is reachable this way because separating a miscompile from the change that exposed it needed a switch, not because anybody should set it.
- **`KHORA_NO_SIGNALS`** stops the signal watcher from installing. A program running with it set does not shut down gracefully on `SIGTERM` or `SIGINT`, and nothing reports that it did not.

Both are recorded as decisions still to be made rather than as settings. The rest of the `KHORA_*` names in the source are test and packaging scaffolding — they configure the repository's own soak tests and release builds, and do nothing in a shell.
