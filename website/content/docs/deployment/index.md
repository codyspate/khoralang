---
title: Deployment
sidebar:
  order: 0
---

Start with [Supported targets](/docs/deployment/supported-targets/) to see which platforms are
actually supported by the current toolchain. The platform pages then cover how
to build, package, and run Khora programs on those targets.

A target is only described as supported when the compiler, runtime, linker/sysroot,
packaging, and deployment path work end to end. Compiler code generation by itself
does not make a platform deployable.

## What you deploy

One file. `khora build` links a native executable that carries the Khora
runtime inside it — no VM, no tracing garbage collector, no `std/` beside it,
and no toolchain on the deployment host. The release workflow proves this on
every release: it unpacks the packaged toolchain somewhere else, builds a
program with it, and then runs the produced binary directly rather than through
`khora run`, because what has to hold is that the *binary* stands on its own
(`.github/workflows/release.yml`).

What it still expects from the host is the platform C library it was linked
against, and the machine's certificate trust store if it opens a TLS client
connection ([`std::net::tls`](/docs/stdlib/api/net/tls/) verifies against it).
[Supported targets](/docs/deployment/supported-targets/) says which triples are
built and what each expects.

## The build, end to end

```bash
khora fmt --check .     # formatting, as a gate rather than a rewrite
khora check .           # types, exhaustiveness, capabilities — no code generated
khora test .            # the package's tests, one fiber each
khora build . --release # optimized, no debug information, reproducible
```

`khora build` writes `build/<package>` — `build/<package>.exe` on Windows —
next to your `khora.toml` rather than among your sources. `--out` puts it
somewhere else under a name you choose:

```bash
khora build . --release --out dist/myservice
```

**Release is not the default profile.** Plain `khora build` is unoptimized and
carries debug information, which is what a crash you are about to read wants;
`--release` runs LLVM's `default<O2>`, drops debug information, and is
bit-for-bit reproducible. `KHORA_PROFILE=release` says the same thing to the
commands that have no flag of their own, `khora test` and `khora bench`.

If the project has dependencies, fetch and lock them first — this is the
command to run after cloning, and it needs `git` on the build machine, because
a `git` dependency is fetched by shelling out to it:

```bash
khora install
```

A bill of materials for what that resolved, as CycloneDX 1.5 JSON, and a pure
function of `khora.toml` and `khora.lock` so two runs over unchanged input
produce identical bytes:

```bash
khora sbom . --out myservice.cdx.json
```

## Where to go next

For native services, see [Linux](/docs/deployment/linux/) and
[Containers](/docs/deployment/containers/). For WebAssembly hosts such as
Cloudflare Workers, read [Cloudflare Workers](/docs/deployment/cloudflare/)
before designing around host-specific APIs: it is not a target of this release.
