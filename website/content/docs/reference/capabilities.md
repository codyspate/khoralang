---
title: Capabilities
sidebar:
  order: 11
---

A function that needs to touch the outside world says so in its signature, and
the caller has to hand it the means:

```khora
fn load_user(id: Id) -> User with { store: Store } raises StoreError
```

`with { store: Store }` is the requirement. Callers supply it with a `with`
block. Nothing reaches a database, a clock or the network without a caller
passing it in.

## Capability row on a declaration

```khora
fn load_user(id: Id) -> User
  with { store: Store }
  raises StoreError
{
  store.load(id)!
}
```

The row entry has a label and an effect type:

```text
store: Store
```

The label `store` is in scope in the function body.

The call mirrors the declaration. The function writes `with { store: Store }`
to say what it needs; the caller writes `with { store: memory_store }` to
supply it. Read the first as *this function requires a capability named `store`
implementing the `Store` effect*.

## Several capabilities

```khora
fn create_session(id: Id) -> Session
  with { store: Store, clock: Clock }
  raises StoreError
{
  let user = store.load(id)!;
  Session::new(user, clock.unix_seconds())
}
```

## Open capability row

```khora
{ clock: Clock | 'ef }
```

The named capability is required and `'ef` represents any additional row entries.

A function can preserve the open row:

```khora
fn run<A, 'ef>(body: () -> A with 'ef) -> A
  with 'ef
{
  body()
}
```

## Handler values

```khora
let fixed_clock = handler for Clock {
  unix_seconds: fn () => 1700000000,
  unix_millis: fn () => 1700000000000,
  monotonic_millis: fn () => 0,
  sleep: fn _millis => (),
};
```

A handler's type is the effect it implements.

## Installing the real thing

The handlers above are invented, because a page about syntax should not depend
on which effects `std` happens to ship. Real programs install the ones it does,
and each capability's own module provides them:

```khora
import std::clock::{Clock};
import std::env::{Env};
import std::fs::{FsRead, FsWrite};
import std::log::{Severity, Log};

pub fn main() -> Int {
  with {
    env: Env::real(),
    reads: FsRead::real(),
    writes: FsWrite::real(),
    clock: Clock::real(),
    log: Log::json(Severity::Info),
  } {
    work()
  }
}
```

**The label is chosen by the function you are calling, not by you.** A row entry
named `fs:` will not satisfy a function that declared `reads: FsRead`; the error
says so — ``needs `reads: FsRead`, which this function does not require`` — but
it is worth knowing before you meet it, because the labels are effectively part
of `std`'s public API:

| Capability | Label | From |
| --- | --- | --- |
| `Env` | `env` | [`std::env`](/docs/stdlib/api/env/) |
| `FsRead` | `reads` | [`std::fs`](/docs/stdlib/api/fs/) |
| `FsWrite` | `writes` | [`std::fs`](/docs/stdlib/api/fs/) |
| `Clock` | `clock` | [`std::clock`](/docs/stdlib/api/clock/) |
| `Random` | `random` | [`std::random`](/docs/stdlib/api/random/) |
| `SecureRandom` | `random` | [`std::crypto`](/docs/stdlib/api/crypto/) |
| `Log` | `log` | [`std::log`](/docs/stdlib/api/log/) |
| `Tracer` | `tracer` | [`std::trace`](/docs/stdlib/api/trace/) |
| `Db` | `db` | [`std::db`](/docs/stdlib/api/db/) |
| `HttpClient` | `client` | [`std::net::http`](/docs/stdlib/api/net/http/) |
| `Process` | `process` | [`std::process`](/docs/stdlib/api/process/) |
| `Nursery` | `nursery` | [`std::core`](/docs/stdlib/api/core/) |
| `Scope` | `scope` | [`std::core`](/docs/stdlib/api/core/) |

Every `real()` reaches the actual machine. A test installs a handler of its own
instead — see [Testing capabilities](/docs/cookbook/testing-capabilities/),
which is what the seam is for.

## Postfix `with`

Install handlers for one expression:

```khora
let user = load_user(id)! with {
  store: memory_store,
};
```

General form:

```text
Expr with { label: HandlerExpr, ... }
```

The installation is postfix, so it applies to the expression immediately before it.

## `with` block

Install handlers for a lexical region:

```khora
with {
  store: memory_store,
  clock: fixed_clock,
} {
  let user = load_user(id)!;
  create_session(user)
}
```

General form:

```text
with ContextRow Block
```

Handlers lexically enclose the operations they serve. That is a real
consequence of direct style rather than a syntactic detail: the operation runs
when the call is evaluated, inside the block, rather than being collected into
a description that something runs later.

A closure written inside the block is the one case worth being exact about. It
*captures* the capability, because a capability row is a block of bindings and a
capability is an ordinary binding, so the closure keeps the handler it was
written with even when it is called somewhere else -- including after the block
has ended. That is what makes a callback that logs cost nothing in the type of
whatever takes it. The handler stays alive as long as the closure does.

## Sequential bindings

Bindings inside a capability row are sequential. A later expression may use handlers introduced above it:

```khora
with {
  config: env_config(),
  scope: Scope::root(),
  db: postgres_db()!,
  store: sql_store(),
} {
  run_server()!
}
```

This allows service construction to remain flat rather than nesting one installation block per dependency.

## Named context declaration

```khora
pub context Production {
  config: env_config(),
  scope: Scope::root(),
  db: postgres_db()!,
  store: sql_store(),
}
```

General form:

```text
pub? context Name {
  label: Expr,
  ...
}
```

## Use a named context

Postfix:

```khora
load_user(id)! with Production
```

Block:

```khora
with Production {
  run_server()!
}
```

## Override named-context entries

```khora
load_user(id)! with Production {
  store: test_store,
}
```

or around a block:

```khora
with Production {
  store: test_store,
} {
  run_test_case()!
}
```

Entries written at the use site replace or extend the corresponding capability row
for that installation. This is what makes a named production context usable
from a test: install `Production` and override the one capability the test
wants to control.

## A capability as an ordinary parameter

A `with` row is not the only way in. An effect value is a value, so a function
can take one as a plain parameter and call its operations with the same method
syntax:

```khora
import std::clock::{Clock};
import std::core::{print};

/// The capability arrives as an argument.
fn stamp(clock: Clock, message: String) -> String {
  "${clock.unix_millis()}: ${message}"
}

/// The same thing said with a row.
fn stamped(message: String) -> String with { clock: Clock } {
  "${clock.unix_millis()}: ${message}"
}

pub fn main() {
  let clock = Clock::real();

  print(stamp(clock, "started"));
  print(stamped("started") with { clock: clock });
}
```

The two forms differ in who chooses. A row is *requested*: the caller decides
what to install, and every intermediate frame passes it along without naming
it. A parameter is *given*: the caller has a particular handler in hand and
hands that one over. Prefer the row — it is what makes an intermediate function
say nothing about a capability it only passes through.

**Reach for the parameter where a row cannot go.** The clearest case is a type
that has no capability row to put one in. `SharedFn<A, B, 'er>`, which
`Router::get` takes, is `(A) -> B raises 'er` and nothing else — there is no
`with` in it, so a handler needing a database or an HTTP client looks
impossible to write. It is not: the handler takes the capability as a
parameter, and the closure that is certified closes over it.

```khora
import std::core::{Result, SharedFn};
import std::net::http::{Answer, Call, HttpClient, Request, Response, Router};

/// The capability arrives as a parameter, because `SharedFn` has no row.
fn items(client: HttpClient, _request: Request) -> Response {
  match client.send(Call::get("http://inventory/items")) {
    Result::Ok(answer) => Response::text(answer.status, answer.body),
    Result::Err(_why) => Response::text(502, "inventory unreachable"),
  }
}

fn routes<'er>(client: HttpClient) -> Router<'er> {
  Router::new()
    |> Router::get("/items", SharedFn::of(fn request => items(client, request)))
}
```

A handler is written the same way whether the capability came from a row or an
argument, so a function can be moved from one to the other without its body
changing.

## Capability rows on function values

```khora
Request -> Response with { db: Db, clock: Clock }
```

Generic row:

```khora
A -> B with 'ef
```

Capability rows are part of function types, so higher-order functions can preserve requirements without a runtime service locator.

## Capabilities do not imply failure

```khora
fn choose_bucket() -> Int
  with { random: Random }
{
  random.in_range(0, 10)
}
```

A capability may be required by an operation that does not raise a recoverable failure. Conversely, a pure computation may use `raises` without any `with` requirement.

## Manifest permissions

A capability row says a function reaches outside the program. `[permissions]` in `khora.toml` says how far it may reach, and the standard library's real handlers enforce it. It limits a program you trust; it is not a sandbox for code you do not, and [what the permission table is, and what it is not](#what-the-permission-table-is-and-what-it-is-not) says where the line is:

```toml
[permissions]
network = ["api.example.com:443", "*.internal"]
env = ["PORT", "DATABASE_*"]

[permissions.fs]
read = ["./data", "./data/**"]
write = ["./data/out.txt"]
```

**Both `read` entries above are needed**, and the reason is the one surprise in
the glob dialect below: `./data/**` grants what is *inside* `data` and not `data` itself, so with
only that line a program can read every file in the directory and cannot list
it. `read_dir("data")` and `is_dir("data")` both raise `Denied` rather than
answering `false`, for the reason given under the probes below.

The grants are compiled into the binary rather than read at run time. A file the program consults for its own permissions is a file an attacker edits.

**A missing table grants everything, and each category is independent.** Naming `network` says nothing about `env`. Tightening is opt-in.

**`network` is a list of hosts the program may connect *out* to, and nothing
else.** Every way `std` dials out checks it before anything is looked up or
dialed: `HttpClient::send` once the URL is parsed, `std::net::socket`'s
`connect_to` and `connect_by`, and `std::net::tls`'s `connect` and
`connect_by`. Binding a port is the other direction and is not covered:
`Router::listen`, `Router::listen_quietly`, `Router::listen_tls` and
`std::net::socket::listen_on` never consult it, so a program with
`default = "deny"` and no hosts granted still serves on any port it asks
for. Write `network = ["127.0.0.1:8787"]` expecting it to authorize a listener
and it will do nothing, in either direction. [Known
limitations](/docs/limitations/#inbound-connections-are-not-permissioned) has
the reasoning.

**A path grant is checked against where the path leads.** `std::fs` follows
the symbolic links on a path and matches the result as well as the path as
written, so `data/link.txt -> ../secret.txt` is refused under `./data/**` even
though its text is inside the grant. A link inside a granted tree that points
inside it works. Removing or renaming a link acts on the link, so only the
directories above it have to be inside the grant; a grant with no glob that
names the link itself covers removing it, wherever it points. Under a bare
`**`, which is what no `[permissions.fs]` table means, nothing is followed and
nothing is refused. Only `**` written exactly so is the whole file system:
`./**` is the working directory's tree, checked like `./data/**`.

Three things follow from checking where a path leads:

- **A relative grant is a tree under the working directory.** `**/*.txt`
  covers `.txt` files below the directory the program runs in, not an
  absolute path elsewhere that happens to end in `.txt`. A grant meant for an
  absolute location says so: `/srv/data/**`.
- **A grant that names a link follows it on every operation.** Under
  `./data/**` with `data` a link, the grant covers wherever `data` points at
  that moment. Whoever can replace that link moves the grant, for every
  operation until it is changed back.
- **A hard link is not followed, because it leads nowhere.** It is the file
  itself under a second name, so a hard link inside a granted tree is inside
  the tree, whatever other directory the same file also appears in. A grant
  limits which names a program may use, not which files those names are.

A path inside a grant whose links cannot be followed -- a loop, or a
directory the program has no permission to search -- is refused as
`IoError::Failed`, not `Denied`: the reason is the file system, and the fix is
not in the manifest. `FsRead::exists` and `FsRead::is_dir` answer `false` for
it instead. A link inside a grant that points at something missing outside
it is `Denied`, not `NotFound`: where it leads is outside, whether or not
anything is there yet. A trailing `/` changes nothing: `data/` is `data`.

A denial is its own error case, separate from the ordinary failure, because the two send a reader to different files:

| Reaching for | Denied as | Where the fix is |
| --- | --- | --- |
| an environment variable | `EnvError::Denied(name)` | `[permissions] env` |
| a host over HTTP | `CallError::Denied(host)` | `[permissions] network` |
| a host over TLS | `TlsError::Denied(host)` | `[permissions] network` |
| a host over a raw socket | `SocketError::Denied(host)` | `[permissions] network` |
| a path on disk | `IoError::Denied(path)` | `[permissions.fs]` |
| a program | `ProcessError::Denied(program)` | `[permissions] process` |

`std::net::socket`'s `connect_to` and `connect_by` raise `SocketError`, so a
refused host cannot be mistaken for a connection: `Denied` for a host the
manifest does not grant, `Unreachable` for one that could not be reached, and
`TimedOut` for a `connect_by` whose deadline passed. The handle they answer is
always a connected socket.

**Including the two probes.** `FsRead::exists` and `FsRead::is_dir` answer a
`Bool` and raise `IoError` as well, so a path the manifest denies is `Denied`
rather than `false`. They answered a plain `Bool` until recently, which made
three situations one word — not there, there but unreadable, and there and
readable and simply not granted — and the third is the one somebody debugs for
an hour, because nothing in a `false` points at a manifest. The remaining
`false` means one thing: the operating system would not open it.

`Unreachable` is DNS or a firewall; `Denied` is a line you can copy out of the message into the manifest. `Env::variable` and `std::env::variable_or` therefore `raise EnvError`, so both need a `!` at the call site:

```khora
let port = variable_or("PORT", "8080")!;
```

Globbing differs by category, and each one is the reading that costs the least surprise. For a path, `*` stops at a separator and `**` crosses one — and, as in `.gitignore`, **neither covers the directory being described**, which is why the `[permissions.fs]` example above names `./data` as well as `./data/**`. `\` is a separator on Windows only; elsewhere it is an ordinary character of a file name, so `data\x` is not inside `data`. For a name — a variable, a command — there are no segments, so `*` spans everything. For a host, `*` spans dots, so `*.internal` covers `db.eu.internal`, and a grant with no port covers every port. An IPv6 address is written in brackets, as in a URL: `[::1]:5432`, `[::1]:*`, or `[::1]` for every port. A grant ending in `:` with no port after it, such as `example.com:`, is refused when the manifest loads. A host handed to a connect may carry brackets only as `[address]`.

See [Effects and rows](./effects/) for effect declarations and [Failures](./failures/) for typed failure.

## What the permission table is, and what it is not

**The permission table is a program limiting itself. It is not a sandbox for
untrusted code.** It stops a program you trust from doing more than you meant
it to: a typo'd path, a library that opens a socket you did not expect, a
request path joined under a directory and walked out of it. It does not stop
code that is trying to get out:

- **Grants apply to the whole program.** `[permissions]` is read from the
  manifest of the package being built, and what it grants, every line of code
  in the program holds, the dependencies included. There is no per-package
  grant.
- **A dependency's own `[permissions]` table is ignored.** Only the root
  package's table is compiled in. A library cannot narrow itself, and cannot
  widen the program it is part of.
- **Any code can make a real capability.** `FsRead::real()`, `Env::real()`,
  `Process::real()` and the rest are ordinary functions, so a function with no
  `with` row can make one and use it; a signature without `FsRead` is not proof
  that the function reads no files. What stops it is the manifest, checked at
  run time inside the real handler: a capability made that way is refused
  exactly what any other would be.
- **`extern fn` is the door out.** A foreign function reaches the operating
  system without any capability, so nothing the table says applies to it. The
  `extern` key is what governs it: it names the packages that may declare one,
  `std` always may, and under `default = "deny"` a table with no `extern` key
  allows `std` alone, so a program that wants its own `extern fn` lists its
  own package. `khora check` refuses an unlisted declaration and names the line
  to add.
- **The checks are made by the program itself, in-process.** A link swapped
  between the check and the open, a library that writes over memory, or a
  native library linked in, is not something the table can see.
- **A grant is about names, not files.** A hard link inside a granted tree
  reaches the file it names wherever else that file lives, and a grant that
  names a symbolic link covers whatever the link points at when it is used.

**For code you do not trust, use an operating-system boundary**: run it as a
separate process under its own user, in a container, or in a virtual machine,
and give that boundary the files and the network the code may have. A Wasm
plugin target, where a plugin runs isolated inside the host process with only
what the host grants it, is planned and not yet available.