---
title: Cloudflare Workers
sidebar:
  order: 4
---

Khora does **not yet advertise Cloudflare Workers as a supported deployment target**.

**Not "experimental" either**, by the vocabulary on
[Supported targets](/docs/deployment/supported-targets/): experimental there
means important pieces work and production support is not promised yet, and
here only the bottom piece works. Khora can build a WebAssembly library that
runs its own code — arrays, strings, loops, error handling and regions, on one
thread of control with nothing to cancel it — and imports nothing from its host.
`std` has no Worker-shaped platform surface above that, and no host-provided
networking or storage capability is modeled, so there is no request to handle
and nothing to store into. `print` inside the module writes nowhere: it has no
standard output, and no import that could carry the text to the Worker's log.
This page is here to say what the target would need, and to be the thing a
reader finds instead of guessing.

Cloudflare Workers run WebAssembly inside a host-provided execution environment. A Khora Worker target therefore cannot reuse the native Linux runtime model unchanged: networking, request handling, persistence, clocks, and other platform facilities must be provided through Worker host capabilities rather than Unix sockets or a local filesystem.

## When this page becomes a deployment guide

This page will contain copy-pasteable build and deploy commands only after the
compiler, Worker bindings, runtime/platform split, packaging, and an end-to-end
deployed Khora example are working together. There is deliberately no
`wrangler.toml` sketch here: a configuration file for a module nothing can
produce is a thing somebody will copy and then spend an afternoon on.

Until then, do not choose Cloudflare Workers for a Khora application that needs a supported production deployment path. Use one of the targets listed as supported in [Supported targets](/docs/deployment/supported-targets/).
