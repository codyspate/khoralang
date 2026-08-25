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

## Native targets

Windows, Linux, and macOS are exercised by the compiler/runtime test matrix. Cross-target support is being expanded so release builds can produce deployable artifacts without requiring the destination machine to compile them locally.

Exact supported triples will be listed here for each public release once the runtime, linker/sysroot, packaging, and CI story is complete for those triples.

## WebAssembly

WebAssembly is a distinct runtime environment, not Linux with a different object format. A wasm target must use a std/platform surface appropriate to its host and must not expose filesystem or socket APIs the host does not provide.

Cloudflare Workers is the motivating first wasm deployment target. Its host-provided networking model and single-threaded isolate mean its runtime contract differs intentionally from native server targets.

## Release rule

A target enters the supported table only when a fresh CI environment can produce the release artifact and a deployment/conformance test executes it in the environment users are being told to target.
