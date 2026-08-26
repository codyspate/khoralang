# Khora

A statically-typed, pure-functional systems language that compiles to native
static executables — no VM, no tracing GC. Memory is managed by Perceus
reference counting; effects, capabilities and typed failure channels are
tracked in the type system by row polymorphism.

This repository is the compiler, written in Rust.

## Status

**Khora runs real programs.** `khora build` takes a package through parsing,
name resolution, Hindley-Milner inference with row unification, exhaustiveness
checking, whole-program monomorphization, reference-count planning and LLVM to a
linked native executable. The standard library is written in Khora, and one of
the reference applications is an HTTP service.

Two profiles: `khora build` is unoptimized with debug information, and
`khora build --release` runs LLVM's `default<O2>`, drops debug information, and
is bit-for-bit reproducible — object and executable both.
`docs/design/profiles.md`.

It is **pre-1.0 and pre-release.** There is no registry, no editions, and no
stability promise yet — `docs/design/compatibility.md` decides what the
promise will be and names what 1.0 is still waiting on.

**Platforms.** Developed on Windows, and green on Windows, Linux and macOS —
the whole suite and the baseline, including a Khora HTTP server binding a real
socket and answering `curl`. `.github/workflows/ci.yml` runs all three, **on
demand rather than on every push**: this repository is private, macOS minutes
bill at ten times wall clock, and one full run costs around two hours of
billed time. `sh scripts/baseline.sh` is the gate; run the workflow before a
release or a change to the runtime or code generation.

Two tests keep it that way from a laptop, and the reason they exist is worth a
sentence. `khora-types/tests/portability.rs` type-checks `std` for every target
from any host, and `khora-codegen-llvm/tests/portability.rs` *generates and
verifies a module* for every target. The second was added after a bug that only
existed in a combination of modules Windows never compiles together: `std/fs`
declares `close`, so do the Linux and macOS socket files, and
`socket_windows.kh` calls it `closesocket`. Every POSIX build was broken and no
Windows developer could see it.

### What the language has

Algebraic data types, records and tuples, generics with higher-kinded types and
const generics, traits with `derive(Eq, Ord, Show, Hash, ToJson, FromJson)`,
closures, string interpolation, pattern matching with exhaustiveness and
reachability, irrefutable destructuring in a `let`, `while`/`loop`/`for`,
direct-style algebraic effects (`with` and `raises` rows, handlers, `!` at
fallible call sites), structured concurrency with nurseries, and a sharing
discipline that decides what may cross into another fiber.

`std` covers collections, strings, JSON, files, processes, randomness, time,
HTTP and **TLS** — a server that serves HTTPS, and a client that connects by
name and verifies the certificate against the machine's own trust store.

`std::net::http` is deliberately layered, so its `Router` is replaceable rather
than merely optional: the codec (`parse`, `Response`, `matches`) and the
connection layer (`Connection`, which does the reading, `Content-Length`
framing and keep-alive) are public, and `Router` is written against them with
nothing reserved. Nothing below the accept loop spawns, so a framework can be
synchronous, pooled, or fiber-per-connection as it chooses.
`crates/khora-codegen-llvm/tests/http_layers.rs` is a second framework of a
different shape, kept in the suite so the layering stays true.

### What it does not have yet

- **No chunked transfer, multipart bodies, or request bodies over 8 KB** in
  `std::net::http`, and a body must be UTF-8 text. TLS is there, both ends,
  and `Connection::holding` takes a larger cap than the default.
- **No HTTP/2 and no WebSockets**, so no upgrade path.
- **`[permissions]` is still not a sandbox**, though the largest hole in it is
  closed. `[permissions] extern` now decides which packages may declare a
  foreign function, so nothing reaches outside Khora except through signatures
  carrying capability rows. What it does *not* do is confine a program at run
  time: the compile-time gate is a gate, not a jail.
- **No registry.** A dependency names a git repository and a revision.
  `khora.lock` pins both the commit and the SHA-256 of what arrived, and the
  content-addressed store means two packages with identical contents are one
  directory — but there is nothing to `khora publish` to, and no version
  solving, because every source names one exact thing.
- **The language server answers two questions.** Diagnostics and hover.
  Completion, rename and capability inlay hints are not written.
- **The linter has two lints**, both deliberately narrow: an expression that
  cannot do anything and is discarded, and a capability a body cannot be
  using. Each declines the interesting cases rather than guess.

### Pinning a compiler

A project says which Khora builds it, in the same file as everything else:

```toml
[toolchain]
version = "0.1.0"
```

`khora` hands over to that version before it parses an argument, so a project
pinning a release with flags your build has never heard of still works. A
version that is not installed **stops the build** rather than falling back —
quietly using a different compiler than the one the project asked for is worse
than no pin at all, because it looks like it worked.

```sh
khora toolchain list                      # what is installed, and what is running
khora toolchain link 0.2.0 ./target/debug/khora
khora toolchain which                     # what this directory would use, and why
```

There is no `install`: nothing exists to download from yet. `link` registers a
build you already have, which is what two checkouts of this repository need.

### For an AI coding agent

No model has training data for Khora, so an agent writing it guesses from Rust
and is wrong in ways it cannot see. `khora mcp` is a Model Context Protocol
server that lets it ask the compiler instead:

```json
{
  "mcpServers": {
    "khora": { "command": "khora", "args": ["mcp"] }
  }
}
```

The tool that matters is `khora_check` — it compiles a snippet against the real
standard library and returns the real diagnostics, so an agent can find out
whether something works rather than guessing. `khora_std_search`,
`khora_grammar`, `khora_design_doc` and `khora_format` exist so its first guess
is worth checking. It runs through the toolchain shim, so a pinned project gets
that version's compiler answering.

### Two things worth knowing about the implementation

**A fiber is an OS thread today.** `Fiber::spawn` starts a real thread;
nurseries bound how many run at once. The design intends stackful coroutines
later and `docs/design/fibers.md` states the property that makes the swap legal
— a program cannot tell which it got — but *today* the answer is threads, and a
fiber costs what a thread costs.

**Compilation is whole-program.** Generics are monomorphized with no dictionary
passing, so a generic function does not exist as code until something calls it
at a type. One consequence is decided and permanent: **Khora has no stable
binary interface.** A package will ship source; the only stable ABI is C's, at
the `extern` boundary.

### Where the crates stand

| Crate | State |
| --- | --- |
| `khora-syntax` | Lexer, lossless CST parser, typed AST, error recovery. |
| `khora-db` | Salsa database, `SourceFile`/`SourceRoot` inputs, the `parse` query. |
| `khora-manifest` | `khora.toml` parsing; unknown keys warn rather than abort. |
| `khora-fmt` | Canonical formatter over the CST. |
| `khora-hir` | Module graph, item collection, name resolution, body lowering, `derive` expansion. |
| `khora-types` | HM inference, row unification, traits and HKT, exhaustiveness, monomorphization. |
| `khora-perceus` | `dup`/`drop` placement, last-use ownership, and reuse-token planning. |
| `khora-rt` | Reference-counted heap, fibers, sockets, TLS, intrinsics. Linked into every executable. |
| `khora-codegen-llvm` | LLVM backend behind the `llvm` feature. |
| `khora-pkg` | Resolution, `khora.lock`, the content-addressed store, the task DAG. |
| `khora-lint` | Lints that need types. |
| `khora-doc` | The API reference, read out of the syntax tree's `///` and `//!` comments. |
| `khora-lsp` | A language server over the same queries. |
| `khora-toolchain` | Several Khora versions on one machine, and which one a project wants. |
| `khora-mcp` | A Model Context Protocol server, so an agent can ask the compiler. |
| `khora-cli` | `check`, `fmt`, `doc`, `install`, `lex`, `parse`, `lsp`, `mcp`, `toolchain`, `test`, `bench`, and `build` with `--features llvm`. |

1,376 Rust tests and 18 Khora ones pass, `clippy -D warnings` is clean, and
`khora check` and `khora fmt --check` pass over all of `std/`, `examples/`,
`bench/` and `packages/` — with no lint warnings anywhere in them.
`khora doc --check` holds the generated standard library reference to the
source it was generated from.
`sh scripts/baseline.sh` runs the lot, including twelve HTTP conformance checks
against a real `curl`.

It leaves a receipt naming the tree it passed for, and `sh scripts/gate.sh`
asks whether one exists for the tree as it stands. `sh scripts/install-hooks.sh`
puts that question in a pre-push hook. An exit status can be dropped by the
shell chain around a script — a `| grep` taking grep's status has put three
commits on a red baseline — and a file on disk cannot be.

## Numbers

One measurement, so it can be argued with. A `/health` route on
`std::net::http`, answering **538,000 requests a second** — 48 reused
connections, 16-core Windows desktop, load generator on the same machine, five
second runs, median of three, spread 535k to 566k.

**Read the comparisons only against each other.** In the same sitting, a Khora
server stripped to accept-read-write-close does 781k and the identical loop
written straight in Rust does 560k — both near what the load generator can
drive, so the honest reading is that the runtime is not what limits anything,
not that Khora beats Rust. That Rust control measured 653k in an earlier
sitting on the same machine, which is well outside the eight per cent the runs
vary by and is the machine rather than the program. Absolute figures do not
travel between sittings; ratios within one do.

Phase 9 is where the parser's own numbers come from: an 80-byte HTTP request
parse went from **2,440ns to 1,555ns**, and a browser's fourteen-header request
from 14,560ns to 7,345ns.

Anything quoting a number from this project should name its workload and its
machine, because that is the only part of a benchmark that travels. `bench/`
holds the servers and the load generator, so the figures above are reproducible
rather than reported.

## Quickstart

**The front end needs nothing but Rust.** Parsing, type checking, the effect
rows, the formatter and most of the test suite build with a plain `cargo test`.

```bash
cargo test --workspace
```

**Compiling to a binary needs LLVM 22.1.8**, which the pin in `inkwell` and
`llvm-sys` makes exact. One script gets it, on any of the three platforms — a
bottled `brew install llvm@22` on macOS and Linux, the official tarball plus two
workarounds on Windows. `docs/llvm-setup.md` has the why.

```bash
sh scripts/setup-llvm.sh
cargo test --workspace --features llvm
```

The script also writes `.cargo/config.toml` from the committed template. That
file is not in the repository because both settings in it — where LLVM lives,
and which Windows SDK to link against — differ per machine.

```bash
cargo build -p khora-rt && cargo run -p khora-cli --features llvm -- build examples/core_demo
```

```bash
cargo run -p khora-cli -- check std examples
cargo run -p khora-cli -- fmt std examples --check
cargo run -p khora-cli -- parse examples/risk_analyzer/src/main.kh --no-trivia
```

## Layout

```
crates/
  khora-syntax/        logos lexer, rowan CST parser, typed AST
  khora-db/            salsa database, source inputs, the parse query
  khora-manifest/      khora.toml parsing
  khora-fmt/           the canonical formatter
  khora-hir/           AST -> HIR lowering, name resolution, derive expansion
  khora-types/         HM inference, row unification, traits, monomorphization
  khora-perceus/       reference counting
  khora-codegen-llvm/  inkwell backend, lld linking
  khora-cli/           the `khora` driver
docs/
  vision.md            what Khora is for; breaks ties in the roadmap
  positioning.md       who it is for, and what it is not
  roadmap.md           decisions, open questions, phases
  design/              nineteen decision records
  project.md           the original specification this was built against
  grammar.ebnf         the implemented grammar
  errata.md            where the specification was wrong, and what was done
std/                   the standard library, written in Khora
examples/              three reference applications
bench/                 four servers and a load generator; see bench/README.md
scripts/
  setup-llvm.sh        installs LLVM 22.1.8 and writes .cargo/config.toml
  check.sh             the fast loop: front end in ~30s, `native` for the rest
  baseline.sh          everything that must keep working, ~2m
  gate.sh              whether the baseline passed for the tree as it stands
  tree-id.sh           one line naming that tree; the receipt's content
  install-hooks.sh     puts gate.sh in a pre-push hook, opt-in
  http_conformance.sh  what an ordinary client gets, checked with curl
  check-linux.sh       the runtime's tests on Linux, through WSL2
  tsan.sh              the runtime under ThreadSanitizer; see soundness.md
.github/workflows/
  ci.yml               the three-platform matrix
```

`docs/errata.md` is the most useful file for understanding why the compiler is
shaped the way it is. It is not a changelog — it is the list of things that were
believed and turned out to be false, each with what the mistake cost and the
rule it produced.

## Reference applications

- **`core_demo`** — the language's own features, exercised.
- **`risk_analyzer`** — capabilities, handlers and typed failure, the shape the
  effect system was designed for.
- **`link_shortener`** — an HTTP service with shared mutable state, JSON,
  persistence and a clock.

Each is evidence that the pieces compose. **None is a claim of production
completeness**, and the list above of what the language does not have applies to
all three.

## Front-end design notes

**The parser never fails.** It always returns a tree spanning the entire input
plus a list of diagnostics. Whitespace and comments are tokens in that tree, so
`parse(src).syntax().text() == src` holds for any input — including binary
garbage. This is a hard requirement for a future language server and is
enforced by tests.

**Events, not direct tree building.** The parser emits a flat event stream that
is replayed into a `rowan` green tree afterwards. That indirection is what makes
`CompletedMarker::precede` possible: a finished node can be given a new parent
retroactively, which is how left-associative operators are parsed without
backtracking.

**Two syntactic ambiguities the specification leaves open** are resolved as
follows, and both are called out in `docs/errata.md`:

- `{` opens a record literal when followed by `}` or `Ident :`, and a block
  otherwise. In a `match` scrutinee it always opens the arm list.
- `a.b.c` in expression position stays an unresolved `FIELD_EXPR` chain — but
  only for `.`. The specification's "universal dot" gave a module path, an enum
  constructor and a record projection the same spelling; Khora splits them,
  using `::` for compile-time paths and `.` for runtime projection, so
  `a::b::c` is a `PATH` the parser builds outright and only `.` is left to name
  resolution. See `docs/errata.md` item 13 and
  `docs/design/associated-items.md`.

Operator precedence, loosest to tightest: `=` (right-associative, so
`x = a |> b` assigns the whole pipeline), `|>`, `||`, `&&`, comparisons,
`+ -`, `* / %`, prefix `- !`, then call and field access.

## What is next

Phase 9 is done — reuse analysis and FBIP, so `map` over a uniquely-owned list
allocates nothing, and an HTTP request parse went from 2,440ns to 1,555.

Phase 9.5 closes the gaps a stranger hits in their first afternoon: tuples and
irrefutable `let` destructuring, string interpolation, a one-command LLVM
install, and macOS. All four are written; the last waits on a green CI run for
a Mac to have actually executed it.

Phase 10 is largely done: `khora.lock` and a content-addressed store, the
`extern` allow-list, two lints, and a language server that reports diagnostics
and answers hover. What is left of it is a registry, cross-compilation targets,
and the sandboxed WASM build plugins — plus the rest of the editor surface.
Then phase 11, the scheduler — a fiber is an operating-system thread today, so a server holds
thousands of connections and not hundreds of thousands, and that is the last
thing standing between the positioning and the truth.

`docs/roadmap.md` has the order, the reasons, and what each one costs.

## Licence

MIT or Apache-2.0, at your option — `LICENSE-MIT` and `LICENSE-APACHE`. The
Rust ecosystem's default pair, and what `Cargo.toml` has declared since the
first commit.

Unless you say otherwise, a contribution you deliberately submit for inclusion
is licensed the same way, with no additional terms.

## Security

`SECURITY.md`. Report through GitHub's private vulnerability reporting rather
than a public issue; there is no address to email.
