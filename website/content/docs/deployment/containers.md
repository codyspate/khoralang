---
title: Containers
sidebar:
  order: 2
---

Khora's native deployment goal is a small, self-contained executable that does
not require a language VM or tracing garbage collector in the container image.
The compiler is large — it carries LLVM inside itself — and none of it belongs
in the image you ship, so the build is two stages and the second one copies a
single file.

## A Dockerfile

This example pins the stable 0.4.0 release and builds after it is published.
For a published release candidate, set `KHORA_VERSION=0.4.0-rc.2` and pin
the application and its Khora-repository dependencies to the same candidate.

```dockerfile
# --- build ------------------------------------------------------------------
FROM debian:trixie-slim AS build

# clang links the executable; the toolchain download cannot bring a linker with
# it. git is only needed if khora.toml has a git dependency.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates clang curl git tar \
    && rm -rf /var/lib/apt/lists/*

# Pinned. A plain `curl | sh` takes the newest stable release, which makes the
# image depend on the day it was built.
ARG KHORA_VERSION=0.4.0
RUN curl -fsSL https://raw.githubusercontent.com/codyspate/khoralang/main/install.sh \
      | sh -s -- --version "${KHORA_VERSION}" --to /opt/khora --no-modify-path
ENV PATH="/opt/khora/bin:${PATH}"

WORKDIR /src
COPY . .
RUN khora install \
    && khora build . --release --out /out/myservice

# --- run --------------------------------------------------------------------
FROM debian:trixie-slim

# ca-certificates only if the service opens outbound TLS: a TlsClient verifies
# against the machine's own trust store, and a slim image has none.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /var/lib/myservice --create-home myservice

COPY --from=build /out/myservice /usr/local/bin/myservice

USER 10001
WORKDIR /var/lib/myservice
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/myservice"]
```

```bash
docker build --build-arg KHORA_VERSION=0.4.0 -t myservice:1 .
docker run --rm -p 8080:8080 myservice:1
```

## Why both stages are the same base image

A Khora executable built for the default target is dynamically linked against
the system C library. So the runtime image
has to supply a glibc at least as new as the one the binary was linked against.
Same distribution and same release in both stages is the way to stop thinking
about it, and **which** release is not free to choose: the published toolchain
needs glibc 2.39 or newer, so `bookworm` cannot run it and `trixie` can. That
is why both stages above are trixie and not the more familiar bookworm; the
table on [Supported targets](/docs/deployment/supported-targets/) is the list.
A distroless runtime image works for the same reason and is smaller, as long as
it is the Debian 13 variant. For Alpine, or for an image with no C library at
all, build a static executable instead — the next section.

Nothing else from the build stage is copied. There is no `std/`, no compiler,
and no toolchain in the final image: the executable carries the Khora runtime
inside it, which the release workflow checks on every release by running a
packaged toolchain's output with nothing else on the path.

## A static binary in a `scratch` image

`khora build --target x86_64-unknown-linux-musl` (or
`aarch64-unknown-linux-musl`) produces a **statically linked** executable: the
Khora runtime and the musl C library are inside it, it asks for no dynamic
loader, and it runs on any Linux kernel of that architecture — Alpine, a
distroless `static` image, or `scratch`, which has nothing in it at all. The
Linux toolchain carries the musl runtime and C library; the build needs only
the C compiler driver any build needs.

```dockerfile
# --- build ------------------------------------------------------------------
# On the machine doing the build, whatever the image is for: the toolchain
# links either architecture's static executable, so `docker buildx build
# --platform linux/amd64,linux/arm64` needs no emulator.
FROM --platform=$BUILDPLATFORM debian:trixie-slim AS build

# clang drives the link. lld is needed only to build for the other
# architecture (an arm64 binary from an x86-64 machine, or the reverse).
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates clang lld curl git tar \
    && rm -rf /var/lib/apt/lists/*

ARG KHORA_VERSION=0.5.0
RUN curl -fsSL https://raw.githubusercontent.com/codyspate/khoralang/main/install.sh \
      | sh -s -- --version "${KHORA_VERSION}" --to /opt/khora --no-modify-path
ENV PATH="/opt/khora/bin:${PATH}"

WORKDIR /src
COPY . .
# The architecture of the image being built, which BuildKit sets: `amd64` or
# `arm64`. A plain `docker build` sets it to the machine's own.
ARG TARGETARCH
RUN case "${TARGETARCH}" in \
      amd64) arch=x86_64 ;; \
      arm64) arch=aarch64 ;; \
      *) echo "no static Khora target for ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
    && khora install \
    && khora build . --release --target "${arch}-unknown-linux-musl" --out /out/myservice

# --- run --------------------------------------------------------------------
FROM scratch

# Only if the service opens outbound TLS: a TlsClient verifies against a
# trust store, and `scratch` has none. Without this line `client()` raises
# `TlsError::BadCertificate` before it connects.
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build /out/myservice /myservice

USER 10001
EXPOSE 8080
ENTRYPOINT ["/myservice"]
```

What the image contains is the one file, plus the certificates if you copied
them. Docker writes `/etc/hosts` and `/etc/resolv.conf` into every container,
`scratch` included, so `localhost` and DNS names resolve without anything else
in the image. `USER 10001` needs no `/etc/passwd` entry; a numeric user is
enough.

**What an empty image cannot do:**

- **Run other programs.** `std::process` starts programs that are in the
  image, and `scratch` has none: `run`, `output` and `checked_output` raise
  `ProcessError` for any program you did not copy in, and `shell` raises it
  always, because there is no `/bin/sh`. A service that shells out — a
  `git` call, a health hook — needs a base image that has them:
  `gcr.io/distroless/static` plus the programs, or `busybox` for a shell and
  the usual tools.
- **Verify TLS without a trust store.** A `TlsClient` with no certificates to
  verify against fails every handshake with `TlsError::BadCertificate`,
  naming no certificate you supplied. The `ca-certificates.crt` line above is
  the fix, and `SSL_CERT_FILE` names a bundle elsewhere.

Fibers on both backends, sockets, TLS client and server, and the standard
library's own test suite all run in a static build. Its costs: the executable
is self-contained and therefore not small (a release hello world is about 9 MB,
against about 10 MB for the dynamic build, most of it the runtime both carry),
and `--lib` is refused for a musl target, because a shared library is loaded
into a process that already has a C library.

**Native libraries** named in `[build] link` must be built for musl and placed
in a `[build] link-search` directory: a static musl build takes nothing from
the machine's own library directories, whose archives are built against its
glibc. `m`, `pthread`, `dl`, `rt` and `util` need nothing — musl's C library
already holds them.

## Cross-building

**Linux programs can be built for the other Linux architecture**, as static
musl executables: from an x86-64 machine, `khora build --target
aarch64-unknown-linux-musl` gives an arm64 binary for an arm64 `scratch` image,
and the reverse works the same way. This needs `ld.lld` (`apt install lld`),
because the system linker reads only its own machine's objects.

A **dynamically linked** program for another architecture is not supported:
`aarch64-unknown-linux-gnu` from an x86-64 machine needs that architecture's
glibc, which the toolchain does not carry. Build those on the architecture
they run on — the toolchain is published for both `x86_64-unknown-linux-gnu`
and `aarch64-unknown-linux-gnu`. Building for Linux from a macOS or Windows
toolchain is not supported either.

For a multi-architecture image, the Dockerfile above builds both from one
machine: `docker buildx build --platform linux/amd64,linux/arm64 .` runs its
build stage natively and picks the target from `TARGETARCH`.

## Health and shutdown

Expose the readiness and liveness endpoints your orchestrator expects as
ordinary routes; there is nothing Khora-specific in them. A probe that sends
`HEAD` — a Docker `HEALTHCHECK` running `curl -I`, a load balancer target
group configured that way — is answered by the `GET` route without a second
mount, with the headers `GET` would have sent and none of the body. So a
health route is one route however the orchestrator asks for it.

Shutdown works, with edges worth stating plainly. **The `SIGTERM` a runtime
sends before its grace period becomes a cancellation at the root of the
program**: nursery cancellation runs, `scoped` finalizers run, in-flight
requests unwind, and the process exits 130. The grace period stays the
orchestrator's — `--stop-timeout`, or Kubernetes'
`terminationGracePeriodSeconds` — because a second deadline inside the runtime
would be the one number nobody configured. A second `SIGTERM` restores the
default disposition and re-raises, so an operator who means it is not waiting
on a slow finalizer.

Two shapes are **not** covered and a drain at the layer above is still the
answer for them: a `main` that reaches no cancellation point at all -- no
loop, no blocking call, no call to a function with one -- has nowhere to stop,
so the runtime kills the process outright; and a fiber inside a blocking
`connect_to` reaches no cancellation point until the kernel gives up. Writing
handlers so that being killed between two instructions is recoverable at the
next start remains worth doing — `SIGKILL` at the end of the grace period has
not gone anywhere.
