# Khora Documentation

Khora is a statically typed, native-compiled language for reliable concurrent applications. It combines direct-style effects, typed failures, capabilities and structured concurrency with native code generation and memory management that does not require programmers to write ownership proofs.

This documentation is for people using Khora. Design records for the compiler and runtime live separately under the repository's `docs/` tree.

## Learn Khora

- **Getting Started** — install Khora, create a project, build it, run it and test it.
- **Language Guide** — learn the language by writing programs: values, functions, ADTs, pattern matching, pipelines, generics, traits, effects, capabilities, resources and fibers.
- **Language Reference** — precise syntax and semantics for the language.
- **Standard Library** — API documentation for `std`.
- **Cookbook** — production patterns for HTTP services, database access, tracing, cancellation, bounded concurrency and testing.
- **Deployment** — supported targets, cross-compilation and deployment workflows.
- **Migration Guides** — mental-model bridges for developers coming from TypeScript/Effect, Go and Rust.

## Project status

Khora is not public-release ready until every release gate in [Production Release Readiness](project/release-readiness.md) is satisfied. The checklist intentionally includes language/runtime correctness, tooling, documentation, deployment, ecosystem, security and operating concerns. A fast compiler and a successful demo application are not enough by themselves.

Until the first public release, this documentation describes the intended production surface and may contain sections that are still being implemented. Any such section must be marked clearly rather than presented as available functionality.

## Documentation rule

Public documentation answers **how to use Khora** first. Internal documents answer **why Khora is implemented that way**.

A user should not need to understand LLVM, Perceus, coroutine frames, the scheduler's ownership audit, or compiler lowering in order to learn ordinary Khora. When implementation rationale is useful, the public page may link to the relevant internal design document as optional deeper reading.
