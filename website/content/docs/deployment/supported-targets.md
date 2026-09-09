---
title: Supported targets
sidebar:
  order: 1
---

A platform is in one of three states here, and the words mean this:

- **supported** — the toolchain builds, links, tests, and releases artifacts for the target;
- **experimental** — important pieces work, but production support is not yet promised;
- **emission-only** — LLVM can emit the object/module format, but the runtime/linker/deployment path is incomplete.

The website must never describe emission-only support as deployable platform support.

## What is supported

Three triples, and they are exactly the ones the release workflow builds, packages, and then uses to compile a program before publishing:

| triple | state | |
| --- | --- | --- |
| `x86_64-unknown-linux-gnu` | supported | on glibc 2.39 or newer — [see below](#the-linux-build-needs-glibc-239) |
| `x86_64-pc-windows-msvc` | supported | |
| `aarch64-apple-darwin` | supported | |

A release is not published unless each of them has produced an artifact, unpacked it somewhere else, and built and run a program with it. That test is the reason this list is short: it is what has actually been done, not what is expected to work.

**Everything else is out of scope for this release**, which is a statement about what is promised rather than a prediction about what would happen if you tried:

- **Linux arm64** is not built or tested by CI, so it is not listed. The compiler may well produce a working binary there; nobody has checked, and an unchecked platform in a supported table is the claim this page exists to prevent.
- **Cross-compilation** — building a Linux artifact on a Mac, say — is not supported. The compiler can emit for another target, and `KHORA_TARGET` will verify its code generation, but the linker and sysroot story is unfinished, so a deployable artifact still means building on the platform it runs on.
- **Static and musl builds** are not produced or tested. The published Linux artifact is dynamically linked against the system C library, which is what [Containers](/docs/deployment/containers/) assumes.
- **WebAssembly, and Cloudflare Workers with it**, are not a target of this release. See below.

## The Linux build needs glibc 2.39

**`x86_64-unknown-linux-gnu` means a recent one.** The published Linux
toolchain is compiled on GitHub's `ubuntu-latest` runner, which is Ubuntu
24.04, and a glibc program carries the symbol versions of the machine that
built it. The published binary asks for `GLIBC_2.39` and will not start against
anything older — not with a degraded feature, but with
`libc.so.6: version 'GLIBC_2.39' not found` on every command.

| distribution | glibc | the published toolchain |
| --- | --- | --- |
| Ubuntu 24.04 LTS | 2.39 | runs |
| Debian 13 (trixie) | 2.41 | runs |
| Debian 12 (bookworm), current stable | 2.36 | does not run |
| Ubuntu 22.04 LTS | 2.35 | does not run |
| RHEL 9 and rebuilds | 2.34 | does not run |
| Alpine, and anything on musl | not glibc | does not run |

That table is the honest reading of the release rule at the bottom of this
page, applied to a detail the rule did not previously ask about: what a fresh
CI environment produced was tested on the environment that produced it. Debian
stable and a supported Ubuntu LTS are on the wrong side of the line, so the
requirement is stated here rather than discovered by somebody installing.

`install.sh` refuses before downloading on a system below the floor, and stops
with the error rather than reporting success if the unpacked binary cannot run
anyway. `scripts/check-install.sh` is what holds the release to that: it
installs from the published archive inside each of those images and checks that
the toolchain and the installer agree.

**Raising the floor is a release-workflow change, not a documentation one.**
Building the Linux artifact on an older base image would lower the number; until
that is done and checked, the number here is what the release actually is. What
this page will not do is list a target as supported and leave the constraint
that makes it unusable on half of today's Linux machines to be found at runtime.

Nothing here applies to the programs *you* compile. A Khora executable links
against the machine that built it, so its floor is that machine's glibc —
which is why [Containers](/docs/deployment/containers/) builds and runs in the
same base image.

## The artifacts, and checking one

A release publishes one archive per triple and a checksum beside it, named for
the tag:

```text
https://github.com/codyspate/khoralang/releases/download/v0.1.0/khora-0.1.0-x86_64-unknown-linux-gnu.tar.gz
https://github.com/codyspate/khoralang/releases/download/v0.1.0/khora-0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256
```

`install.sh` and `install.ps1` fetch exactly that pair and refuse the archive if
the digest does not match. To do it by hand — in a build image that should not
pipe a script into a shell, say:

```bash
version=0.1.0
triple=x86_64-unknown-linux-gnu
base=https://github.com/codyspate/khoralang/releases/download/v$version
curl -fsSLO "$base/khora-$version-$triple.tar.gz"
curl -fsSLO "$base/khora-$version-$triple.tar.gz.sha256"
sha256sum -c "khora-$version-$triple.tar.gz.sha256"
tar xzf "khora-$version-$triple.tar.gz" -C /opt
/opt/khora-$version-$triple/bin/khora --version
```

Windows ships the same layout as a `.zip` rather than a `.tar.gz`.
The archive unpacks to `khora-<version>-<triple>/` holding `bin/khora`, the
runtime archive beside it, `std/` as source, and the licences. Nothing needs
configuring after unpacking: the compiler finds `std/` and the runtime beside
its own binary, and `KHORA_STD` and `KHORA_RT_LIB` exist only as overrides for
an unusual layout.

A release also carries `khora-<version>.cdx.json`, a CycloneDX bill of
materials for the toolchain itself, with its own checksum.

## WebAssembly

WebAssembly is a distinct runtime environment, not Linux with a different object format. A wasm target must use a std/platform surface appropriate to its host and must not expose filesystem or socket APIs the host does not provide.

**No wasm target is advertised**, so none of that has been built. `std` has no Worker-shaped platform surface, there is no no-fibers execution model to test, and no host-provided networking or storage capabilities are modelled. LLVM can emit wasm — the compiler's own tests check that the runtime's symbols resolve there — and that is emission, not a deployment path.

Cloudflare Workers is the motivating first wasm deployment target, and its host-provided networking model and single-threaded isolate mean its runtime contract will differ intentionally from native server targets. [Cloudflare Workers](/docs/deployment/cloudflare/) says what would have to exist, and tells you not to choose it in the meantime.

## Release rule

A target enters the supported table only when a fresh CI environment can produce the release artifact and a deployment/conformance test executes it in the environment users are being told to target.
