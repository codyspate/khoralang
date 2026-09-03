---
title: Supported targets
sidebar:
  order: 1
---

Khora's production documentation distinguishes three states clearly:

- **supported** — the toolchain builds, links, tests, and releases artifacts for the target;
- **experimental** — important pieces work, but production support is not yet promised;
- **emission-only** — LLVM can emit the object/module format, but the runtime/linker/deployment path is incomplete.

The website must never describe emission-only support as deployable platform support.

## What 0.1.0 supports

Three triples, and they are exactly the ones the release workflow builds, packages, and then uses to compile a program before publishing:

| triple | state |
| --- | --- |
| `x86_64-unknown-linux-gnu` | supported |
| `x86_64-pc-windows-msvc` | supported |
| `aarch64-apple-darwin` | supported |

A release is not published unless each of them has produced an artifact, unpacked it somewhere else, and built and run a program with it. That test is the reason this list is short: it is what has actually been done, not what is expected to work.

**Everything else is out of scope for this release**, which is a statement about what is promised rather than a prediction about what would happen if you tried:

- **Linux arm64** is not built or tested by CI, so it is not listed. The compiler may well produce a working binary there; nobody has checked, and an unchecked platform in a supported table is the claim this page exists to prevent.
- **Cross-compilation** — building a Linux artifact on a Mac, say — is not supported. The compiler can emit for another target, and `KHORA_TARGET` will verify its code generation, but the linker and sysroot story is unfinished, so a deployable artifact still means building on the platform it runs on.
- **Static and musl builds** are not produced or tested. The published Linux artifact is dynamically linked against the system C library, which is what [Containers](/docs/deployment/containers/) assumes.
- **WebAssembly, and Cloudflare Workers with it**, are not a target of this release. See below.

## WebAssembly

WebAssembly is a distinct runtime environment, not Linux with a different object format. A wasm target must use a std/platform surface appropriate to its host and must not expose filesystem or socket APIs the host does not provide.

**No wasm target is advertised in 0.1.0**, so none of that has been built. `std` has no Worker-shaped platform surface, there is no no-fibers execution model to test, and no host-provided networking or storage capabilities are modelled. LLVM can emit wasm — the compiler's own tests check that the runtime's symbols resolve there — and that is emission, not a deployment path.

Cloudflare Workers is the motivating first wasm deployment target, and its host-provided networking model and single-threaded isolate mean its runtime contract will differ intentionally from native server targets. [Cloudflare Workers](/docs/deployment/cloudflare/) says what would have to exist, and tells you not to choose it in the meantime.

## Release rule

A target enters the supported table only when a fresh CI environment can produce the release artifact and a deployment/conformance test executes it in the environment users are being told to target.
