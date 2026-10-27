---
title: The build
sidebar:
  order: 4
---

`khora build` takes a package from source to a native executable in one
process: parsing, name resolution, type inference, exhaustiveness checking,
whole-program monomorphization, reference-count planning, LLVM, and a link.

## Whole-program, always

Khora monomorphizes the entire program. A generic function is compiled once per
concrete set of type arguments, and so is a row variable — `'ef` and `'er`
specialize at every call site reachable from `main`, for the same reason a type
variable does.

This is why performing an effect costs a function call rather than a lookup,
and why an error's tag can be a program-wide id assigned once. It is also the
trade: compile time grows with the program, and there is no separate
compilation of a generic across package boundaries.

### Only what is reached is compiled

The walk starts from the build's entry points and compiles what they reach,
generic or not. The entry points are:

- `main` for `khora build` and `khora run`, the `test` blocks for `khora test`,
  and the `bench` blocks for `khora bench`;
- every `pub extern fn`, in every build, because C can call one by name with
  no Khora call leading to it.

So a program pays to compile the parts of `std` it uses, not all of it: a
hello world compiles two functions. A function nothing reaches is still
type-checked, and `khora check` refuses the same programs whether or not
anything calls the function at fault, but no machine code is generated for it.

## LLVM is inside the compiler; the linker is not

The `khora` binary carries LLVM within it, and finds `std/` and the runtime
archive beside itself. Nothing needs configuring to compile.

Linking is different. Turning an object file into an executable needs the
platform's C runtime and system libraries, and the driver that knows where
those live belongs to the platform — so Khora calls `clang` or `gcc`. That is
the one thing the toolchain cannot bring with it, and `rustc` has the same
requirement for the same reason.

## Two profiles

| | `debug` (default) | `release` |
| --- | --- | --- |
| optimization | none | LLVM's `default<O2>` |
| debug information | yes | no |
| reproducible | no | **bit for bit** |

`release` drops debug information, which is one of the things that makes it
reproducible: a debug build embeds each source file's absolute path, so two
checkouts of identical content produce different bytes.

### What "reproducible" means here

Two `--release` builds of the same source, with the same toolchain, produce
byte-identical object files *and* byte-identical executables — on the same
machine or on two different ones.

Getting there means removing everything that varies with something other than
the input:

- **Windows** — a PE header carries a timestamp, so the linker is told to write
  a hash of the content instead.
- **macOS** — a Mach-O image carries a UUID, and an `arm64` image is ad-hoc
  code signed; both are derived partly from the *output file's name*. The link
  writes to a fixed name and the result is moved into place, so neither depends
  on what the caller chose to call it.
- **Everywhere** — no absolute paths, which is what the debug-information trade
  buys.

## The build cache

`khora build`, `khora run`, `khora test` and `khora bench` share one cache. A
build is cached under a key covering everything that can change the output:

| in the key | why |
| --- | --- |
| every source file's contents | the obvious half |
| the compiler binary, hashed | not its version string — a version is constant across every development build |
| the linker binary | Khora emits an object and a C driver links it, so the driver's bytes are in the output's |
| the runtime archive | every executable links it statically |
| the target triple | a different target produces different code |
| the profile, and whether debug information is on | the environment can override the profile in both directions |
| executable, library, test runner or bench runner | one package's sources build all four, and each is a different artifact |
| the `[build]` table's `link` names and `link-search` directories, and each archive found there by content | the linker is told them; a library found in a system directory is known only by its name |
| the source *paths*, when debug information is on | because a debug build embeds them |

Two things that change how a test run behaves are deliberately *not* in it.
`--filter` is passed to the test runner when it starts, so one cached runner
serves every filter. `KHORA_FIBERS` is read by the runtime when the program
starts, so one artifact runs on either fiber backend; which one it gets by
default is decided by the runtime archive, which is in the key.

`--no-cache` on `khora build`, `khora run`, `khora test` and `khora bench`
builds without asking the cache, and files the result. It is how to see the
IR of a cached program, because `KHORA_EMIT_LLVM` is not in the key and a
build served from the cache writes no `.ll`.

Fields are length-prefixed before hashing, so two adjacent ones cannot be run
together into a different key with the same bytes. Sources are sorted by
content rather than by path, so the key does not depend on where the checkout
lives.

**Hashing the compiler and the linker is the unusual part**, and it is what
makes a hit provable rather than probable. A cache keyed only on sources is a
bet that nothing else changed; the way that bet is normally lost is a toolchain
difference nobody hashed, and the symptom is an artifact that is subtly wrong
with no failure anywhere.

Because `release` is reproducible, a cache hit is not merely "an artifact built
from the same inputs" — it is the same bytes a fresh build would have produced,
and the project's own gate checks that by building both ways and comparing.

A cache miss is never a build failure: a corrupt or unreadable entry is treated
as absent and the build proceeds.
