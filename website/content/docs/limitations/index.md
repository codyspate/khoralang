---
title: Known limitations
sidebar:
  order: 0
---

Khora is pre-1.0. This page exists so users can tell the difference between a
language rule, a supported feature, and unfinished work.

**The ones most likely to affect you:**

- [What a signal does](#what-a-signal-does-and-the-three-shapes-it-does-not-reach)
  — `SIGTERM` and `SIGINT` unwind the program and run its finalizers, but a
  `main` with no cancellation point, a blocking `connect_to` and Windows are
  not covered.
- [Nursery cancellation is cooperative](#nursery-cancellation-is-cooperative):
  a failing child cancels its siblings when it fails, but a sibling with no
  cancellation point runs on, and a body that keeps spawning keeps starting
  children.
- [Work taken off a queue survives cancellation, not a crash](#work-taken-off-a-queue):
  `Channel::claim` hands an unsettled job to its handler on every way out of
  the program except SIGKILL, a trap, and a handler cut short by `abort`.
- [A canceled fiber waiting on a child stops only when the child
  does](#a-canceled-fiber-waiting-on-a-child-stops-when-the-child-does), if
  it holds the child's last handle.
- [The two fiber backends behave differently](#the-two-fiber-backends-are-distinguishable)
  in scheduling order.
- [There is no `timeout` or `race`](#concurrency-combinators) in `std`; a
  deadline is built from a fiber and a clock.
- [Only Linux has a scalable I/O backend](#io-scaling-on-macos-and-windows)
  for the fiber scheduler; macOS and Windows wait on sockets with `poll`.
- [On one CPU, the scheduler can hang](#the-fiber-scheduler) a program in
  which a fiber blocks inside `Shared::update` on something another fiber
  must do. The default thread backend does not.
- [A `Map` whose keys a client chooses can be made slow](#maps-keyed-by-untrusted-input):
  the string hash takes no secret, so colliding keys can be computed ahead of
  time, and a JSON object built from them costs time in the square of its size.
- [The `postgres` package does not speak TLS](#package-ecosystem), so rows
  cross the network in the clear.
- [Database queries trail Go's](#performance-against-other-servers) on the
  TechEmpower query tests; JSON and fortunes do not.
- [Inbound connections are not permissioned](#inbound-connections-are-not-permissioned) —
  the manifest governs outbound only.

## Target coverage

Khora has versioned toolchain artifacts and installers for the platforms the
project releases. The normal path is the installer documented in
[Installation](/docs/getting-started/installation/), not compiling the compiler
from source.

A target is only called supported when the compiler, runtime, linker/sysroot,
packaging, CI and deployment path work end to end. [Supported
targets](/docs/deployment/supported-targets/) lists the ones that do.

There are three: `x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc` and
`aarch64-apple-darwin`. **Linux on arm64 and Windows on arm64 are not among
them** — the compiler can emit an object for `aarch64-unknown-linux-gnu`, but
no toolchain is published for it and a build for it stops at the link. **There
is no static or musl build**: the Linux toolchain and the programs it builds
link dynamically against glibc, so Alpine and other musl distributions cannot
run them, and a `scratch` container image is not an option.

## Recursion depth and very large lists

Khora does not guarantee tail-call optimization, so a function that recurses once per element uses one stack frame per element. Running out of stack ends the program; it reports

```
khora: the stack ran out
```

on standard error and exits with the platform's stack-overflow status.

A spawned fiber has eight megabytes of stack on either fiber backend, which is what `main` gets by default on Linux and macOS and what a Khora program's `main` is linked with on Windows, so a recursion that runs in `main` runs the same on a fiber. [Traps](/docs/reference/traps/#running-out-of-stack) has how deep that goes.

**Where the message is verified.** On Linux, on every stack Khora code runs
on: `main`, a fiber on either backend, a test, and a stack that runs out while
the runtime is starting a thread or a process. On macOS and Windows it is
verified by continuous integration only, which covers `main`, a fiber on
either backend and a test on both, and the thread-start case on macOS.

**A thread a C library starts can still overflow silently.** Before the
runtime starts a thread or a process it makes sure 16 KB of stack is left, so
that an overflow lands where the message can be printed. A C library linked
through [`build.link`](/docs/reference/ffi/#link-against-a-native-library)
that starts threads of its own does not do that, and on Linux the C library
blocks signals while it starts one: a Khora stack that runs out during that
start ends the process with no message. It needs a call into such a library
from very deep in a recursion.

Every traversal in `std::core`'s `List` is written as a loop rather than as recursion — `length`, `fold`, `reverse`, `filter`, `take`, `drop`, `any`, `all`, `find`, `contains`, `zip`, `flat_map`, `sum`, and the `merge` inside `sort` — so walking a list of any size is safe. `List::sort` recurses only to divide, which is about `log2(n)` deep.

String operations are loops too. `split`, `join` and `repeat` handle inputs of
any size, and `join` is linear rather than quadratic in the length of its
result.

`std::json` reads and writes documents of any length with loops. Nesting is
the exception: each level of `[` or `{` takes a stack frame, so `parse`
refuses a document nested deeper than 512 levels with a `JsonError` rather
than letting the document decide how much stack it uses, and
`parse_with_depth` takes another limit. `encode` and `Show for Json` have no
limit and recurse once per level of a `Json` the program built itself: one
nested 200,000 levels deep runs the stack out in a debug build. A `Json` that
came from `parse` is at most 512 levels deep, so only a value the program
nests by hand can reach that. A derived `Eq`, `Ord` or `Show` on a deeply
nested value of the program's own recursive type recurses the same way.

**`fold_lines` copies a long line once per chunk it spans.** It reads a file
64 KB at a time and joins what is left of an unfinished line to the next
chunk, so a line longer than a chunk is copied again for every chunk it
crosses: the cost grows with the square of the line's length. A file of
short lines is unaffected. A 16 MB file with no newline took 819 ms in a
release build, against 19 ms for the same bytes in 1 KB lines, and doubling
the line quadruples the time. To read a file that may be one long line, use
`fold_chunks` and split it yourself, or `read_text` when it fits in memory.

Releasing a value costs no stack either: reference counting frees a value's children through a queue rather than by recursing, so letting go of a long list is a loop like walking one. A million-element `List` sorts.

What is left is ordinary recursion that somebody writes. A function that calls itself once per element of its input will use a frame per element, and no analysis in the compiler turns that into a loop.

`Array<A>` and `Vector<A>` are the better shape for a large indexed collection; a list is for building front-to-back and walking once.

## Package ecosystem

Dependencies can be pinned reproducibly to git revisions, but there is not yet a public package registry or broad third-party ecosystem.

**Three packages are maintained in the Khora repository**, and they are what "the ecosystem" means today: [`postgres`](/docs/packages/postgres/), `ai` — the effect a caller names when it wants model inference — and `otlp`, an exporter for `std::trace` over OTLP/HTTP JSON. [Packages](/docs/packages/) lists all three and gives the manifest line each is depended on with. Beyond those there is nothing to install: no registry to search and no third-party publishing.

**One database driver is published: `postgres`.** `std::db` defines `Db`, transaction semantics and cancellation behavior, and [`packages/postgres`](/docs/packages/postgres/) satisfies that interface — it speaks the wire protocol directly, authenticates with `scram-sha-256`, and supplies the `Db` handler. Depend on it with a git revision and a `subdir`; there is no registry yet. **SQLite and D1 have no driver**, and a program that needs one writes its own handler — `Db` is a record of closures, so that is a day's work and a test double is a few lines — over a native client it links with [`build.link`](/docs/reference/manifest/#build--what-to-produce).

**`postgres` does not speak TLS.** It authenticates with `scram-sha-256`, so
the password itself never crosses the network, but every query and every row
read back travel in the clear. Connect over a loopback address or a network
you already trust — a private network or a tunnel — and do not point it at a
managed database across the public internet. A server whose `pg_hba.conf`
admits only `hostssl` connections refuses it.

**A dependency cannot link a native library on your behalf.** `build.link` is read from the root package's manifest and nowhere else, so a package that ships an archive and declares `extern fn` against it cannot put a flag on your link line — a transitive package adding a native library to your build would be a supply-chain change with no signal at the place that would have to consent. The package does the work and documents one line for you to add, which means every native library a program links can be read off its own manifest. [Foreign function interface](/docs/reference/ffi/#link-against-a-native-library) has the shape.

## Cryptography

[`std::crypto`](/docs/stdlib/api/crypto/) has SHA-256 and SHA-512, HMAC with
constant-time verification, a constant-time byte comparison, hex and base64,
and random bytes from the operating system. **It has no ciphers, no
authenticated encryption, no public-key signatures (Ed25519, ECDSA), no key
exchange and no password hashing (argon2, scrypt, bcrypt).** TLS for HTTP
connections is in [`std::net::tls`](/docs/stdlib/api/net/tls/). For the rest,
bind a native library through the [foreign function
interface](/docs/reference/ffi/). `std::crypto` is not available when compiling
for WebAssembly.

## Editor tooling

`khora lsp` provides compiler-backed diagnostics, hover, formatting, completion, signature help, go-to-definition, references, document/workspace symbols, semantic tokens, code actions, code lenses, and inlay hints.

Rename covers a declaration and every file that names it, including the import that brings the name into each file, within the opened project; it does not edit dependency code outside the opened folder or in the package store. It renames the original rather than a file's own alias. A parameter's rename covers the labeled arguments written against it; a method's labels are found by its type and method name, so two modules that each declare a type (or a trait) and a method of the same names can have each other's labels renamed. It refuses a **trait member**, whose name belongs to the trait and to every impl of it, and a **constructor**, which has no recorded range to edit. It also refuses a new name that is not a usable identifier or that is already bound anywhere in the same function, even where that binding is in a separate `match` arm and could not have been captured. Further refactoring operations are editor-tooling work.

**A record field has no go-to-definition, references or rename**, inside or
outside the module that declares it. Renaming a field is a search and an edit
by hand, and since a field is private to its module unless it is marked
`pub`, the search can usually stop at that module's edges.

**The language server reads lint and formatter settings and dependencies once, when it
starts**, from the `khora.toml` at the root of the folder the editor opened.
An edit to the manifest or to a lint group file, or a dependency installed
while the server is running, takes effect after it restarts. Opening a
workspace root reads its members' source files but does not resolve their
individual dependencies; open the member directory for dependency-aware
editor checks. A workspace's `[workspace.lints]` and member manifests are not
consulted for lint levels when the workspace root is opened, so the levels it
shows can differ from what `khora check` reports in a member package. The
command-line check is the authority.

See [Editor setup](/docs/getting-started/editor/) for the language-server command and client setup.

## A few build errors name the wrong file

Almost every error is found by `khora check`, which runs per file and names the
file and line it is in. A small number are found only while generating code —
an integer pattern too large for an `Int`, a compiler intrinsic such as
`Fiber::wait` taken as a value (`let w = Fiber::wait;`). Those errors carry a
line and column
but not a file, and `khora build` renders them against the first source file
it read, which may be a standard-library file or another file of your package.

**Trust the message and the line number less than you would a `check` error.**
If `khora check` passes and `khora build` reports an error at a place that
cannot be the cause, search your own sources for the construct the message
names. `khora check` passing and `khora build` failing is always worth
[reporting](#reporting-a-limitation): each one is a rule the checker has not
learned.

## Standard-library API docs

`khora doc` generates the checked-in standard-library API reference from compiler-resolved declarations plus `///` and `//!` documentation comments. `khora doc --check` is used to detect drift between the source declarations and generated pages.

Two important documentation-tooling gaps remain:

- An API code block is only type-checked if it declares its own `module`. One of the roughly 1,200 blocks on the generated pages does, and it is type-checked on every CI run; the rest are parsed as fragments and none are executed.
- Generated signatures name referenced types but do not yet cross-link those type names to their API pages.

See the [Standard library](/docs/stdlib/) entry point for the generated reference.

## HTTP surface

Assume nothing beyond what this section lists.

**The verbs are `GET`, `POST`, `PUT`, `PATCH` and `DELETE`, routed to
handlers, plus `HEAD` and `OPTIONS`, which the router answers from what is
mounted unless you mount a handler for them.** Anything else — `TRACE`,
`CONNECT`, an extension method — is answered `400` and the connection closed,
because `Method::of` does not name it and an unparsed request line is a
malformed one as far as the reader is concerned. The failure is silent from the
client's side: a `400` to a verb the server does not know reads as the client's
mistake.

**A request is capped at 8 KB by default, headers and body together, and the
cap is configurable.** Past it the server answers `413` before parsing
anything, so the handler never runs. The number answers "how much may an
unauthenticated client make a server hold" rather than "how large can a
request be" — nothing in the parser recurses per byte or per line, and a
39,808-byte request carrying 2,001 headers parses when the limit admits it.
`Router::holding` sets another; the buffer is allocated once at that size per
connection, so it multiplies by the connection bound below when deciding what
a full server costs. There is no multipart decoding, and a body must be UTF-8
text. **Chunked transfer is read but not written:** `HttpClient` accepts a
response framed by `Transfer-Encoding: chunked` and hands the handler the
de-chunked body, so a service that answers that way can be called; the server
never writes a chunked response. That is the split real traffic has — a
response of unknown length is ordinary and a request of unknown length is not.

**The server serves at most 256 connections at once, and the number is not
configurable.** `Router::listen` and the TLS form wrap their accept loops in
`bounded_nursery(256, ..)`, and an accepted connection is a fiber that is
inside your handler for as long as the handler runs. There is no second,
smaller pool that handlers queue for, so that one number is both the most
connections served at once and the most handlers running at once; the next
connection waits for one of them to finish. It is a property of the reference
server rather than of the `Router` type, so it does not appear in the generated
[`std::net::http` pages](/docs/stdlib/api/net/http/); [Serve HTTP
requests](/docs/cookbook/http-service/) is where it is discussed.

## Performance against other servers

On the TechEmpower read tests, run against PostgreSQL on one machine with
Khora on the scheduler backend, the **database query tests trail Go**: the
single-query test reached 74% of Go's requests per second and the 20-query
test 84%. The JSON test was ahead of Go (114%) and fortunes level with it.
Tail latency was at or below Go's in every test except the single query, where
the 99th percentile was 6.7 ms against Go's 6.4 ms. These are one run on one
machine, not a published benchmark; [Performance](/docs/performance/) says
what a figure has to satisfy before it is published, and why.

## Inbound connections are not permissioned

**`[permissions] network` governs outbound connections only.** The grant list is
consulted in exactly one place in the standard library — inside
`HttpClient::send`, after the URL is parsed and before anything is dialed — and
what it decides is whether that host may be reached. Nothing on the server side
consults it: `Router::listen`, `Router::listen_quietly`, `Router::listen_tls`
and the `listen_on` under them bind and serve without asking. A program with
`default = "deny"` and `network = []` opens whatever port it is given and
answers requests on it.

So do not read a `network` grant as an authorization to listen, and do not read
its absence as a refusal to. `network = ["127.0.0.1:8787"]` written to allow a
server to bind 8787 does nothing at all — it neither permits the bind, which
needed no permission, nor restricts it. The mistake is easy to make and leaves
no trace: the program works, and the line that was supposed to be holding it
back is not.

The reason it is this way rather than fixed is that the two directions do not
share a vocabulary. `network` is a list of hosts a program may *reach*. A port
it may *bind* is not a host it reaches, so extending the same list to cover
listening would produce a control whose meaning could not be written down —
`api.example.com:443` would have to mean one thing as a destination and another
as a local port, and a permission that cannot be stated is a permission nobody
can audit. Naming inbound authority properly needs a grant of its own, and what
that grant ranges over — ports, interfaces, both — is an open design question
rather than a fix waiting to be typed. Until there is one, a program's ability
to listen is bounded by the operating system and by whatever runs it, not by its
manifest.

## What a signal does, and the three shapes it does not reach

`SIGTERM` and `SIGINT` become a **cancellation at the root of the program**.
There is no new API and nothing to spell: a program observes a signal as the
cancellation it already knows how to observe, so nursery cancellation runs,
scoped finalizers run, a `std::db` transaction sends its `ROLLBACK`, and the
process exits **130**. `Ctrl-C` is `SIGINT` and behaves the same way.

**The second signal is forceful.** The runtime restores the default
disposition and re-raises, so the process dies the way the platform says —
a real `WIFSIGNALED`, not an `exit(143)` that prints the same number. The
operator holds the deadline: there is no grace period in the language, because
a runtime timer that disagreed with `TimeoutStopSec` or
`terminationGracePeriodSeconds` would be the one number nobody configured.

Measured, on a program with a scoped finalizer in a fallible loop:

```
kill -TERM -> exit 130; the finalizer ran
kill -TERM twice -> killed by signal 15 mid-finalizer
```

### What this does not cover

**A `main` that reaches no cancellation point dies rather than unwinding.**
Such a `main` — no loop, no blocking call, no call to a function that has one
— has nowhere to stop, so the runtime uses the platform's default instead:
default disposition, re-raised at once. The program still answers `kill`, at
wait-status 143 with no finalizers. Any `main` that loops, serves or waits
takes the graceful path, whatever its `raises` row.

**A compute-bound call to foreign code still delays the shutdown.** A fiber is
asked to stop at a cancellation point, and a single C call already in progress
is not one: it finishes first. The status is the truth about the outcome and
not about the latency.

**A blocking `connect_to` is not a cancellation point.** `connect_to` is a
blocking `connect(2)` on the worker, so a fiber inside one reaches no
cancellation point until the kernel gives up — minutes, on an unroutable host.
`accept` and `recv` are fine: they are reactor-driven, and an idle
`Router::listen` server stops in about ten milliseconds. A non-blocking connect
driven by the reactor would make it one, and it is not built.

**Waiting for a child process is not a cancellation point.** `process.run`,
`process.output` and `process.shell` wait for the program they started in one
blocking call, so a fiber inside one stops only after that program exits. The
program is not killed when the fiber is canceled; whether it should be is not
decided. Put a bound on the program itself (`timeout 10 ...`) where that
matters.

**A change function that never returns cannot be stopped.** A
`Shared::update` or `modify` change function holds the cell's lock, so
nothing in it stops at a cancellation point -- not even after `Fiber::abort`
or `cancel_within`. A blocking call inside one gives up, but a loop that never
ends holds the lock, and the fiber, until the process ends.
[Sharing](/docs/reference/sharing/) has the rule.

**Windows has none of this.** Windows has no `SIGTERM`, and no way for an
arbitrary process to ask another to stop: `TerminateProcess` is `SIGKILL` with
no notice and nothing to observe. The console events (`CTRL_C_EVENT` and its
two siblings) are not wired up in this release.

So a program that must not lose work should still be crash-only — durable state
advancing by one atomic append or rename — because `SIGKILL`, a power cut and
the cases above all remain. The ordinary deploy on Linux or macOS — one
`SIGTERM` to a program whose `main` loops, serves or waits — is not one of
them.
[Running on Linux](/docs/deployment/linux/) and
[Containers](/docs/deployment/containers/) say this in the setting where it
bites.

## The fiber scheduler

A fiber is an operating-system thread. The M:N scheduler — stackful coroutines on a worker pool — is built and is opt-in with `KHORA_FIBERS=scheduler`.

It is not the default, for three reasons, and one of them is a gap rather than a preference:

- Threads are faster at the connection counts a service runs at.
- The scheduler exists for fiber **density**. A hundred thousand waiting
  fibers measured about 4 KB each on Linux as well as Windows, but on Linux
  each fiber's stack and its guard page are two memory mappings, so a process
  with more than about 32,000 fibers needs `vm.max_map_count` above the 65,530
  many distributions set by default. Each fiber also reserves eight megabytes
  of address space for its stack, so a host with `vm.overcommit_memory=2`
  runs out of commit long before it runs out of memory.
- It is the less-exercised path, and therefore the likelier home of the next runtime bug.

The two backends are distinguishable under cancellation — see [The two fiber
backends are distinguishable](#the-two-fiber-backends-are-distinguishable)
below — so the default cannot change without a breaking-change note.

**A known defect: on one CPU, the scheduler can hang a program in which a
fiber blocks inside a `Shared` change function.** If a fiber inside
`Shared::update` or `modify` waits on a channel, and the fiber that will send
on it then needs the same cell, the scheduler hangs when the process may run
on only one CPU — a one-vCPU container or VM, or `taskset -c 0`. With two or
more CPUs it finishes. The thread backend finishes either way. Measured on
x86-64 Linux with the scheduler pinned to one CPU: 10 of 10 runs hung, against
0 of 10 on two CPUs and on four. The change function holds its worker's thread
while it waits, and with one worker nothing else can run the fiber it is
waiting for. [Sharing](/docs/reference/sharing/) already advises keeping
blocking work out of a change function; on one CPU under the scheduler, that
advice is required.

## I/O scaling on macOS and Windows

This matters only under `KHORA_FIBERS=scheduler`. Under the default thread
backend each fiber is an operating-system thread waiting on its own socket, and
the ceiling is threads rather than the wait.

Under the scheduler, a fiber waiting on a socket parks and one reactor wakes it
when the socket is ready. **On Linux that reactor uses `epoll`**, which returns
only the sockets that are ready. **On macOS and Windows it uses `poll`**
(`WSAPoll` on Windows), which hands the kernel every watched socket on every
wait, so the cost of one wait grows with the number of open connections.

The scheduler exists for fiber density — many mostly idle connections — and
that is exactly the case `poll` serves worst. A program built for tens of
thousands of long-lived connections should run on Linux. macOS would need a
`kqueue` backend and Windows an IOCP one; neither exists.

## Characters and strings

A `String` is UTF-8 and is indexed in **bytes**. `String::slice` stops the program if a cut lands inside a character, so ask first:

```khora
let safe = String::slice(text, 0, String::next_boundary(text, 20));
```

`is_char_boundary`, `next_boundary`, `previous_boundary`, `char_at`, `chars` and `char_length` are the character-level API; `byte_length` is the constant-time one and `char_length` walks.

The character predicates — `Char::is_digit`, `is_alpha`, `is_whitespace`, `to_upper`, `to_lower` — are **ASCII only** and say so in their own documentation. Unicode case mapping and the full `Nd` category are not in `std`, deliberately: they need tables that would double its size, and a library is the right place for them.

## Maps keyed by untrusted input

**A `Map` whose keys come from a client can be made to take time in the square
of its size.** `Map` is a hash table, and the hash of a `String` is FNV-1a
with no per-process secret, so anybody can compute in advance a set of keys
that all land in one bucket. Each insert or lookup then walks a chain as long
as the map. Measured in a release build on x86-64 Linux: 8,192 colliding keys
took 249 ms to insert against 10 ms for ordinary ones, and 16,384 took 1,032
ms against 17 — twice the keys, four times the time.

The path a server meets it on is JSON: `std::json::parse` builds every object
as a `Map<String, Json>`, so a request body whose member names were chosen to
collide costs the server that time while it parses. A 1.7 MB object of 16,384
colliding names took 1.2 s to parse, against 25 ms for the same size with
ordinary names. The default request limit of 8 KB keeps a body far below that.
The [JSON API recipe](/docs/cookbook/json-api/) raises it to 1 MB with
`Router::holding(1048576)`; at that limit a 786 KB body of 8,192 colliding
names took 299 ms to parse, against 15 ms for ordinary ones, on every request
that sends it.

What to do about it: keep `Router::holding` as low as the application allows,
and for a structure keyed by client-chosen strings that can grow large, use
`Dict`, which is ordered by comparison rather than by hash and has no worst
case of this shape.

## Anonymous union types

There is no way to write "an `Int` or a `String`" **inline**, as the type of a value, without declaring anything.

A named [variant type](/docs/reference/types/#variant-types) — a discriminated union, in other languages' words — is how "one of several" is expressed, and it is exhaustively checked:

```khora
pub type Answer =
  | Number(Int)
  | Text(String);
```

What is missing is the anonymous form. Declaring `Answer` is the cost, and the compiler's exhaustiveness checking is what it buys.

`+` joins the failure types of a `raises` row and means nothing outside one; `T: Eq + Show` is the other meaning of the symbol, a trait bound, and works as it does in Rust. Writing `Int + String` in a value's type is refused by name rather than by a parse error.

The practical consequence is that `attempt` handles a body raising exactly one type. Use [`catch`](/docs/reference/failures/#handle-failures-with-catch) for a wider row — it matches per type and never has to name a combined type.

[The unions design note](https://github.com/codyspate/khoralang/blob/main/docs/design/unions.md) records what an anonymous union would mean, what it would cost, and why existentials are not part of the same question.

## A canceled fiber waiting on a child stops when the child does

`Fiber::wait` and `Fiber::join` are cancellation points: a parent canceled
while waiting stops there and does not run the statement after it, whether or
not the child can fail. **But if the parent holds the last handle on the
child, it does not finish stopping until the child has**, because the handle
is released on the way out, and a canceled fiber releasing the last handle on
another passes the cancellation on and waits for it. So that parent's latency
is the child's: however long the child takes to reach its own next
cancellation point. A child that reaches one often -- a loop, a call, a sleep
-- stops at once.

A parent that waits on a child somebody else also holds stops at once: its
release is not the last, so it does not wait.

Releasing the last handle *without* being canceled waits for the child and
does not stop it: a `main` that returns while holding a handle on a fiber
that never ends does not exit. Cancel it first.

`Fiber::outcome` hands back an answer without unwinding the asker when the
answer is wanted.

## Finalizers under `abort`

`Fiber::abort` (and a `cancel_within` deadline that runs out) stops a fiber
at its next cancellation point, cleanup included. **Every registered
finalizer still starts, and runs until the first loop trip, `!` or blocking
call it reaches**, in its own body or in a function it calls. A blocking
call is a `receive`, a `send` that has to wait, a sleep, a `join`; it stops
the finalizer there, and does not hand it a made-up answer such as a `None`
from a channel that is still open. Calling a function, directly or through a
function value, does not stop it before then. That holds however the
finalizer was registered — a lambda written in `Region::defer`, a lambda
bound to a `let` first, a named function, one that arrived as a parameter,
or the release `acquire` registers:

```khora
let finalizer = fn () => give_back(conn);
Region::defer(region, finalizer);
```

So put the step that must happen in a finalizer before anything that can
wait or loop: under `abort`, the first wait or loop trip is where it ends.
A finalizer made only of calls and `Shared::set` runs to its end. An `abort`
that arrives while a finalizer is already running is acted on at its next
cancellation point of any kind, a call included. A plain cancel does not
stop cleanup at all; only `abort` does.

## Concurrency combinators

A fiber carries its answer and its failure row — `Fiber<A, 'er>`, with `join`
re-raising what the child raised — and `Clock` can `sleep`. The combinators
built on top of those do not exist: there is no `timeout`, no `race`, and no
bounded parallel map.

**Channel fan-in is concurrent.** Two 2000 ms fibers take about 2.1 seconds
whether the parent reads their results off a `Channel::bounded(4)`, joins both
handles, or uses `join_all` — measured three runs per backend on x86_64 Linux,
and indistinguishable under `KHORA_FIBERS=scheduler`. At 500 ms per fiber over
25 runs the three are 518, 514 and 516 ms.

**A deadline can be built by hand, and a `race` only partly.** Put the work in
a fiber, sleep the deadline, `Fiber::cancel` the handle and `Fiber::wait` for
it; [Give work a deadline](/docs/cookbook/timeouts-and-cancellation/) has the
program, and it returns within a few milliseconds of its deadline when the work
has cancellation points. What such a deadline cannot do is stop work that has
none — see the section above — and a hand-written `race` waiting on several
fibers still has no way to wait on the *first* of them except through a
`Channel` the branches send to.

Two smaller things a supervisor meets on the way. `Fiber::wait` tells you
nothing about how the fiber ended. `Fiber::canceled` answers whether the fiber
was *stopped*, without waiting and without unwinding the asker, so a supervisor
can tell a shutdown from a fault; `Fiber::outcome` goes one step further and
hands back `Answered(value)` or `Stopped`, raising only when the child
*failed*. A `join` on a canceled fiber still unwinds its caller, and at the
entry point still ends the program at 130, so `outcome` is the call for a
caller that wants the answer and tolerates a stop. A stopped fiber's partial
progress is never handed back; to keep it, the child writes a `Shared` cell.

**A failure nobody takes is reported on standard error, once per fiber that
ends holding it, when that fiber's last handle is let go of:**

```
khora: a fiber ended with an error nobody was waiting for
```

It does not change the exit status and cannot be suppressed. What counts as
taking the failure:

| how the program treats a fiber that raised | the line |
| --- | --- |
| `Fiber::join`, `Fiber::outcome` or `Fiber::wait` hands back the error | not printed by that fiber's handle; the error belongs to whoever took it (the rows below) |
| a nursery child; the nursery raises `ChildFailed` | not printed: `ChildFailed` is the report |
| a nursery child, and the nursery's body raised or was canceled first, so no `ChildFailed` | printed once per such child, as the nursery ends |
| the handle is let go of and nothing took the error (end of its block, a `Fiber::wait` whose waiter was canceled before the child finished, or nothing at all) | printed once, when the handle is let go of |
| two or more fibers each take the error with `join` or `wait`, pass it on with `!`, and their own handles are let go of unjoined | printed once per such fiber: each one carries its own copy of the error |
| the error was handed back, and then dropped: a `catch` arm that is canceled before it acts on it, or `attempt(fn () => Fiber::wait(c)!)` whose `Result` is discarded | not printed: the error was taken, so nothing reports it — handle it where it arrives |
| `Fiber::detach` | not printed: detaching says the answer is not wanted |
| the fiber was canceled rather than failing | not printed: a cancellation is not a failure |

**The line marks when the handle went, not when the fiber failed.** A handle
let go of at the end of a block reports right there. One bound in `main` and
never joined reports after `main`'s last line, as the program exits. One held
by a loop that never ends reports only when that loop is stopped — on
`SIGTERM`, as the unwind releases it — and until then the failure is silent.
A supervisor that has to notice a failed worker while it runs asks
`Fiber::finished` and then `Fiber::join` or `Fiber::outcome`; [Serve
HTTP](/docs/cookbook/http-service/#stopping-a-service) has the loop.

`Channel` also has no `select` (waiting on the first of several) and no zero-capacity rendezvous. `Channel::bounded(0)` gets a capacity of one rather than a rendezvous, deliberately.

## Nursery cancellation is cooperative

A child that fails cancels its siblings at the moment it fails, whichever
order they were spawned in: an older sibling asleep for a second is canceled
within milliseconds of a younger one raising. Every child is still waited for
before the nursery raises `ChildFailed`. What a cancellation cannot do is stop
work that has no cancellation point — a long computation with no loop, call or
`!` in it runs to its end, and the nursery waits for it.

A failure cancels the children running at that moment, and every failure
does: a child spawned after one failure is canceled by the next. A body
that goes on calling `nursery.spawn` after a child has failed starts those
children, and a bounded nursery admits them as room frees up. Stop
producing work explicitly when that matters, for instance by having the
producer check a `Shared` flag the failing work sets. A bound of `n` is exact
either way: at most `n` children run at once, and a limit below 1 is refused.

`Fiber::cancel` on a fiber whose body is a nursery returns at once and cancels
the children too. The `Fiber::wait` after it still waits for every child,
including one with no cancellation point.

## Work taken off a queue

`Channel::claim` makes a job taken off a channel impossible to lose silently
inside a running program: an unsettled claim runs its abandon handler on
return, raise, `break`, discard, cancellation, and the first SIGTERM or
Ctrl-C. What it does not reach:

- **SIGKILL, running out of memory, power loss.** No cleanup runs, and the job
  in an in-memory channel is gone.
- **A trap** (overflow, `todo()`, an index out of range) ends the process at
  status 134 without running cleanup.
- **`Fiber::abort`** starts the handler, which runs until the first loop
  trip, `!` or blocking call it reaches. A handler made of `Channel::offer`
  and `Shared::set` has none and runs to its end; one with a loop or a wait
  may be cut short partway.
- **A foreign call that never returns** is not a cancellation point, so the
  claim is never abandoned.
- **A claim kept in a long-lived structure** is abandoned only when its last
  reference goes; one held by a reference cycle never is.

A plain `Channel::receive` followed by a call that can stop still loses the
job on a cancel, and the compiler does not warn about it. [Take work off a
queue safely](/docs/cookbook/taking-work-off-a-queue/) has the measurements
and the hand-written pattern for values that do not come from a channel.

## The two fiber backends are distinguishable

Under cancellation the two backends answer alike: a fiber in `clock.sleep`, on
a channel, on a socket read or on another fiber is woken by a cancellation on
either one, and stops there. What still differs is scheduling -- which fiber
runs when, and how many run at once -- so a program that depends on an order
the language does not promise can see which backend it has.

Cancellation unwinds at a point, not between arbitrary instructions --
[Concurrency](/docs/reference/concurrency/) has the model and the list of
points.

## Cross-compilation and WebAssembly

The compiler can emit an object for a triple it cannot link, and
[Supported targets](/docs/deployment/supported-targets/) lists which triples
have been carried all the way to a running binary. Only those are called
supported.

WebAssembly needs more than a triple: a Worker or a browser has no filesystem
and no sockets, so `std` would need a platform surface shaped for the host.
What a wasm build gives you is a library module that runs Khora code and
imports nothing: one thread of control, no cancellation, no `Fiber::spawn`, and
`print` and trap messages that go nowhere because the module has no output to
write them to. Cloudflare Workers in particular is **not** an experimental
target — the Worker-facing pieces do not exist, and
[that page](/docs/deployment/cloudflare/) says what they would be.

## Stability

Khora has not reached 1.0. Source compatibility across arbitrary development revisions is not promised. Pin the toolchain version for applications where reproducible builds matter, and review migration notes when deliberately moving between incompatible releases.

[Compatibility and stability](/docs/reference/compatibility/) is the policy: what a `0.x` release promises, what counts as a breaking change, and the four things 1.0 is waiting for.

## Reporting a limitation

If the documentation says something should work and the compiler disagrees, treat that as a bug in either the implementation or the docs.

Every hand-written example on this site is compiled as a step of the project's build gate, and the generated API pages are checked against their declarations by `khora doc --check`. Two gaps remain. A hand-written fragment is *parsed* rather than type-checked unless it declares its own `module`, so an example can be syntactically valid and still mean the wrong thing — `List<String` with the bracket missing is a valid comparison. And the examples inside `///` doc comments, which become the generated API pages, follow the same rule: one of them declares a `module` and is type-checked in CI, and the rest are parsed. None are executed, so an example that compiles can still print something other than what it says it prints. Where an example and the compiler disagree, reconcile them against the implementation rather than assuming either side is right.
