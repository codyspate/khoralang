---
title: Installation
sidebar:
  order: 1
---

Install the Khora toolchain once, then use `khora` to manage compiler versions for all of your projects. There is no separate version manager to install.

## Install Khora

On macOS or Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/codyspate/khoralang/main/install.sh | sh
```

On Windows, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/codyspate/khoralang/main/install.ps1 | iex
```

The installer downloads the build for your platform, verifies it against the published checksum, and installs it under `~/.khora`. It does not compile Khora from source and does not require administrator access.

Verify the installation:

```bash
khora --version
```

Then continue with [Getting Started](/docs/getting-started/) or [Your first Khora project](/docs/getting-started/first-project/).

## Verify what you downloaded

Every archive is published with a `.sha256` beside it, and the installer checks
it. If you fetched an archive by hand, check it the same way:

```bash
sha256sum -c khora-0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256
```

A checksum says the bytes are the ones that were published. It does not say who
published them, and anybody who can replace the archive can replace the
checksum next to it. For that, every archive also carries **build provenance**:
a signed statement, made by GitHub during the release run, of which workflow in
which repository at which commit produced that exact file.

```bash
gh attestation verify khora-0.1.0-x86_64-unknown-linux-gnu.tar.gz   --repo codyspate/khoralang
```

There is no maintainer key to trust and none to leak. The signing identity is
the release workflow's own, which is also why nothing is published from
anybody's workstation.

## What is in the toolchain

Each release includes a **bill of materials** as `khora-<version>.cdx.json`, in
CycloneDX 1.5, listing every Rust crate compiled into the toolchain with its
version and licence, the pinned LLVM, and the Rust toolchain that built it.
Dependency scanners read it directly.

That document is about the *compiler*. For the other question — what does the
program I am building pull in — `khora sbom` renders the same format from your
package's own resolution:

```bash
khora sbom --out my-app.cdx.json
```

It is rendered from the resolution rather than from a lockfile read off disk,
so it describes what a build here would use. Pass `--locked` to refuse a stale
lockfile instead of absorbing the difference.

## System linker requirement

Khora compiles to native object code, so producing an executable requires the platform's linker, C runtime, and system libraries. You do **not** need to install LLVM separately; LLVM is linked into the Khora compiler rather than invoked as an external program.

| Platform | Linker/toolchain |
| --- | --- |
| macOS | `xcode-select --install` |
| Debian / Ubuntu | `apt install clang` or `apt install build-essential` |
| Fedora / RHEL | `dnf install clang` |
| Windows | Visual Studio Build Tools with **Desktop development with C++**, or LLVM |

The installer checks for a usable linker and tells you if one is missing.

## Update and manage toolchains

Use `khora` itself to install and switch compiler versions:

```bash
khora update                      # install the newest release and use it
khora toolchain install 0.2.0     # install a particular release
khora toolchain default 0.1.0     # choose the default release
khora toolchain list              # list installed toolchains
khora toolchain which             # show the version selected here and why
```

Updating does not remove the version it replaces, so rolling back is a toolchain selection rather than a reinstall.

## Pin a project to a compiler version

A project can select its compiler in `khora.toml`:

```toml
[toolchain]
version = "0.2.0"
```

A project pin takes precedence over your machine default. If the pinned version is not installed, Khora stops rather than silently building the project with a different compiler.

That makes the compiler version part of the reproducible project configuration:

```bash
khora toolchain which
```

shows which version the current directory selects.

## Uninstall

The default installation lives under `~/.khora`. If you no longer want Khora installed, remove that directory and remove any Khora path entry you added manually.

## Build the compiler from source

Building from source is for contributing to Khora itself, testing compiler changes, or working on a platform without a published toolchain artifact. Application developers normally use the installer above.

Requirements are Git, a current Rust toolchain, and LLVM 22.1.8 for native code generation.

```bash
git clone https://github.com/codyspate/khoralang.git
cd khoralang
sh scripts/setup-llvm.sh
cargo build -p khora-cli --features llvm
cargo build -p khora-rt
```

`scripts/setup-llvm.sh` installs or locates the pinned LLVM version and writes the machine-specific Cargo configuration needed by the backend.

You can register a compiler you built locally as a Khora toolchain:

```bash
khora toolchain link 0.2.0 target/debug/khora
```

## Release candidates

If you intentionally want to test a release candidate, opt into prerelease versions explicitly.

On macOS or Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/codyspate/khoralang/main/install.sh | sh -s -- --pre
```

On Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/codyspate/khoralang/main/installrc.ps1 | iex
```

After a Khora toolchain is installed, the normal toolchain commands can also include prereleases:

```bash
khora toolchain install --pre
khora update --pre
```

`--pre` means prereleases are eligible; it does not prevent the tool from selecting a newer stable release. Release candidates have their own immutable version numbers such as `0.2.0-rc.1` and are not promoted in place.

When you are ready to write code, continue with [Your first Khora project](/docs/getting-started/first-project/).
