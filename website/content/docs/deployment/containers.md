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
ARG KHORA_VERSION=0.1.0
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
docker build --build-arg KHORA_VERSION=0.1.0 -t myservice:1 .
docker run --rm -p 8080:8080 myservice:1
```

## Why both stages are the same base image

The published Linux artifact is dynamically linked against the system C
library, and static and musl builds are not produced or tested
([Supported targets](/docs/deployment/supported-targets/)). So the runtime image
has to supply a glibc at least as new as the one the binary was linked against.
Same distribution and same release in both stages is the way to stop thinking
about it, and **which** release is not free to choose: the published toolchain
needs glibc 2.39 or newer, so `bookworm` cannot run it and `trixie` can. That
is why both stages above are trixie and not the more familiar bookworm; the
table on [Supported targets](/docs/deployment/supported-targets/) is the list.
A distroless runtime image works for the same reason and is smaller, as long as
it is the Debian 13 variant; Alpine does not, because it is musl.

Nothing else from the build stage is copied. There is no `std/`, no compiler,
and no toolchain in the final image: the executable carries the Khora runtime
inside it, which the release workflow checks on every release by running a
packaged toolchain's output with nothing else on the path.

## Cross-building

**There is none, and this is the part to plan around.** Cross-compilation is
not supported: the compiler can emit for another triple under `KHORA_TARGET`,
which checks code generation, but the linker and sysroot story is unfinished,
so a deployable artifact means building on the platform it runs on. `linux/arm64`
is not built or tested by CI at all.

In practice that means `docker build` on an `x86_64` Linux machine or runner —
`--platform linux/amd64` if your daemon would otherwise pick something else —
and not `docker buildx` fan-out across architectures.

## Health and shutdown

Expose the readiness and liveness endpoints your orchestrator expects as
ordinary routes; there is nothing Khora-specific in them. A probe that sends
`HEAD` — a Docker `HEALTHCHECK` running `curl -I`, a load balancer target
group configured that way — is answered by the `GET` route without a second
mount, with the headers `GET` would have sent and none of the body. So a
health route is one route however the orchestrator asks for it.

Shutdown is the part where a container's assumptions and Khora's current
capabilities disagree, and it is worth stating plainly. **`std` has no signal
API in this release**, so the `SIGTERM` a runtime sends before its grace period
terminates the process immediately: no nursery cancellation, no `scoped`
finalizer, and every in-flight request dropped. Structured cancellation is
delivered inside the program by a nursery, not by the kernel.

Until there is a signal surface, drain at the layer above — remove the container
from the load balancer or endpoint list, wait, then stop it — and write handlers
so that being killed between two instructions is recoverable at the next start.
