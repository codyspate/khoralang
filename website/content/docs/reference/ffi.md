---
title: Foreign function interface
sidebar:
  order: 23
---

Khora has no stable native Khora ABI. Whole-program monomorphization and compiler/runtime evolution make a language-level binary ABI the wrong compatibility boundary.

C is the stable interoperability boundary. Khora can import foreign functions through `extern`, subject to package permissions and the runtime rules for ownership, blocking, callbacks, and thread affinity.

## Suspension and thread affinity

A Khora fiber may resume on a different OS thread after a suspension. Native code must not keep a thread-local address, errno-like borrowed state, native-thread identity, or a thread-affine handle across a suspension unless the API contract makes that safe.

An `extern` call cannot silently suspend while foreign stack/thread-affine state remains live. Potentially blocking native work belongs behind the runtime's blocking boundary rather than occupying a scheduler worker indefinitely.

## Exporting Khora

The production toolchain includes a C export surface so Khora libraries can be called from Python extensions, Node addons, C/C++ programs, and other systems able to consume a C ABI.

Exported APIs need explicit ownership/lifetime rules for strings, buffers, callbacks, and handles. The generated C header is part of that contract.

## Permissions

Compile-time permission to declare foreign functions is an authority boundary, not a runtime sandbox. Documentation must not describe it as process confinement.
