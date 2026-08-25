---
title: Containers
sidebar:
  order: 3
---

Khora's native deployment goal is a small, self-contained executable that does not require a language VM or tracing garbage collector in the container image.

A production container should build the application with a release compiler/toolchain, copy only the resulting executable and any explicitly required runtime data into the final image, and run as a non-root user where the environment permits it.

## Static and dynamic dependencies

Do not assume every target is fully static merely because Khora itself has no VM. TLS trust stores, C libraries, system integrations, and foreign libraries can still affect the final artifact. The supported-target documentation for a release must state what the produced binary expects from its host.

## Cross-building

The intended toolchain supports producing Linux artifacts from CI without requiring the final container to include the compiler. Cross-compilation counts as supported only once the runtime, linker/sysroot, and release tooling are part of that workflow.

## Health and shutdown

Services should expose ordinary readiness/health behavior and use structured cancellation during shutdown so nurseries stop accepting new work, in-flight work receives its shutdown policy, and resource finalizers run before process exit.
