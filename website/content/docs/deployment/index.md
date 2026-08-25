---
title: Deployment
sidebar:
  order: 0
---

Khora's deployment documentation distinguishes compiler output from actually supported runtime targets.

Use this section for supported target triples, Linux/container guidance, and Cloudflare deployment. A platform is only labeled supported when the compiler, runtime, linker/sysroot, packaging, and deployment/conformance path work end to end for a release.

The documentation website itself is deployed to Cloudflare Workers at `khoralang.com`, independently of the language's future WebAssembly/Workers application target.
