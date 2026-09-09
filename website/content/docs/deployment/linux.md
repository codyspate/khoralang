---
title: Linux
sidebar:
  order: 3
---

Linux is a primary native deployment target for Khora services.
`x86_64-unknown-linux-gnu` is one of the three triples the release workflow
builds, packages, and then uses to compile and run a program before publishing
— see [Supported targets](/docs/deployment/supported-targets/) for the whole
list and for what is deliberately not on it.

## Install the toolchain on the build machine

```bash
curl -fsSL https://raw.githubusercontent.com/codyspate/khoralang/main/install.sh | sh
```

That downloads the release for this platform, checks it against the published
`.sha256`, unpacks it into `~/.khora`, and appends a `PATH` line to whichever of
`~/.profile`, `~/.bashrc` and `~/.zshrc` already exist. Nothing is compiled,
nothing needs root, and `rm -rf ~/.khora` undoes it.

For a build machine, pin the version and leave the shell profiles alone:

```bash
curl -fsSL https://raw.githubusercontent.com/codyspate/khoralang/main/install.sh \
  | sh -s -- --version 0.1.0 --to /opt/khora --no-modify-path
export PATH="/opt/khora/bin:$PATH"
```

`sh -s --` is how arguments reach a script that is being piped. `--pre` adds
release candidates to what a plain run would take; a plain run never reaches
one, because candidates are published as GitHub pre-releases.

**One thing is not in the download and cannot be: a linker.** A Khora program
is a native object that has to be linked against this platform's C runtime, and
the driver that knows where those live belongs to the platform. Install
`clang` or `gcc` from the package manager. The installer checks before
downloading anything and warns rather than refusing, so a missing linker shows
up at `khora build` if you skip it.

## Build

```bash
khora install                # only if the project has dependencies; needs git
khora check .
khora test .
khora build . --release
```

The executable is `build/<package>`. Run it:

```bash
./build/myservice
```

Nothing else is copied to the target host. The binary carries the Khora runtime
and does not need `std/`, the compiler, or a language VM beside it. What it does
need is the system C library it was linked against — the published Linux
artifact is dynamically linked against glibc, and static and musl builds are not
produced or tested — and `ca-certificates`, if the service opens outbound TLS
connections, because a `TlsClient` verifies against the machine's own trust
store.

**Build on the machine family you deploy to.** Cross-compilation is not
supported: `KHORA_TARGET` makes the compiler emit for another triple, which
checks code generation and does not produce a runnable artifact.

## Running it under systemd

Khora contributes nothing special here — the unit is the ordinary one for a
static native binary, which is the point:

```ini
[Unit]
Description=myservice
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/myservice
WorkingDirectory=/var/lib/myservice
User=myservice
Group=myservice
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

```bash
sudo install -m 0755 build/myservice /usr/local/bin/myservice
sudo systemctl daemon-reload
sudo systemctl enable --now myservice
journalctl -u myservice -f
```

`WorkingDirectory` is worth setting deliberately. A relative path a program
opens is relative to where the process was started, not to where its
`khora.toml` was, and an `[permissions.fs]` grant written as `./data/**` is
resolved against the manifest — so a service started from `/` and a service
started from its own directory disagree about which files exist.

`std::log` writes one JSON object per line to standard error, which is where
`journalctl` and every container log collector already look; there is no log
file to rotate unless you make one.

## Shutdown, and what it currently is not

**`std` has no signal API in this release.** A `SIGTERM` from `systemctl stop`,
a container runtime, or a Kubernetes eviction takes the process the way the
kernel takes any process that has not asked otherwise: immediately, without
unwinding. Nursery cancellation does not run, `scoped` finalizers do not run,
and in-flight requests are dropped.

So a graceful shutdown has to be arranged outside the program for now — drain
the load balancer, then stop the process — and a service should be written so
that being killed at an arbitrary instant is survivable: commit before
acknowledging, and let the next start recover. [Cancellation-safe
resources](/docs/cookbook/cancellation-safe-resources/) is the pattern for the
cancellations Khora *can* deliver, which are the ones a nursery raises inside
the program.
