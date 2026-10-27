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

Four toolchains, and they are exactly the ones the release workflow builds, packages, and then uses to compile a program before publishing:

| triple | state | |
| --- | --- | --- |
| `x86_64-unknown-linux-gnu` | supported | on glibc 2.39 or newer — [see below](#the-linux-build-needs-glibc-239) |
| `aarch64-unknown-linux-gnu` | supported | on glibc 2.39 or newer — Graviton, Ampere, arm64 containers |
| `x86_64-pc-windows-msvc` | supported | |
| `aarch64-apple-darwin` | supported | |

A release is not published unless each of them has produced an artifact, unpacked it somewhere else, and built and run a program with it. That test is the reason this list is short: it is what has actually been done, not what is expected to work.

Both Linux toolchains also build **static executables** for two more targets, which are targets for your programs rather than places the toolchain itself runs:

| triple | state | |
| --- | --- | --- |
| `x86_64-unknown-linux-musl` | supported | `khora build --target x86_64-unknown-linux-musl` |
| `aarch64-unknown-linux-musl` | supported | `khora build --target aarch64-unknown-linux-musl` |

[Static executables](#static-executables-musl) below says what one is and what it costs.

**Everything else is out of scope for this release**, which is a statement about what is promised rather than a prediction about what would happen if you tried:

- **The toolchain itself on Alpine or another musl system.** Programs built for musl run there; the compiler does not, because it carries LLVM linked against glibc. Build on a glibc machine or image and copy the executable.
- **Windows on arm64**, and **Linux on arm64 from a Windows or macOS toolchain**, are not built or tested.
- **Cross-compiling a dynamically linked program** — `aarch64-unknown-linux-gnu` from an x86-64 machine, or anything for Linux from a Mac — is not supported. The compiler can emit for another target, and `KHORA_TARGET` will verify its code generation, but the link needs that target's glibc and nothing supplies it. Static musl executables are the exception: either Linux toolchain builds them for either architecture.
- **WebAssembly, and Cloudflare Workers with it**, are not a target of this release. See below.

## Static executables (musl)

`khora build --target <arch>-unknown-linux-musl` links the program, the Khora
runtime and the musl C library into one statically linked executable. It has
no dynamic loader and needs no C library on the machine that runs it, so it
runs on any Linux kernel of that architecture: Alpine, a distroless `static`
image, or an empty `scratch` container — [Containers](/docs/deployment/containers/#a-static-binary-in-a-scratch-image)
has a Dockerfile.

What a build needs:

- **A Linux toolchain.** It carries the musl runtime and C library for both
  architectures in its `targets/` directory, so nothing is downloaded.
- **A C compiler driver**, as for any build. For the machine's own
  architecture `clang` or `gcc` will do.
- **For the other architecture, `clang` and `ld.lld`** (`apt install clang
  lld`): gcc cannot be told another target, and the system linker reads only
  its own machine's objects. Without them the build stops and says which is
  missing.

What it costs and what it excludes:

- **Size.** Everything the program uses is inside it. A release hello world
  is about 9 MB static against about 10 MB dynamic; the runtime both carry is
  most of either.
- **No `--lib`.** A shared library is loaded into a process that already has
  a C library, and a musl one inside a glibc process is two C libraries with
  different ideas of the same state. The build refuses and says so.
- **Name resolution** is musl's: `/etc/hosts`, then the servers in
  `/etc/resolv.conf`. There is no NSS, so an `nsswitch.conf` module (LDAP,
  mDNS, `systemd-resolved` over D-Bus) is not consulted. Docker writes both
  files into every container.
- **Outbound TLS** verifies against the trust store at the usual paths, as
  the dynamic build does. In an image with none — `scratch` — `client()`
  raises `TlsError::BadCertificate` before it connects; copy in a CA bundle
  (`/etc/ssl/certs/ca-certificates.crt`) or name one with `SSL_CERT_FILE`.
- **Other programs.** `std::process` runs programs that are in the image. In
  `scratch` there are none and no `/bin/sh`, so `run`, `output` and
  `checked_output` raise `ProcessError` for anything not copied in, and
  `shell` always does. A program that shells out needs a base that has the
  programs: `gcr.io/distroless/static` plus them, or `busybox`.
- **Native libraries** in `[build] link` come only from `[build]
  link-search`, as `lib<name>.a` built for musl. The machine's own library
  directories are not searched — their archives are built against its glibc
  — and a name found nowhere stops the build saying so. `m`, `pthread`, `dl`,
  `rt` and `util` are part of musl's C library and need no file.

Fibers on both backends, sockets, TLS client and server, and the standard
library's own test suite run in a static build, and the release workflow
builds and runs one with each packaged Linux toolchain before publishing.

## The Linux build needs glibc 2.39

**Both Linux triples mean a recent one.** The published Linux
toolchains are compiled on GitHub's Ubuntu 24.04 runners, x86-64 and arm64,
and a glibc program carries the symbol versions of the machine that
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
same base image. A [static executable](#static-executables-musl) has no floor.

## The artifacts, and checking one

A published stable release provides one archive per triple and a checksum
beside it, named for the tag. These 0.4.0 URLs work only after the stable
release is published; for a published release candidate, replace both `0.4.0`
strings with its full version, such as `0.4.0-rc.2`:

```text
https://github.com/codyspate/khoralang/releases/download/v0.4.0/khora-0.4.0-x86_64-unknown-linux-gnu.tar.gz
https://github.com/codyspate/khoralang/releases/download/v0.4.0/khora-0.4.0-x86_64-unknown-linux-gnu.tar.gz.sha256
```

`install.sh` and `install.ps1` fetch exactly that pair and refuse the archive if
the digest does not match. To do it by hand — in a build image that should not
pipe a script into a shell, say:

```bash
version=0.4.0
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
runtime archive beside it, `std/` as source, and the licenses. A Linux
archive also holds `targets/`, with the runtime and musl C library for each
[static target](#static-executables-musl). Nothing needs
configuring after unpacking: the compiler finds `std/` and the runtime beside
its own binary, and `KHORA_STD` and `KHORA_RT_LIB` exist only as overrides for
an unusual layout.

A release also carries `khora-<version>.cdx.json`, a CycloneDX bill of
materials for the toolchain itself, with its own checksum.

## WebAssembly

WebAssembly is a distinct runtime environment, not Linux with a different object format. A wasm target must use a std/platform surface appropriate to its host and must not expose filesystem or socket APIs the host does not provide.

**No wasm target is advertised.** What exists is a WebAssembly *library*: `khora build --lib` with `KHORA_TARGET=wasm32-unknown-unknown` produces a module that exports your `pub extern fn`s and imports nothing. **It needs a runtime archive that release archives do not include**: build one from a source checkout of the same version with `cargo build -p khora-rt --target wasm32-unknown-unknown` and point [`KHORA_RT_LIB`](/docs/reference/environment/#khora_rt_lib) at the `libkhora_rt.a` it writes under `wasm32-unknown-unknown/debug/` in the cargo target directory. Without it the build fails at the link. Inside the module, arrays, strings, loops, `!`, and regions with their finalizers work as they do natively, and the compiler's own tests run such modules and check what they return. It has one thread of control: nothing can be canceled from inside the module, a long loop runs until the host stops it, and `Fiber::spawn` is not available. **`print` and the text of a trap go nowhere** — the module has no standard output or error, and imports nothing that could carry them — so return what the host needs to see. **After a trap, discard the instance** and make a new one rather than calling it again: the trapped call's objects are never freed and its finalizers never run, so the instance that survives it is not in a state anything promises.

What is missing is everything a deployment needs. `std` has no Worker-shaped platform surface, and no host-provided networking or storage capabilities are modeled. That is the difference between a module that runs and a deployment path, and only the first exists.

Cloudflare Workers is the motivating first wasm deployment target, and its host-provided networking model and single-threaded isolate mean its runtime contract will differ intentionally from native server targets. [Cloudflare Workers](/docs/deployment/cloudflare/) says what would have to exist, and tells you not to choose it in the meantime.

## Release rule

A target enters the supported table only when a fresh CI environment can produce the release artifact and a deployment/conformance test executes it in the environment users are being told to target.
