---
title: Linux
sidebar:
  order: 4
---

Linux is a primary native deployment target for Khora services.

A release-supported Linux target must provide the compiler/runtime artifacts, linker/sysroot story, and CI coverage necessary to produce a runnable executable from a clean build environment.

## Service deployment

Run the application under the process supervisor used by your platform—container runtime, systemd, Kubernetes, or equivalent. Khora does not require a language VM process beside the executable.

Provide configuration and secrets at the application boundary, configure TLS trust roots where outbound TLS requires the host store, and expose health/readiness behavior appropriate to the service.

## Shutdown

Translate the platform's termination signal into structured application cancellation. Stop accepting new work, allow or cancel in-flight work according to the service policy, and let region/nursery cleanup finish before process exit.

## Static binaries

Whether a particular released target is fully static is an artifact property, not a blanket language claim. The release's supported-target table must state libc/system dependencies for each Linux triple.
