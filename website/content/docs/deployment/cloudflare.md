---
title: Cloudflare Workers
sidebar:
  order: 2
---

Khora does **not yet advertise Cloudflare Workers as a supported deployment target**.

**Not "experimental" either**, by the vocabulary on
[Supported targets](/docs/deployment/supported-targets/): experimental there
means important pieces work and production support is not promised yet, and
here none of the pieces exist. `std` has no Worker-shaped platform surface,
there is no no-fibers execution model to test, and no host-provided networking
or storage capability is modelled. LLVM can emit wasm — that is emission, and
the same page says the website must never describe emission-only support as
deployable platform support. This page is here to say what the target would
need, and to be the thing a reader finds instead of guessing.

Cloudflare Workers run WebAssembly inside a host-provided execution environment. A Khora Worker target therefore cannot reuse the native Linux runtime model unchanged: networking, request handling, persistence, clocks, and other platform facilities must be provided through Worker host capabilities rather than Unix sockets or a local filesystem.

## Planned target model

The intended target is `wasm32-unknown-unknown` with a Worker-specific standard-library/platform surface.

A Khora Worker application will need to:

- compile to WebAssembly using the supported Khora target;
- receive HTTP requests through Worker host bindings rather than `std::net` sockets;
- use host-provided storage and database capabilities where required;
- avoid native filesystem and process APIs that do not exist in the Worker environment;
- package the generated module and bindings in the form required by the supported deployment tooling.

## When this page becomes a deployment guide

This page will contain copy-pasteable build and deploy commands only after the
compiler, Worker bindings, runtime/platform split, packaging, and an end-to-end
deployed Khora example are working together. There is deliberately no
`wrangler.toml` sketch here: a configuration file for a module nothing can
produce is a thing somebody will copy and then spend an afternoon on.

Until then, do not choose Cloudflare Workers for a Khora application that needs a supported production deployment path. Use one of the targets listed as supported in [Supported targets](/docs/deployment/supported-targets/).
