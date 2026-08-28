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

| Manifest key | Capability type | State |
| --- | --- | --- |
| `fs` | `std::fs::FsRead`, `std::fs::FsWrite` | **enforced** |
| `network` | `std::net::HttpClient` | parsed, not consulted |
| `env` | `std::env::Env` | parsed, not consulted |
| `extern` | — | **enforced**, at build time |

**`process` and `clock` are not keys.** This table listed them for a long time
and the manifest has never had them: writing `[permissions] process = [".."]`
gets *unrecognized key `permissions.process`* and is otherwise ignored, which
is the worst of both -- a reader believes a restriction is in place and
nothing is. Named here as absent rather than quietly dropped, because the
table having claimed them is the reason somebody would look.

Whether they should exist is a separate question. `process` has an obvious
shape, `granted_name` over command names. `clock` has none: a grant is a list
of things, and there is nothing to list about knowing the time -- it would be
a boolean, which is a different feature than the rest of this table.

**`fs` maps to two types, and the pair is the point.** `[permissions.fs]`
already had `read` and `write` as separate grants, and a single `Fs`
capability made the finer half unexpressible: a package allowed only to read
still handed every function it called the authority to delete. The manifest
key stays one, because "reaching the file system" is one kind of access to the
outside world; which half a given function needs is a question its signature
answers.

A capability the table does not mention — one an application defines for
itself, like `Ledger` — is not governed by the manifest at all. It is not
access to the outside world; it is a seam the program chose to have, and
nothing outside the program has an opinion about it.

## What is enforced at run time

**Built, for `fs`.** The grants for a category are handed to that capability's
real implementation, which checks them where the access happens:

```khora
with { reads: FsRead::real(), writes: FsWrite::real() } { .. }
```

The manifest's paths are compiled into the program as data, and a path outside
them is refused, raising `IoError` like any other failure. It is ordinary Khora
in a module you can read, not a second enforcement engine hidden in the
compiler.

### How the paths get there

`khora build` writes `std::permissions::grants` — a module with two functions
returning `List<String>` — from `[permissions.fs]`, replacing the copy in
`std/grants.kh`. That checked-in copy grants `**`, which is what a program with
no `[permissions]` table compiles with, so **the default lives in a file
somebody can read** rather than in a branch of the compiler.

A whole file is replaced rather than a function body edited, because the
matcher beside it is real code and a compiler that rewrites part of a source
file it does not otherwise understand breaks the next time somebody reformats
it.

`std::permissions::granted` is the matcher, in Khora. It answers the same
question as `granted_path` in `khora-manifest`, which is what the compiler uses
to check a manifest against `[workspace.policy]` — so the two have to agree,
and both are tested against the same table of cases. Where they disagreed, the
compiler's reading won: `data/**` covers what is inside `data` and not `data`
itself, which is what the same pattern means in `.gitignore`.

### `IoError::Denied`

A refusal is its own case, not `Failed`. The two want different things from
whoever reads the message: `Failed` is a disk, a permission bit, a name that
was there a moment ago, and `Denied` is `khora.toml` — the fix is a line in a
file the reader owns. Folding them together would send somebody to look at
their file system for a decision their own manifest made.

### What is not checked

**A test double is not subject to the manifest**, and should not be: the grant
is about what the program may do to the machine it runs on, and a double
touches no machine. The check lives inside `FsRead::real` and `FsWrite::real`
rather than in their callers, which is what makes that true without anybody
having to remember it.

`rename` is checked at **both** ends. Checking only the source would let a
granted directory be emptied into an ungranted one, which is the whole of what
the grant was meant to stop.

`network` and `env` are parsed and matched and not yet consulted by their
capabilities. `fs` is the pattern they should follow, with one thing to settle
first for `env`: `Env::variable` returns `Option<String>` and has no error
channel, so a denial has nowhere to go that is not `None` -- which would mean
"not set", the exact conflation `IoError::Denied` exists to avoid.

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

## The hole this closes, as of 10.2

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

**It is implemented.** It could not be until 10.2, because it is a rule about
*which package* a declaration is in and there were no packages — one source
root and no package identity. There are packages now, so:

- `Permissions::may_declare_extern` answers the question, and `std` always may.
  That is the design rather than an exception to it: the point of the list is
  that everything reaching outside Khora goes through functions whose
  signatures carry capability rows, and those live in `std`. A `std` that could
  not declare `fopen` could not offer `Fs`.
- The check runs in `khora-cli` after resolution, because package identity
  exists in the resolver and nowhere else — the type checker sees a flat set of
  files. It reads the declarations out of the syntax tree rather than
  type-checking a package the build may be about to refuse.
- An absent key still grants every package, like everything else in this table.
  `extern = []` is the interesting value: nothing but `std` may reach out.

The refusal names the function, the file and the package, and prints the line
to add — because the alternative is somebody guessing at TOML.

`crates/khora-cli/tests/permissions.rs` holds it, including the two cases that
say the hole is no *wider* than documented: `std` is never refused, and a
project that has never thought about this is not punished for it.

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

## A workspace caps its members

Everything above is a package speaking for itself. In a monorepo the root can
speak for all of them, and that is a different table:

```toml
[workspace.policy]
network = ["ledger_service", "link_shortener", "risk_analyzer"]
env = ["ledger_service", "link_shortener", "risk_analyzer"]
fs = ["link_shortener", "risk_analyzer"]
extern = []
```

**A cap, not a default.** `[workspace.permissions]` is a table a member opts
into with `workspace = true`; `[workspace.policy]` is one a member cannot opt
out of. A fourth example deciding to reach the network is a build failure
rather than something a reviewer has to notice:

```
khora: bench/floor/khora.toml: `permissions.network`: `floor` is not allowed
to grant `network`. The workspace at .../khora.toml caps it to
ledger_service, link_shortener, risk_analyzer. Add `floor` to
`[workspace.policy] network` if it should be, or drop the grant
```

Four decisions inside that, each of which could have gone the other way:

**It caps which member may ask, not what it may ask for.** `network =
["gateway"]` says only `gateway` may have a `[permissions] network` entry at
all; it says nothing about which hosts. Capping the *values* — "no member may
reach anything outside `*.internal`" — is a real feature and a different one:
it needs a rule for what "narrower" means for a glob, and getting that subtly
wrong produces a cap that looks enforced and is not.

**Names, not the directory paths `members` uses.** A path in a policy stops
matching the day somebody moves a directory, and stops matching *silently* —
which for a cap means it quietly stops capping. For the same reason, a name
matching no member is refused rather than ignored: a typo in a cap fails open,
so it has to be loud.

**An absent category caps nothing.** The same "tightening is opt-in" rule the
per-package table follows. `[]` is the interesting value: nobody at all, which
is what this repository says about `extern`.

**An empty grant is not a request.** `network = []` in a member is a package
that has thought about the network and decided on none, and refusing that would
refuse a manifest for being careful.

Enforced in `Manifest::parse_at`, which every command that reads a manifest off
disk goes through — a cap that held for `khora build` and not for `khora check`
would be a convention with extra steps. The one exception is
`Manifest::load_for_resolution`, which reads a *sibling* member because one
lockfile needs every member's `[dependencies]`; a member's standing under the
policy is not a fact about the lockfile, and reporting it there would report a
violation in one member while somebody was building another.

No other monorepo tool can do this, because no other language has the grants in
the manifest and enforced by the compiler. Roadmap 14.19.

## Not decided here

- **What a denied access does at run time.** `Fs::real()` raising `IoError` is
  the obvious answer for the file system and probably wrong for a clock. Each
  capability's real implementation decides, and each is a small enough question
  to answer when the capability is written.
- **Whether a policy can narrow values and not only askers.** The section
  above says why it does not today. A root that could say "any host under
  `*.internal`" and have members narrow further is the shape a platform team
  will eventually want, and it needs a definition of "narrower" that is
  checkable rather than plausible.
- **Whether the compile-time gate is a hard error or a lint.** A hard error is
  the assumption above. `[lints] unused-capabilities` already exists in the
  manifest, so there is a precedent for the other shape, and the two could
  coexist: denied is an error, *unnecessary* is a lint.
