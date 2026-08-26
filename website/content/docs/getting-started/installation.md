---
title: Installation
sidebar:
  order: 1
---

One command, then `khora` looks after itself. There is no separate version
manager to install first.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/codyspate/khoralang/main/install.sh | sh
```

On Windows, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/codyspate/khoralang/main/install.ps1 | iex
```

This downloads the build for your platform, checks it against the checksum
published beside it, and unpacks it into `~/.khora`. Nothing is compiled,
nothing needs administrator, and `rm -rf ~/.khora` undoes it.

### What you need on the machine

A C toolchain, and only for its **linker**. Khora compiles to a native object
file, and turning one into an executable needs the platform's own C runtime and
system libraries — the same requirement `rustc` has, for the same reason.

| | |
| --- | --- |
| macOS | `xcode-select --install` |
| Debian, Ubuntu | `apt install clang` — or `build-essential` |
| Fedora, RHEL | `dnf install clang` |
| Windows | Visual Studio Build Tools with "Desktop development with C++", or [LLVM](https://releases.llvm.org) |

The installer checks for one before downloading and tells you if it is missing.
You do not need LLVM: it is linked into `khora` itself, not called as a program.

## Keeping up to date

```bash
khora update                      # the newest release, and use it
khora toolchain install 0.2.0     # a particular one
khora toolchain default 0.1.0     # go back to one you already have
khora toolchain list              # what is on this machine
```

An update never removes the version it replaces, so going back is a command
rather than a reinstall.

### Release candidates

**Before there is a stable release, this is the install.** A candidate is
published as a GitHub pre-release, which `/releases/latest` excludes — so the
plain command above cannot reach one, and says so rather than failing oddly.

```sh
curl -fsSL https://raw.githubusercontent.com/codyspate/khoralang/main/install.sh | sh -s -- --pre
```

```powershell
irm https://raw.githubusercontent.com/codyspate/khoralang/main/installrc.ps1 | iex
```

Two different shapes because `iex` cannot pass an argument to what it is piped,
where `sh -s --` can. `installrc.ps1` is `install.ps1` with `-Pre`, and nothing
else — it forwards rather than copying, so there is one implementation of
installing and no second file to keep true about checksums and layout.

`--pre` and `-Pre` mean "candidates as well", not "candidates only". The day
after a stable release they install that stable release, which is the right
answer for somebody who ran this once and left it in a script.

A particular one, by name:

```sh
curl -fsSL .../install.sh | sh -s -- --version 0.1.0-rc.2
```

Once a candidate is installed, `khora` does the rest itself:

```bash
khora toolchain install --pre    # a newer candidate, alongside this one
khora update --pre               # and make it the default
```

Candidates are versions of their own: `0.2.0-rc.1`, then `-rc.2`, and finally
`0.2.0` built from the same commit as the last candidate. Nothing is promoted
in place, so a version number never changes meaning.

## Pinning a version for one project

```toml
# khora.toml
[toolchain]
version = "0.2.0"
```

`khora` hands the command over to that version before it parses any arguments,
so a project pinning a release with flags your build has never heard of still
works. A pin always wins over your default — and a pinned version you do not
have **stops** the command rather than falling back, because a build that
quietly used a different compiler is worse than one that refused.

```bash
khora toolchain which     # which version this directory gets, and why
```

## Building from source

For working on Khora itself, or on a platform with no published build.

**Requirements:** Git, a current Rust toolchain, and LLVM 22.1.8 for native code
generation. The parser, type checker, formatter and most compiler tests do not
need LLVM; compiling Khora programs to executables does.

```bash
git clone https://github.com/codyspate/khoralang.git
cd khoralang
sh scripts/setup-llvm.sh
cargo build -p khora-cli --features llvm
cargo build -p khora-rt
```

`scripts/setup-llvm.sh` installs or locates the pinned LLVM and writes the
machine-specific Cargo configuration the backend needs.

```bash
cargo run -p khora-cli --features llvm -- check std examples
```

To use a compiler you built as a pinned version:

```bash
khora toolchain link 0.2.0 target/debug/khora
```

It is copied rather than pointed at, so a later `cargo clean` cannot leave the
registration dangling.

### Running the tests

```bash
cargo nextest run --workspace --features llvm
cargo test --workspace --doc
```

`cargo nextest` is optional and roughly halves the wait; a plain
`cargo test --workspace --features llvm` does the same work, doctests included.
`sh scripts/baseline.sh` is the full gate, and prefers nextest when it is
installed.
