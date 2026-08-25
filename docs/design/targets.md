# Targets

Khora compiled for the machine it was running on and nothing else, because
`target_machine` initialized only the native target and there was nowhere for a
`--target` to point. **Step one of the four below is done**: it initializes
every backend inkwell was built with and takes its triple from `KHORA_TARGET`,
and `tests/targets.rs` proves an object comes out for the machine that was
asked for — a WebAssembly module, an aarch64 ELF and an x86-64 ELF, all from a
Windows host.

Steps two to four are not, so a cross build stops at the link with a message
that says which target its object was for and that a linker and sysroot are
what is missing. Nothing here yet *runs* on another machine.

That is a smaller problem than it looks for correctness and a large one for
adoption: `docs/positioning.md` puts Khora where a team is choosing Go, and
`GOOS=linux GOARCH=arm64 go build` is a headline feature of the thing being
compared against.

## This is not a `std` question

`docs/design/ecosystem.md`'s rule decides what belongs in the standard library.
Almost nothing here does. A target is a property of the compiler and the
runtime, and the only `std` consequence is that a module which binds a syscall
needs one implementation per platform — which `std::net` already does, in
`socket_linux.kh`, `socket_macos.kh` and `socket_windows.kh`. The pattern is
established; there would just be more of it.

## Cross-compilation first, WebAssembly as its first consumer

The temptation is to treat wasm as a special project. It should be the second
target that works, not the first exception:

1. ~~a `--target` flag, and a `TargetMachine` built from a triple rather than
   from the host~~ — **done**, as `KHORA_TARGET`. It reuses the variable that
   already chose which `std` files a build reads, rather than adding a second
   one that could disagree with it: a build cannot now generate for one
   platform while compiling another's bindings. Three-letter families
   (`linux`, `macos`, `windows`) keep their old meaning of "the host's
   architecture, that platform's `std`", because a build that quietly changed
   architecture under somebody would be a surprise;
2. `khora-rt` cross-compiled for that triple, and its platform bindings selected
   by it rather than by `cfg(windows)` versus `cfg(unix)`;
3. a linker and sysroot for the target, since `clang` currently drives the host
   linker with the host's libraries;
4. the toolchain able to *fetch* a target's runtime and sysroot, which is
   roadmap 10.6's version-management machinery pointed at a second axis.

Doing those four buys `linux/arm64` for containers, `x86_64-unknown-linux-musl`
for a static binary in a `scratch` image, and macOS from a Linux CI runner —
each of which is worth as much as wasm to somebody, and none of which needs a
new idea. Then wasm is the target that exercises the mechanism hardest.

## WebAssembly

**The compiler side is nearly free.** `docs/llvm-setup.md` §4 records that the
pinned LLVM already has the WebAssembly backend compiled in — it is in
`llvm-config --targets-built` — and inkwell's feature was switched off only
because nothing emitted for it. Nothing in inference, row unification,
monomorphization or Perceus cares what the target is.

**The runtime side has one obstacle that is not a matter of work.**

### WebAssembly cannot switch stacks

The call stack is not addressable memory in the MVP. A stackful coroutine has to
save a stack pointer and restore another one, and there is no stack pointer to
save. corosensei ships backends for x86, x86_64, aarch64, arm, riscv, powerpc64
and loongarch64 and none for wasm, and that is not an oversight.

Phase 11 is built on exactly that operation. The choices are:

- **The stack-switching proposal.** The right answer, and not broadly shipped.
- **JSPI.** Works in browsers and Node, suspends only across a JavaScript
  boundary, and ties the runtime to a host that has JavaScript in it.
- **Asyncify.** Binaryen rewrites the whole program into a state machine that
  can unwind and rewind. It works everywhere and costs code size and speed —
  and it is the same state-machine transform Phase 11 rejected for the language
  itself, arriving through the back door.
- **Do not have fibers on wasm.** Blocking, single-threaded, one request at a
  time.

### The fourth option is better than it sounds

An edge isolate is single-threaded anyway. Cloudflare gives a Worker one thread;
so does Fastly; a Lambda handler is one invocation. The concurrency a fiber
buys is worth much less there than it is in a long-lived server, and the thing
that actually matters — not blocking a worker on I/O — is the *host's* problem
in that model.

11E's blocking pool already has the shape: `blocking()` checks whether there is
a worker to protect and calls straight through when there is not. A wasm build
where that is always the answer is a supported configuration rather than a
special case.

**So: ship wasm without fibers, and let stack-switching add them later.** The
alternative is shipping Asyncify's cost to every wasm user for a feature most of
them cannot use.

### The other porting work, in order of nuisance

- **Threads.** Workers, the timer thread, the reactor thread and the blocking
  pool all use `std::thread`. wasm needs the threads proposal, a
  `SharedArrayBuffer` and cross-origin isolation in a browser, and none of that
  in an isolate. Single-threaded mode is the same work as the item above.
- **Sockets.** `reactor.rs` binds `poll` and `WSAPoll`; `net.rs` binds `recv`,
  `send` and `accept`. WASI preview 2 has sockets; a Cloudflare Worker has
  `fetch` and nothing that looks like a socket. This is where the target
  flavours stop being interchangeable.
- **TLS.** `rustls` with `ring`. Partial wasm support, and in an isolate the
  host terminates TLS anyway, so the honest answer may be that
  `std::net::tls` is not available on that target rather than that it is slow.
- **Time and randomness.** `khora_unix_millis` and the entropy source both need
  host calls. Small, but they are in `std`'s `Clock` and `Random` handlers.
- **Stacks.** No `mmap`, so corosensei's guard-paged allocation does not apply —
  moot if there are no coroutines.

### Which wasm, though

The three are not one target and the choice is not obvious:

| | runtime | sockets | who wants it |
| --- | --- | --- | --- |
| `wasm32-unknown-unknown` + JS glue | browser, Cloudflare | host `fetch` | Cloudflare Workers, browsers |
| `wasm32-wasip1` | wasmtime, Fastly, Spin | WASI sockets | Fastly Compute, Spin, CLI plugins |
| components / `wasip2` | wasmtime, jco | WASI 0.2 | anything composing wasm modules |

**The first target is `wasm32-unknown-unknown`, for Cloudflare Workers.**
That is the motivating platform and it settles several of the questions above
at once: the host does the networking through `fetch`, so `std::net`'s sockets
and `rustls` are not needed and their absence is not a gap; the isolate is
single-threaded, so the no-fibers build is the right build rather than a
compromise; and there is no filesystem, so `std::fs` and a `Db` engine are
satisfied by the host — D1 or KV behind the `Db` capability — rather than by
SQLite.

One correction worth recording because it is easy to conflate: **AWS CloudFront
does not run WebAssembly.** CloudFront Functions is a restricted JavaScript
runtime and Lambda@Edge is Node or Python. Cloudflare Workers, Fastly Compute,
Deno Deploy, Vercel Edge and Spin are the wasm platforms, and Fastly and Spin
want `wasip1` rather than the target above — so the second wasm target is a
separate piece of work and not a rename.

**Components are the interesting long game.** A WIT interface is a record of
typed functions, which is what a Khora capability already is. An effect could
become a component import rather than fighting one, and a handler could become
a component export. That is a better fit than any other language has with WIT,
and it is worth not foreclosing while the first target is chosen.

## Other targets the same mechanism buys

- **`x86_64-unknown-linux-musl`**, for a static binary in a `scratch` image.
  Go's deployment story in one line, and mostly a linking question.
- **`aarch64-unknown-linux-gnu`**, for Graviton and for arm64 containers.
- **`aarch64-apple-darwin` from Linux CI**, which removes the ten-times billing
  multiplier that `.github/workflows/runtime.yml` exists to avoid.

## What this document does not decide

The flag's spelling. Whether a target's `khora-rt` is fetched or built. Whether
`std` modules that cannot work on a target fail at import or at call. Whether
wasm gets fibers through the stack-switching proposal when it lands, or never.
