# Permissions

The decision for D4: what in `[permissions]` is actually enforced, and when.

> **The manifest decides what capabilities a program may hold. The capability
> decides what may be done with it.** The first is compile-time and total. The
> second is run-time, written in Khora, and enforced at the point of use.

**It is not a sandbox, and should not be described as one yet.** The gate is
total over Khora code — every requirement row in the program, transitively —
and it has a hole underneath it: `extern fn` reaches the operating system
without a capability row to be gated on. Closing it needs an allow-list on
which *packages* may declare one, and packages do not exist until phase 10. So
what the manifest gives you today is an honest account of what a program's own
code can reach, and no defence against a dependency that goes around it. The
whole argument is at the end of this file, under "The hole this does not close yet"; it
is repeated here because a reader should not have to reach the end to find out.

## Why the line falls there

`project.md` §4.1 says "OS-level capability limits enforced at compile time"
and gives:

```toml
[permissions]
network = ["allow-net=0.0.0.0:8080", "allow-net=db.internal:5432"]
```

Two different claims are tangled together in that line, and only one of them is
decidable.

**Which capabilities exist** is a question about types. Khora already answers
it: a function that touches the network says `with { net: Net }`, its callers
say so too, and the chain ends at the `main` that constructs one. Whole-program
monomorphization already computes every requirement row in the program, so
"does this program need `Net` anywhere" is a scan of something the compiler has
in its hand. It is decidable, total, and — because it follows types rather than
call sites — it holds **transitively through dependencies**.

**Which host it may connect to** is a question about values. `connect("db:5432")`
is checkable; `connect(config.host)` is not, and never will be. Any answer that
claims otherwise is claiming to solve halting.

Pretending both are compile-time gets the worst of it: a guarantee that
evaporates behind one `let`, and two enforcement mechanisms to keep in step. So
they are separated, and each is put where it can actually be kept.

## What is enforced at compile time

A program may not require a capability the manifest does not grant. The error
names the capability, the function that needed it, and what to add:

```
error: this program needs the `fs` capability, which khora.toml does not grant
  --> src/main.kh:12:3
   |
12 |   with { fs: Fs::real() } {
   |          ^^
   = add `fs = ["*"]` under [permissions], or narrow it to the paths you need
```

The mapping from a manifest key to a capability type is a short, fixed table —
short because the manifest names *kinds of access to the outside world*, and
there are not many:

| Manifest key | Capability type |
| --- | --- |
| `fs` | `std::fs::Fs` |
| `network` | `std::net::Net` |
| `env` | `std::env::Env` |
| `process` | `std::process::Process` |
| `clock` | `std::time::Clock` |

A capability the table does not mention — one an application defines for
itself, like `Ledger` — is not governed by the manifest at all. It is not
access to the outside world; it is a seam the program chose to have, and
nothing outside the program has an opinion about it.

## What is enforced at run time

The grants for a category are handed to that capability's real implementation,
which checks them where the access happens:

```khora
with { fs: Fs::real() } { .. }
```

`Fs::real()` is given the manifest's paths at build time — as data compiled
into the program — and refuses a read outside them, raising `IoError` like any
other failure. It is ordinary Khora in a module you can read, not a second
enforcement engine hidden in the compiler.

This is Deno's model, and it is Deno's model for the same reason: the check has
to be where the value is.

## Wildcards, and no barrier to entry

**A missing `[permissions]` table grants everything.** A program that has never
heard of permissions compiles and runs. This is opt-in tightening, not a tax on
starting — the same bargain Rust makes with `unsafe`, which is allowed until
you write `#![forbid(unsafe_code)]`.

**Each category is independent.** Mentioning `network` says nothing about `fs`;
an unmentioned category is unrestricted. A rule that silently locked down
everything the moment you named one thing would punish the first step towards
being careful, which is exactly the wrong incentive.

**`"*"` is how you say "yes, all of it" out loud** — worth writing when you want
the reader of the manifest to know the question was asked and answered.

### Three matchers, not one

A path and a hostname do not have the same structure, and one rule bent to
cover both is surprising in whichever direction it was bent. So there are
three, and each is the reading that costs a newcomer least.

**Paths.** `*` matches within one segment; `**` crosses them. The `.gitignore`
dialect everyone already has in their fingers. Separators are normalized, so a
grant written with `/` covers a Windows path and nobody writes the manifest
twice.

**Hosts.** `*` **spans dots**, so `*.internal` covers `db.eu.internal` and not
only `db.internal`. That is what a Content-Security-Policy origin means by it;
the one-label reading belongs to TLS certificates, and surprising somebody into
a *denied connection* is a worse failure than covering a subdomain they did not
think to enumerate. **A grant with no port covers every port** —
`api.example.com` grants the host, the same thing `--allow-net=example.com`
does in Deno — and `*` alone therefore covers everything.

**Names**, for environment variables. `*` matches any run: a variable name has
no structure to respect, and `DB_*` is the shape nearly every grant takes.

```toml
[permissions]
# Any host. The capability is still required in every signature that uses it —
# this only says the manifest is not narrowing it further.
network = ["*"]

# Or narrow it: an exact origin, any subdomain on one port, any port on one
# host, and a host at any port.
network = ["api.example.com:443", "*.internal:5432", "localhost:*", "cdn.example.com"]

# Environment variables by prefix.
env = ["DB_*", "PORT"]

# Paths, split by what is being done to them, because read and write are not
# the same grant.
[permissions.fs]
read = ["/etc/myapp/**", "./data/*.json"]
write = ["./tmp/**"]
```

For the strict posture, one line flips the default:

```toml
[permissions]
default = "deny"
network = ["api.example.com:443"]
```

Now an unmentioned category grants nothing, and adding a capability is a
deliberate edit. That is the setting a security-conscious team turns on once,
in CI, and forgets — and it is *their* choice rather than everyone's.

### A departure from `project.md`

The spec writes grants as `"allow-net=db.internal:5432"`. That is a Deno
*command-line flag* transcribed into TOML, where the `allow-` prefix and the
`=` are doing work a table already does. `network = ["db.internal:5432"]` says
the same thing, and `[permissions.fs] read = [..] write = [..]` says a thing
the flag form could only say by encoding a second key inside a string.

`project.md`'s claim that the limits are "enforced at compile time" is also
narrowed here, to the half that can be. That is the whole of D4.

## The hole this does not close yet

None of it is worth much while a dependency can write:

```khora
extern fn fopen(path: Ptr, mode: Ptr) -> Ptr;
```

A foreign declaration's effect row is a promise the compiler takes on trust
(`ffi.md` §3). A package that simply declines to make the promise reaches the
operating system with nothing in its signature and nothing in yours.

The answer is an allow-list on `extern` itself:

```toml
[permissions]
extern = ["std"]
```

Only listed packages may declare a foreign function. Then nothing reaches
outside Khora except through the standard library, whose functions all carry
capability rows, and the compile-time gate above becomes a real guarantee
rather than a convention.

**It cannot be implemented yet**, because it is a rule about *which package* a
declaration is in and packages do not exist — there is one source root and no
package identity until phase 10. So it is written down here, and the roadmap
carries it as part of 10.2 rather than pretending the gate is airtight in the
meantime.

Two smaller things also wait for packages:

- A dependency's own `[permissions]` becomes a **declaration** — what it says it
  needs — that the resolver can show you before you install it. The root
  package's manifest stays the only **grant**.
- Whether a dependency may be granted more than the root has is a question with
  an obvious answer (no) and no way to ask it today.

## What exists today

`khora-manifest` parses the table above and answers both questions:
`Permissions::grants(category)` for the compile-time half, and
`granted_path`, `granted_host`, `granted_name` for the run-time half. The
matching rules have tests; `crates/khora-manifest/tests/permissions.rs`.

**Nothing enforces them yet**, and that is on purpose rather than unfinished.
The compile-time gate needs the compiler to read a manifest at all — it does
not today, and `khora build` does not even take enough paths to build the
reference application. The run-time half needs `Net` and `Env` to exist. Both
arrive with phase 8, and the rules being decided and tested first is what stops
them being decided twice.

## Not decided here

- **What a denied access does at run time.** `Fs::real()` raising `IoError` is
  the obvious answer for the file system and probably wrong for a clock. Each
  capability's real implementation decides, and each is a small enough question
  to answer when the capability is written.
- **Whether the compile-time gate is a hard error or a lint.** A hard error is
  the assumption above. `[lints] unused-capabilities` already exists in the
  manifest, so there is a precedent for the other shape, and the two could
  coexist: denied is an error, *unnecessary* is a lint.
