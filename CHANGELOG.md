# Changelog

What changed, and what it breaks. Before 1.0 the language may break; it may not
break quietly, which is what this file is for. `/docs/reference/compatibility`
has the policy.

Entries are grouped by what a reader needs to know first: **Breaking** is what
may stop your program compiling or change what it does, **Fixed** is a wrong
answer that is now right, then the rest. A bug that produced a *silently wrong*
answer is listed under Breaking as well as Fixed, because code written around
it will behave differently now.

## 0.2.0 — 2026-09-04

An editor release, and one manifest change. Everything else here is about what
Khora tells you while you are writing it, and nothing changes what a program
means.

### Breaking

- **`[toolchain] version` is now required, and `edition` is gone.** Both halves
  of one change: two fields were answering the question *which Khora builds
  this?* and only one of them could.

  `edition = "2026"` named a year rather than a compiler. Nothing read it — it
  was inert by design, waiting for an editions mechanism that does not exist —
  so an unknown value had nothing to go wrong with, which is why it took until
  `0.1.0` for anybody to notice that `edition = "1999"` built without a word.
  It is removed. A manifest that still carries the line gets a warning naming
  what replaced it, and builds.

  `[toolchain] version` names a compiler that exists and selects the binary that
  runs: the build, the tests, `khora fmt` and the editor's language server all
  follow it. Requiring it is what makes "this project builds the same way on
  your machine" true by default rather than by convention. A project without one
  now stops, with the two lines to add in the message — the table name, the key
  and the quoting are not things anybody should have to guess at.

  Two consequences worth knowing:

  - **In a workspace the pin belongs at the root.** The search walks up from
    wherever you are to the nearest manifest that has one, and it no longer
    stops at the first manifest it finds — which it did, so a member with no
    `[toolchain]` reported that the project pinned nothing while the root above
    it pinned something.
  - **The machine default now only governs directories that are not projects.**
    Inside a project the pin answers, because there is always a pin.

  `khora new` writes the pin, and does not write one into a member of a
  workspace that already has it. Run it against an existing project and the
  message tells you what to add.

- **A pin may be `latest` or `latest.rc`.** The newest toolchain installed on
  this machine, release candidates excluded or included.

  **They are deliberately not reproducible**, which is the opposite of what a
  pin is for, and they exist anyway: the project that most wants to build
  against whatever compiler was installed this morning is the one developing the
  compiler. Write a version for anything you want built the same way twice.

  Both resolve against installed toolchains and never over the network — a
  channel that asked GitHub for the newest release would put an HTTP request in
  front of every `khora` invocation, including the ones an editor makes on every
  keystroke.

  This needed the version ordering fixed first. `Version` derived `Ord`, which
  compares `pre: Option<String>` with `None` below `Some` — so `0.2.0` sorted
  *below* `0.2.0-rc.1`, and "the newest stable release" would have chosen a
  release candidate. It now implements the precedence rules from the
  specification, including that `rc.2 < rc.10`.

- **Two rules moved from the code generator into the type checker, so
  `khora check` now refuses programs it used to accept.** Raising a type no
  `type` declaration names — `raises String` — and an integer literal wider
  than an `Int`. Neither has ever *built*: the code generator refused both, and
  the only thing that changed is when you are told.

  It is listed here rather than under Fixed because of one workflow: a project
  whose CI runs `khora check` and not `khora build` will see a failure it did
  not see before. That project was already broken; it just had no way to know.
  It is also why this is `0.2.0` and not `0.1.1`: before 1.0 the minor is
  where a breaking change goes, and a new refusal is breaking to somebody even
  when every program it refuses was already wrong.

  The reason it matters beyond the two rules is that `khora-lsp` publishes what
  the parser, the checker and the lints say and nothing from lowering — so a
  rule enforced during code generation is one **no editor can show**, at any
  point, on any platform. `scripts/check-backend-rules.sh` is a new gate that
  makes every one of the 52 remaining backend refusals a decision somebody has
  recorded, so a third does not arrive unnoticed.

### Fixed

- **An unreadable token is reported where it is.** `0xFF`, `0b1010` and `1 @ 2`
  all used to come back as ``this `{` is never closed``, naming a brace on the
  line above — an underline your editor drew on code that was correct. Numeric
  bases Khora does not have are now named as such, and say what it does have:
  decimal digits, with `_` free to separate them.
- **An unreachable `match` arm names its cause.** `Red => 1` where
  `Colour::Red` was meant reported the *next* arm — the one written correctly —
  as unreachable. A bare name in a pattern is a binding, so it had matched
  every colour. The message now says so and gives the line to write.
- **A declaration keyword from another language is redirected.** `struct`,
  `class`, `enum`, `interface`, `func`, `var`, `namespace` and `async` name the
  Khora spelling instead of *expected a declaration*. `enum` shows the variant
  syntax, because being told only "write `type`" leaves you looking for a
  separate word for cases; `async` is told the distinction does not exist here
  rather than that it is a rename.

### Editor

- **Sixty-two new assists**, bringing the total offered under the cursor from
  two to sixty-four. They are in ten groups: control flow, bindings,
  capabilities and failures, matching, patterns, imports, declarations, types,
  calls and pipelines, statements, literals and documentation.

  The ones worth finding first:

  - **Extract into a function**, on an expression or a block. The `with` and
    `raises` clauses are written from what the calls inside actually demanded —
    the checker records that at every call site, so the signature is correct by
    construction rather than inferred a second time by an editor.
  - **Write out the cases a `_` arm covers.** A wildcard is how a `match` stops
    being exhaustive without stopping compiling: add a variant, and every
    `match` that names its cases fails loudly while every `match` ending in `_`
    sends the new one down the default path in silence.
  - **Lift a lambda into a function**, with the parameter and answer types the
    checker gave it.
  - `catch` and `attempt` in both directions, a `with` in both spellings, and a
    handler written from an effect declaration.

  Every editor gets all of them. There is one implementation and it is
  `khora lsp`; `editors/` is configuration.

## Unreleased

### Changed

- **A span knows what it is inside, and `std::log` correlates by itself.**
  `std::trace::current` is the span the running fiber is in: `around` installs
  one for the duration of a body and restores the enclosing one on every path
  out, a handler's `start` takes the trace id, the sampling decision and the
  parent from it, and a fiber inherits its spawner's span when it is created,
  so work that fans out stays one trace. Until now `Span::parent` was `0`
  everywhere and a nested `around` began a second trace.

  **This changes what a `Log::json` line looks like.** A line logged inside a
  span now carries `"trace_id"` and `"span_id"` after `"message"`; a line
  logged outside every span is unchanged, because the fields are absent rather
  than zero. Anything parsing those lines by position rather than by key will
  need to look again; anything indexing them by key gains two fields it
  probably already expects.

  A handler written outside `std` keeps working and keeps its old behaviour —
  starting a fresh trace per span — until its `start` calls `current`.
  `packages/otlp` does now.

### Added

- **`std::trace::current`**, `Context::trace_id` and `Context::span_id`. The
  last two render the ids as OpenTelemetry writes them, thirty-two and sixteen
  lower-case hex digits, which is the form a collector's search box takes.

### Documentation

- **`khoralang.com/docs/` serves `v0.1`, the documentation for the released
  compiler**, with `next` beside it for the one being written. `/docs/` used to
  redirect to `next`, which after `v0.1.0` published meant the front door sent
  every reader to pages marked as describing a compiler they could not install.
  A released tree is cut from its tag and records which one; a section is per
  release allowed to break, so the major from 1.0 and the minor before it.

## 0.1.0 — 2026-09-03

### Breaking

- **`khora doc` no longer defaults to this repository's layout, and no longer
  claims the directory it writes to.** `paths` defaulted to `std` and `--out`
  to `website/content/docs/stdlib/api`, both resolved against the caller's
  working directory, so the command documented nothing a user owned and wrote
  into a path that meant nothing to them -- and its stale sweep deleted every
  markdown file there it had not just generated, which is data loss for a
  directory that is not a generated tree. The defaults are package-relative
  now: the nearest `khora.toml` decides, sources come from its `src`, pages go
  to its `docs/api`, and outside a package the command refuses and says what to
  type. The sweep is scoped by a `.khora-doc` record written beside the pages,
  so a page whose module was deleted still goes and a file the command did not
  write is left alone and reported. Projects that passed both arguments
  explicitly, this repository included, are unaffected.
- **`std::ai::Extract` is deleted; `extract` asks for `Decode`.** A type that
  derives `Decode` is extracted with nothing else written: its shape,
  rendered as a JSON Schema by the new `Shape::to_json_schema`, is what the
  model is told to produce, and its decoder reads the answer back.
  `ModelError::SchemaExtractionError` carries `List<Rejection>` -- every
  problem with what the model said -- rather than a `String`. The
  associated type `Spec`, `spec`, `described` and `parse` go with the trait.
- **`Row` carries its column names.** `std::db::Row` is
  `{ columns: List<String>, cells: List<Cell> }`; every `Db` handler and test
  double that builds a row supplies them, and `packages/postgres` passes on
  the names it had been dropping. `Row::to_raw` is the row as a source hands
  it over, keyed by column, and `Row::sequence` is every row at once, so
  `list(Entry::schema()).decode(Row::sequence(rows))` reads a query's answer
  through the same schema that reads a request body and reports a bad row
  as `[2].amount should be a whole number` rather than dropping it.
  `Row::named` reads one cell by column.
- **`std::config` reads a schema.** `string`, `int`, `decimal`, `bool`,
  `secret`, `or_default` and `ConfigError` are deleted; the module is
  `read(schema) -> Validated<A, Rejection> with { env: Env }`, `variables(shape)`
  for the names a deployment needs, and `report(problems)` with each path
  spelled as the variable it came from. The shape decides the names: a
  nested record's field is `LISTEN_PORT`, a list is split on commas, a
  variant is `MODE` with its payload beside it as `MODE_URL`, and `key`
  renames one segment. A denied variable is `Problem::Denied` and still
  names the `[permissions] env` line to add. A record written once with
  `derive(Decode)` now reads from the environment, a request body and a
  test fixture alike.
- **`std::json` no longer decodes; `std::schema` does.** `FromJson`,
  `ToJson`, `DecodeError`, `decode`, `field_as` and the `variant_*` helpers
  are deleted, and `derive(ToJson, FromJson)` is refused. The replacements
  are `derive(Decode, Encode)`, `Raw::of_json` in and `Raw::to_json` out; a
  decode reports every problem as a `Rejection` rather than the first as a
  `DecodeError`. `parse`, `encode`, `Json` and its accessors, `Field`,
  `member` and `object` are unchanged. The variant wire format changes with
  the derive: a payload-free case is a bare string and a payload case is an
  object tagged with `type` and keyed by its payload names, where
  `derive(ToJson)` wrote `{ "case": .., "fields": [..] }` for every case.
- **`Response::json` takes `A: Encode` rather than `A: Show`.** It encodes
  the body as JSON rather than printing it, so a derived record, a `Json`
  built by hand and a list of `Rejection`s all serve, and a record holding a
  `Redacted` cannot be sent. A pre-serialized `String` is now sent as a JSON
  string; build the response with `Response::text` and its content type, or
  `parse` the text first.
- **`struct2`..`struct5` are gone; a record schema is `struct({ .. })`.**
  `struct({ host: string(), port: between(int(), 1, 65535) })` is a
  `Schema<Listen>` when `Listen` is the record with those fields, decided
  from the type the expression is asked for -- an annotation, a parameter,
  the declared return type -- or from the labels alone, the way any record
  literal is. It is not a function that runs: its argument is a record of
  schemas and its result a schema of the record they decode, so a call to
  it is rewritten before it is typed into `Schema::record` over `Fields`,
  which is what the arity family had become. A call with anything but a
  record literal is refused and says what to write; a field whose schema
  decodes the wrong type is reported at that schema. There is no arity:
  `Fields::zip` nests a tuple, however many fields there are.
- **`std::schema`'s primitives are strict where the source could label the
  value.** `string()` no longer reads a `Raw::Number`, and `int()`, `float()`
  and `bool()` no longer read a `Raw::Text`: a JSON body with `"port": "8080"`
  is refused, as serde and `encoding/json` refuse it. Text a source could not
  label -- the environment, the command line, a query string -- arrives as the
  new `Raw::Untyped`, which every primitive reads, so `PORT=8080` still
  decodes. `decimal()` reads text as well as a number, because money travels
  as a string on most wires. `bool()` reads `true`, `false`, `1` and `0` and
  no longer `yes` or `no`.
- **`Raw` has three new arms, `Null`, `Untyped` and `Denied`**, so every
  exhaustive match over it gains three. A JSON `null` in a required field now
  reads `rate should be a number, and is null` rather than `rate is not set`;
  `optional` treats both as `None`, and `nullable` is the field that must be
  present and may be `null`.
- **A rejection quotes text in double quotes and writes a number bare.**
  `port should be a whole number, and is "8080"` says the value arrived as
  text; `and is 8080` says it did not. Before, both were written in
  backticks and could not be told apart.
- **`many` is `list`**, on the rule that a constructor is named after the type
  it answers, and `Shape`'s arms follow every constructor one for one:
  `Many` is `List`, `Maybe` is `Optional`, and the new arms are `Any`,
  `Float`, `Dict`, `Cases`, `Nullable`, `Default`, `Keyed`, `Closed`,
  `Described` and `Lazy`. `Shape::Refined` carries a structured `Rule` rather
  than a sentence, and `Problem::Refused` carries the same. `Lazy` holds a
  thunk, so `Shape` no longer derives `Show`; its `Show` is written by hand.
- **`Problem` has two new arms.** `Unexpected` is a key a closed record did
  not declare, produced only by `Schema::closed`; `Denied` is a value the
  source was not granted, kept apart from `Missing` because the fix is in
  `khora.toml`.
- **`Rejection::where_` is `Rejection::at`.** `where` is not a reserved word in
  Khora — not a hard keyword, not a contextual one, not a token the lexer knows
  — so the underscore was avoiding a collision that does not exist. It is `at`
  rather than `where` because `where` is reserved in Rust, Haskell, SQL and C#
  for roughly the clause Khora might one day want for trait bounds, and a name
  that needs decorating to be legal is a name to replace.

- **Every `std::schema` constructor is named after the type it answers.**
  `text` is `string`, `whole` is `int`, `exact` is `decimal`, `truth` is
  `bool`; `secret`, `optional`, `refine` and `struct2`..`struct5` are
  unchanged (`many` became `list` afterwards, above). `Shape`'s arms follow the constructors (`String`, `Int`,
  `Decimal`, `Bool`, and `Struct` without its trailing underscore) and `Raw`'s
  follow `std::json`'s (`Text`, `Number`, `Bool`). `std::config`'s `integer`
  and `boolean` are `int` and `bool` for the same reason, so `std` no longer
  holds three vocabularies for four concepts. Rejection messages are unchanged:
  one still reads `listen.port must be a whole number`, because a person
  reading a failure wants a sentence and a person writing a schema wants a
  type.

- **`Db` has a sixth operation, `broken`.** Every handler must implement it —
  `handler for Db { .. }` without it is a compile error naming the gap. It is
  called when a `ROLLBACK` fails, which leaves a connection that may still hold
  a transaction open, and the handler is the only thing that can act on that.
  `packages/postgres` closes the connection; a test handler can count it.

- **A `loop` with no `break` has type `Never`, not `()`.** It could not be the
  body of a function that returns something — `fn serve() -> Int { loop { .. } }`
  was a type error against a body that cannot return at all. A `loop` with a
  bare `break` is still `()`, and one whose `break`s carry a value is still
  their type. Widening a type is not usually breaking; this is listed because a
  program that relied on a `loop` being `()` in a value position now sees
  `Never` unify with whatever is around it.

- **A JSON number keeps its token's text.** `Json::Number` holds a `String`
  rather than a `Float`. `Json::number` still answers a `Float`, `Json::integer`
  is new and exact, and `Json::literal` hands over the text for
  `Decimal::of_string`. Build one with `Json::of_int` or `Json::of_float`.
  Encoding writes the literal back, so `parse("1e3")` re-encodes as `1e3`
  rather than `1000` — a round trip now preserves the document.
  *Silently wrong before:* `9007199254740993` decoded as `9007199254740992`,
  and `10.10` was never exactly recoverable.
- **A nursery raises `ChildFailed` when a child fails.** `nursery` and
  `bounded_nursery` are `raises 'er + ChildFailed`, so callers must handle or
  declare it — including `Router::listen` and `Router::listen_tls`. The first
  failure now cancels the siblings.
  *Silently wrong before:* an adopted child that raised was ignored, its
  siblings ran to completion, and the program exited 0.
- **`Fibers::wait` answers `Int`** — how many children ended with an error —
  rather than `()`.
- **Build output goes to `<package>/build/`**, named after the package, instead
  of landing beside the source. `khora build .` writes `build/hello.exe`, not
  `src/main.exe`. `khora test` and `khora bench` follow. A loose file outside a
  package still gets its executable beside it.
- **`undocumented-export` is `deny` in this repository's own manifest.** Not a
  language change; it is a `[lints]` default nobody else inherits.

### Fixed

- **A diagnostic that told you to make things worse.** A type that derives `Eq`
  but is not imported reported ``\`Money\` has no \`Eq\` impl ... Write
  \`impl Eq for Money\``, which would have a reader hand-write a duplicate impl
  for a type their module does not own. An evaluator hit exactly that and called
  it the worst message in the toolchain, in the tone the good ones had earned
  trust with. It now says the type is not in scope and to import it, keeping the
  old advice for the case where that really is the answer.

- **`khora test --filter` exited 0 when it matched nothing.** A typo in a filter
  is a CI step that tested nothing and went green. It now exits 1 when a filter
  matched none of the declared tests, and still exits 0 for a package that
  simply has none — the two print nearly the same sentence and mean opposite
  things.

- **`khora build` on a library said the wrong thing.** A `--lib` package has no
  `main`, and got `this program has no \`main\` function, so there is nothing to
  run` pointing at an arbitrary file. It now says the package has no program to
  build, that a library has none by design, and names `khora build --lib`.

- **Three concurrency claims the runtime does not honour** are recorded in
  `/docs/limitations/` rather than left in the reference as promises: a bounded
  nursery admits `limit + 1` children, because `Fiber::spawn` starts the child
  before `adopt` can block; a child's failure is noticed in adoption order, so
  cancelling the siblings can be arbitrarily late and how many are still running
  is a race; and the two fiber backends are distinguishable, since `clock.sleep`
  is interrupted by cancellation under `KHORA_FIBERS=scheduler` and not under
  threads. All three were found by measurement.

- **`reference/capabilities` never showed a real handler.** The page contained
  `real()` zero times: every example installed an invented one, so a reader
  finished the canonical page on capabilities knowing the syntax and unable to
  write `main`. It has an `Installing the real thing` section now, with the
  table of which label each `std` capability expects — labels are load-bearing
  and were documented nowhere.

- **Smaller documentation gaps found by the same exercise**: that `+` joins two
  `String`s and there is no `++`; `Fiber::wait`, which is what you need after
  `cancel` and was missing from the concurrency reference's own `impl` block;
  and a pointer to `khora std search` in Getting Started, which one evaluator
  never found and another used three times.


- **Windows was told to install something that does not provide a linker.**
  Khora links through `clang` -- deliberately, since a MinGW `gcc` emits a
  different ABI from the MSVC-targeting objects the backend produces -- but the
  error and `installation.md` both named the Visual Studio Build Tools'
  "Desktop development with C++" workload first, and that workload does not
  include one. Its "C++ Clang tools for Windows" component is separate. Two
  evaluators installed exactly what was asked for and still could not link; one
  extracted strings from `khora.exe` to work out what was really probed for.
  Both now name LLVM first and say what the workload does and does not bring.

- **`reference/capabilities` contradicted itself about `is_dir`.** One line said
  a denied path answers `false`; forty lines later the same page said it raises
  `Denied` and explained why it had changed. `FsRead::is_dir` checks the grant
  before it looks, so the second is true and the first is what it changed from.

- **The tooling list omitted `new`, `build` and `run`** -- the three commands a
  newcomer needs -- listing six of twenty-one. It also now says there is no
  `khora lint` and that the lints run inside `check`, which was discoverable
  only twenty entries into a reference page.

- **Two adjacent getting-started pages disagreed on the first line of every
  program.** `khora new` and one tutorial write `module <package>::main;`; the
  other wrote `module main;`. Both compile, so nothing was broken -- but a
  reader following the two in order met both spellings without being told they
  were the same thing.


- **Closing a connection waited for a peer that had nothing to say.** `shut`
  closes politely -- say there is nothing more coming, read off what is still
  arriving, then close -- because closing while the peer is still writing sends
  an RST and an RST discards the answer just written to it. The drain read with
  `receive`, which suspends until something arrives, and for a peer that is
  open and silent nothing ever does: the half-closed connection sat in
  `FIN_WAIT_2` until the kernel abandoned it, 120 seconds on Windows and
  `tcp_fin_timeout` on Linux.

  A receive deadline hid it wherever one had been set, which is why it lasted:
  the HTTP server sets ten seconds, so every connection it closed took ten
  seconds and nobody called that a hang. Anything using `std::net::socket`
  directly got the kernel's timeout instead. `khora_net_recv_now` is one `recv`
  with no retry and no suspension, `receive_now` exposes it on all three
  platforms, and the drain uses it -- 120.216s became 203.7ms. The conformance
  check for the case the drain exists for, a 9 KB header refused while the
  client is still sending the ninth, still passes. `docs/errata.md` 78.

- **A cancelled fiber left its socket open, and a server never gave its port
  back.** `std::net::socket` registered no release at all, so a socket was
  closed only by a normal return -- which is the one exit a server never takes.
  `Router::held_open` registers the listening socket's close with a region and
  `Router::served` registers the connection's, so both are closed by a raise
  and by a cancellation as well as by returning. `HttpClient`'s transport is
  registered the same way, in the region the call already ran in.

  The close moved rather than being added twice: `shut` on a TLS transport
  frees the session, so closing it a second time would be a double free.
  `net_cancel.rs` proves both ends -- a cancelled fiber's port binds again,
  which it could not while the first listener was open, and a peer sees the
  connection close.

- **An `https` client had no read deadline, so a quiet server held a fiber for
  ever.** The plain path has set one on the socket since the reactor did;
  `set_receive_timeout` takes a *socket* and a TLS session owns its socket
  rather than handing it back, so `https` had nothing. That is the shape of a
  slowloris, and it worked over `https` and not over `http`.
  `std::net::tls::set_receive_timeout` sets the deadline on the socket
  underneath, where the reactor's timer lives, and `dial` uses it for `https`
  exactly as it does for `http`.


- **Only the first import was ever consulted for a bare imported name.**
  `resolve_through_imports` used `?` where it meant `continue`, so a file that
  said `import std::core::{print};` before `import app::helper::{twice};`
  could not resolve `twice`: looking for it in `std::core` failed and ended
  the search. Every real file has more than one import, so this was the common
  case, and it was invisible because the tests each had one import.
  Go-to-definition, hover, references and rename were all affected.
- **Go-to-definition on a method landed nowhere.** `khora_hir` did not collect
  impl members at all, so `Int::to_string` -- the commonest shape of call in
  the language -- resolved to nothing and hovering one showed a type where its
  author had written a sentence. `ItemMap` records the functions inside `impl`
  blocks now, with their ranges, kept apart from `items` so two impls for one
  type still cannot collide. An inherent method is preferred to a trait one,
  because that is what a call resolves to.

- **A backtick string had no rule in the editor grammar, so its contents were
  highlighted as code.** Not merely uncolored: the template's keywords and
  numbers lit up, and a `"` inside one opened a string that ran on and
  mis-colored the rest of the file. `std/core.kh`, `std/json.kh` and
  `std/schema.kh` each contain one, so the standard library was the worst
  affected thing anybody was likely to read. Five more gaps went with it:
  decimal literals `1d` and `0.01d` (both numeric rules end in `\b`, which
  fails against the suffix), bare `<` and `>` (so every type bracket and every
  `a < b` was uncolored while `a <= b` was not), `${}` holes, the `..` record
  spread, and `///`, which had no scope of its own. Postfix `!` no longer
  reads as logical negation.
- **The VS Code extension declared a version of VS Code it cannot run on.**
  `engines.vscode` said `^1.75.0` while its only runtime dependency requires
  `^1.82.0`, so on anything in between it installed cleanly and then failed to
  activate. `khora.runTest` is also hidden from the command palette now: it is
  a code-lens callback, and run without its arguments it opened a terminal
  filtering on nothing.
- **The language server dropped the file-watcher notification the extension
  was already sending.** A `.kh` file created by `git checkout`, `khora new`
  or another editor never joined the source root, so every name it defined
  read as unresolved until somebody restarted the server.
  `workspace/didChangeWatchedFiles` is handled; a file open in the editor is
  left alone, because the buffer is the truth and the disk is behind it.
- **Diagnostics went stale in every file but the one being edited.** A build
  is whole-program, so deleting a function from one file breaks another, and
  the server published only for the file that changed. Every open document is
  republished now. Closing a file also clears its diagnostics, which it never
  did, so a closed file used to stay in the Problems panel for the session.
- **The agent-facing formatter ignored the project's settings.** `khora_format`
  over MCP used the defaults while `khora fmt` and the language server both
  read `[fmt]` from the manifest, so an agent editing a four-space project
  produced two-space output that the project's own `--check` then rejected. It
  takes an optional `path` naming the package.
- **Four documentation claims that were not true of the code.**
  `docs/project.md` advertised sub-15ms completion that has never been
  measured, workspace-wide semantic rename when rename is locals-only, match
  pattern stub generation that does not exist, and a `khora lint` command that
  was never built. The editor setup page called Emacs and Sublime support
  "ready-to-use" when both are untested snippets, and never said how to
  install the VS Code extension.

- **The first table from the new benchmark rig had every memory figure wrong.**
  `tasklist` on Windows prints a process's memory with a thousands separator,
  and both samplers took the text after the last comma -- reading 468 KB from a
  process using 4,468. Everything was understated by roughly ten times and the
  biggest servers most, so the JDK's 699 MB was published as 948 KB. The field
  separator is quote-comma-quote, which the number's own comma is not. The
  corrected column is the one Khora leads: 8.4 MB against Go's 21.8, Node's
  86.8, Kestrel's 240 and the JDK's 699. Errata 77.

- **Every throughput figure this project published was two to twelve times too
  high.** `bench/load.py` ran one process per connection, each timing itself,
  and divided the total by the duration it was *asked* for rather than the one
  it took -- and forty-eight Python processes on Windows take fifty-two seconds
  to get through a four-second run. The workers barely overlapped, each
  measured a nearly idle server, and the report was one connection's rate
  multiplied by the number of connections. A rig whose output is proportional
  to its own worker count cannot flatten, which is why no ceiling was ever
  found and why the spread between sittings looked like 1.85x. Errata 77.
  `bench/loadgen.rs` and `bench/measure.py` replace `load.py` and `compare.py`;
  every published number has been retaken; `README.md`, `/docs/performance/`,
  `/docs/limitations/`, `bench/README.md` and `docs/design/fibers.md` are
  corrected. `std::net::http` answers **174,201 req/s**, p50 161us, p99 554us,
  peak RSS **8.4 MB**, which is a little under Go's `net/http` and a third
  under Kestrel on rate where this repository had claimed 10x Go and 6x
  Kestrel -- and between three and eighty times less memory than any runtime
  it was measured against, which is the column nobody had instrumented and
  the one Khora is actually ahead on.

- **The traps reference told people to set the wrong environment variable,
  and called a failed assertion a trap.** `RUST_BACKTRACE` is honoured but
  `KHORA_BACKTRACE` is the name the runtime's own note gives, and the
  reference named only the first. A failed `assert` reports its line and lets
  the run continue: it prints no backtrace and does not end the process, so
  listing it beside a checked overflow described something the runtime does
  not do. Both pages say the same thing now, and the reference enumerates the
  operations that trap rather than gesturing at them.

- **Checking a package a few imports away from a large module no longer
  takes a minute.** The bodies an imported type reaches were appended to a
  file's view once per import that reached them, and a module exporting that
  view handed every copy to whoever imported from it, so each hop along an
  import chain multiplied the list. `packages/postgres`, three hops from
  `std::schema` once `std::db` imported it, took forty-six seconds to check;
  it takes one.
- **An impl written apart from its type arrives with its trait.** `impl
  Encode for Json` is written in `std::schema`, because `std::json` cannot
  import it. A file that imported `Json` and, from `std::schema`, only a
  type whose impl brought the trait `Encode` along was then refused
  `Response::json(200, Json::Object(..))` for `Json` not implementing a
  trait it implements: the bound had become strict against the one impl
  that had arrived. A trait that arrives with an imported type now brings
  the module's other impls of it, as importing the trait would have.
- **A function's declared return type reaches its body.** `fn f() -> U8 { 200 }`
  was refused, because the literal had settled on `Int` before anything
  mentioned `U8`, and a record literal in tail position had to be found by
  its labels when two records shared them even though the signature had
  already said which. The return type is now the hint for the body root, the
  way an annotation is for a `let`. A mismatch is therefore reported where it
  is: `fn f() -> Bool { id(1) }` says the argument `1` is not a `Bool` rather
  than that the body is not.
- **A `return` inside a closure returns from the closure.** Lowering and code
  generation always treated it so; the checker compared it with the enclosing
  function's return type, so `fn f() -> Int { let g = fn b => { if b { return
  "early"; }; "late" }; .. }` was refused for a disagreement with a signature
  the closure never answered to.
- **A function type written as a `let` annotation is checked.** It was echoed
  as opaque and became `Unknown`, so `let g: (Int) -> Int = fn x => "s";`
  checked clean and the annotation was a comment that was believed. It now
  reaches the closure as its expectation, and a closure that raises against an
  annotation with no `raises` clause is refused, as it is against a parameter
  written the same way. An annotation *with* a `with` or `raises` clause is
  still not echoed.
- **`Validated::zip`** is new: both values together as a pair, keeping both
  sides' failures. `zip(a, zip(b, c))` is three values in a nested tuple, and
  `let (x, (y, z)) = t;` takes it apart, however many there are.
- **A capability offered to a closure is no longer one it has to use.**
  `nursery(fn () => 1)` was refused with ``nursery: Nursery is required here but
  not provided`` — about a nursery that was being provided. Every parameter
  written `with { 'ef | cap: Cap }` behaved that way: a lambda's capability row
  came out closed, so it could not absorb a label nobody asked for. The error
  row beside it had been left open since it was written, for the same reason.
  Errata 69.
- **`unused-import` sees a name used inside a `${..}` hole.** The mention walk
  is over the token stream, and a hole's contents live inside one string token —
  so `"${quoted(c)}"` was a string to the lint and a call to everybody else.
  Following the advice deleted an import the program needed.

- **The documentation site had not built for a week.** `sync-docs.mjs` refuses
  a link written to a `.md` source file rather than to the route it renders as
  — a good check — but asked that before asking whether the link was external,
  so three links to `CONTRIBUTING.md` and friends on GitHub broke the build.
  `scripts/baseline.sh` now assembles the site, which is why nothing caught it.
  Errata 68.

- **A non-ASCII character beside a `${..}` hole panicked the compiler.**
  `print("café ${n}")` ended in a Rust panic from `split_interpolation`, which
  copied the text around a literal's holes one byte at a time. A literal with
  no hole never reaches that code, so plain non-ASCII strings were always fine
  and it needed both in one literal. Errata 67.

- **`khora check` and `khora build` disagreed about `src/bin/`.** The
  `misplaced-main` lint exempted it, the lint's own message recommended putting
  a second program there, and the backend compiled every `main` it found into
  one program and refused — so `check` passed on the layout the message
  suggested and `build` then failed with the error the message was trying to
  help with. All three now say the same true thing: a package builds one
  program, and a second is a package of its own. Multiple binaries per package
  is Roadmap #162.

- **A new project's first build no longer reports a key that moved.** The build
  cache is one directory for the whole machine, and a miss was classified by
  asking whether that directory was empty — so the first build of a *second*
  project opened with `the key moved. Nothing is stored under this one, and the
  cache holds 1751 other(s)`, in which nothing had moved, no input had changed,
  and the keys listed belonged to somebody else. The question is now asked about
  the target being built. A key that really did move still says so, and names
  what the target built under before; an emptied cache says the entry is gone
  rather than blaming an input. Errata 66.
- **A `numeric` column arrives as `Money`.** `postgres`'s decoder mapped
  `numeric` to `Text` because a value that failed to parse would have had to
  become a wrong number or a lost value. `Decimal::of_string` answers an
  `Option` and refuses a numeral too wide for the significand, so neither is
  needed: it parses or the server's own digits are kept. `float4` and `float8`
  stay `Text` — `Cell` has no float variant, for the same reason `std::json`
  stopped using one.

- **Two modules may each declare a type of the same name.** An impl was
  identified by its type's head as a bare string, so the second `Show#Entry`
  was dropped at the whole-program merge, the search returned the wrong one,
  and the call was emitted against the trait's bodyless method — a build
  failure reported against a blank line, in a program that had type checked.
  Errata 62.
- **A `catch` arm may bind the failure value**, which four documentation pages
  already said it could.
- **A `!` no longer takes a pending cancellation before its call runs**, so a
  value just received off a channel is not silently lost.
- **Cancelling a fiber parked on a channel wakes it.** It used to hang forever.
- **`khora test` no longer hangs after a trap in a test fiber.**
- **`Env::arguments()` survives a non-ASCII argument** instead of killing the
  process.
- **Draining a large channel no longer overflows the stack.**
- **`nursery.adopt` in a nested block, then another `adopt`,** is no longer a
  double free.
- **The list combinators carry the caller's rows**, so a raising closure can be
  passed to `List::map` — which the Guide had been showing all along.

### Added

- **`std::log`, and a way to write to standard error at all.** Khora had none:
  everything went to stdout, so a program's diagnostics and its answer shared a
  stream and the first `> out.txt` anybody typed swallowed the diagnostics. Two
  independent evaluators building ordinary command-line tools reported it.

  `eprint` is the primitive. `Log` is the capability over it, for the reason
  everything reaching outside is one: a function that logs writes to a stream
  somebody else owns, so it says so in its row — which also makes it testable,
  since a test installs a logger that collects into a list with no global to
  reset. Five levels, `Trace` through `Error`, filtered by the handler rather
  than the caller.

  The format is one JSON object per line, with `timestamp`, `level` and
  `message` in that fixed order and attributes as typed fields after them.
  Assembled rather than built as a `Json::Object`, because an object is a `Map`
  and a map has no order. `timestamp` is epoch milliseconds as a number —
  nanoseconds would be about 2^60 and JSON numbers are doubles in most readers,
  which would round it. `level` is lower case, which is what ECS, `slog`, `zap`
  and `structlog` all emit, so a collector's default filter matches without
  configuration. `Log::plain` is there for a terminal.

  `Log::json_using` takes the clock, so a test can fix it and compare the line
  byte for byte; `Log::json` uses the real one. Attributes are `std::trace`'s
  `Attribute`, so a line can carry `trace_id` and `span_id` in the field names
  OpenTelemetry expects — passed in, because `std::trace` still has no notion of
  a current span. `/docs/cookbook/logging/` covers all of it.


- **`khora std search <query>`, which was an agent-only facility.** `khora mcp`
  has exposed a searchable view of the standard library to coding agents since
  it existed, and a person had no equivalent: no subcommand, and a website
  built from `main` while the installable compiler was a release behind it.
  Four independent evaluators, given the published pages and a released
  toolchain, each concluded the compiler was broken rather than that the pages
  were ahead of it; the one who recovered did it by reaching for the agent's
  tool and said so.

  It answers out of the same index, which is the compiler's own item map over
  the `std` beside it -- signatures sliced from the declarations, descriptions
  taken from their `///` comments. Nothing to regenerate and nothing to keep in
  step: a function added to `std` is findable the next time it runs, and one
  that was removed stops being. Private items are left out, because writing
  code against one produces `not exported`, and that is a worse teacher than
  never having seen it.

- **`Router::listen_quietly`**, which is `listen` without the line it prints on
  standard output. `listen` announces `listening on <port>` as a readiness
  signal, which is what a supervisor or a test harness can read without being
  taught a protocol -- and is one unsolicited line in the wrong format for a
  server whose output is structured logs. `listen`'s own comment had predicted
  this; both now share one serving half, and `listen` documents that it prints.


- **`std::core::todo`, a hole that type-checks anywhere and refuses to run.**
  It is generic in its result, so it answers whatever is wanted: an unwritten
  `match` arm compiles beside a `String` arm and an `Int` arm alike and the
  rest of the program still builds. Generic rather than `-> Never` because a
  body that *is* a `Never` call has no value for the backend to return. Running one stops with
  `khora: this is not written yet` and status 134, the same as every other
  trap: a placeholder that returned a plausible value would let a half-written
  program answer, and a wrong answer is the one outcome worse than a refusal.
  The backend emits `unreachable` after any call whose type is `Never`, which
  is what lets a function whose whole body is one of these have a return type
  it never returns.
- **Six assists the diagnostics already half-wrote.** Each is offered only
  where the message names one edit and there is nothing to choose:
  - the call that needs its `!` gets it;
  - a `match` that is not exhaustive gets every missing case at once, qualified
    the way the arms already there are written -- a bare constructor name is a
    *binding*, so `Green => ..` would match everything and compile, which is
    the one outcome worse than an error -- with `todo()` for a body;
  - an unused import is removed together with one separating comma, since
    deleting the name the diagnostic covers and nothing else leaves
    `{List, , print}`;
  - an unused binding is renamed to `_name`. The message names a deletion too,
    and that one is not offered: it means deciding what to do with the
    initializer, which may be the call that does the work;
  - `cannot find `prnt`; did you mean `print`?` becomes the name it meant;
  - a record literal missing a field gets the field, with `todo()` in it, found
    by counting braces forward from the field the checker did read.
- **The trait members an impl has not written, written out.** `this impl is
  missing `cmp`` names the member; what it takes and what it answers are in a
  trait declaration in another file, spelled against `Self`. The action copies
  the declaration's own text, swaps `Self` for the type being implemented, and
  gives it a `todo()` body -- copied rather than rendered from a type, so what
  lands is what the trait author wrote rather than the checker's normalisation
  of it. Nothing is offered when any one of the named members cannot be found,
  because half an answer here leaves the reader working out which half.
- **Assists, which answer where the cursor is rather than what is wrong.** A
  quick fix is applied by somebody who read four words of a message they did
  not go looking for, and `fixes.rs` holds a hard rule about that; an assist is
  asked for, so it may restructure code. Two to begin with: writing an inferred
  `let` type down as text, which the inlay hint could only draw, and lifting a
  selected expression into a `let` above its statement. The extraction refuses
  where the walk from the selection to its statement crosses anything
  conditional -- an `if` branch, a `match` arm, a lambda body, the far side of
  `&&` -- because hoisting code out of those runs it when the program said not
  to.
- **Completion inside a `with { .. }` writes the whole handler.** A `with`
  block is where a capability stops being a requirement and becomes something
  the code supplies, and writing one by hand means naming the effect, then
  every operation it declares, then a closure of the right arity for each --
  all of it in a declaration usually in another file. Now the effect's name is
  enough: `Clock` inserts
  `clock: handler for Clock { now: fn () => todo() }`, arities read from the
  operations' own types.

  **The label comes from the requirement where there is one.** `std` installs
  `LLMService` as `ai`, which no rule derives from the type -- but the calls
  inside the block say what they still need, and an entry that answers a
  requirement has to be spelled the way the requirement is. An outstanding
  `ticker: Clock` is offered as `ticker: handler for Clock { .. }`, sorted
  ahead of everything else, because it is not one of several plausible entries
  but the one.

  A signature's `with` is told apart from an expression's and offers types
  rather than handlers -- the same three characters open both rows and want
  opposite things. Both are read from the token stream backwards rather than
  from the tree, because `with {` with nothing after it is a syntax error and
  the node that would say what the brace belongs to is exactly the node that
  does not exist yet.
- **Completion offers names the file has not imported, and writes the import.**
  A name had to be in scope before it could be completed, which is the wrong
  way round: the import is the thing you wanted the editor to write. Every
  public name in the workspace is offered now, shown with the module it comes
  from, and accepting one inserts the name and the `import` together --
  merged into an existing `import` of that module where there is one, and
  placed in sorted order among the others where there is not. They sort below
  everything already in scope, so a local named `rows` is not outranked by
  three hundred names from `std`.
- **The `///` on a completion arrives when the item is looked at.** The server
  answers `completionItem/resolve`, and the list itself now carries a name, a
  kind and the module. Reading the documentation of every public name in a
  workspace to fill a list where one of them gets read cost 100ms a keystroke
  against something the size of `std`, measured; on resolve it costs one lookup
  for the one item highlighted.
- **The server says which code action kinds it has.** `codeActionProvider` was
  a bare `true`, which tells a client nothing and gets the server asked for
  everything every time; it now names `quickfix`, `refactor.rewrite` and
  `refactor.extract`. A client filling one menu asks for that menu, and the
  assists it did not ask for are skipped rather than computed and discarded.
- **A lens for what a function absorbs**, in both halves. Khora's rows are
  transitive, so a lens repeating a signature would be noise -- except where a
  `catch` stops a failure reaching it, or a `with` block answers a requirement
  before it gets there. `installs { db } · catches DbError` above a function
  whose signature mentions neither is the line the type system deliberately
  hides. The capability half needed `CallRows::declared`: `requires` is what a
  call *still* owes, so inside a `with` block it is empty and the discharged
  capability was invisible at exactly the call that discharged it.

- **The server takes messages in batches, and honours `$/cancelRequest`.** It
  is still one thread doing one thing at a time; what changed is that it can
  see the queue before it starts. Typing ten characters used to be ten
  `didChange` notifications and so ten full type-checks, nine of whose answers
  were obsolete before they were computed. Every edit is still applied, since
  each is measured against the last, and the file is checked once at the end.
  A request the same batch cancels is answered with the protocol's
  `RequestCancelled` rather than computed, which a strictly serial loop can
  never do: the cancel always arrives after the work it wanted to stop.

- **Rename reaches the whole workspace.** It refused to leave a body until the
  two things that made it unsafe were answered: a declaration's range covers
  its whole body, so renaming through it would have replaced the body too, and
  an import list is not a `::` path so nothing looked at it. The name is
  narrowed out of the declaration, import lists are searched directly, and
  every edit is checked against the declaration's own spelling — so
  `import m::{foo as bar}` renames the `foo`, leaves the `bar` and its uses
  alone, and still renames a fully qualified `m::foo` in the same file. A
  trait member and a constructor are refused, each with a sentence saying why.
- **Go to the type, and go to the implementations.**
  `textDocument/typeDefinition` answers from the checked type rather than the
  path, so on `let mixed = Colour::make()` it lands on `Colour` where
  go-to-definition lands on `make`; a function type is followed to its result,
  since the callee's own type is `() -> Colour` and the question is about what
  it produces. `textDocument/implementation` lists every `impl` of a type or a
  trait, one result per block rather than one per method.
- **Folding and expand-selection.** A fold is offered for anything with a body
  worth collapsing and for a run of imports, which nothing gives you by
  accident: each import is its own declaration, so no node spans them. A
  region that starts and ends on one line is not offered, because an editor
  asked to draw one puts a chevron beside a line that cannot collapse.
  Expand-selection walks the ancestor chain, discarding steps that do not
  widen the selection so the key press never appears to do nothing.

- **Hovering a generic call shows what the type variables became.** A
  declaration says `A`; at the call site `A` is something in particular, and
  which one it is cannot be read off the declaration. The instantiated type
  leads and the generic form sits under it as *declared as*, which is the
  order TypeScript uses and for the same reason: the substitution is the part
  a reader cannot work out. Shown only for a generic declaration, since for
  anything else it repeats the signature in a second notation.

- **"did you mean" on an unresolved name.** An unresolved `prnt` now ends its
  message with *did you mean `print`?* when exactly one name in reach is close
  enough. Edit distance against the locals, this file's declarations and what
  it imported, with two guards: the distance has to be small relative to the
  name, so a two-letter name gets nothing, and a tie suggests nothing at all,
  because two equally close names is a menu rather than an answer.

- **`textDocument/documentHighlight`.** Every mention of the name under the
  cursor, in the file being read. The same search `references` runs, narrowed
  to one document, because an editor asks for this on every cursor move.

- **`KHORA_TIMINGS=1` says where a build's time went**, splitting it into
  check, monomorphize, lower, optimize, object and link. The split is the
  finding: for the largest program in the corpus, monomorphization is 8.0 of
  the 11.5 seconds a cold build takes, the object file 2.5 and the linker 0.5.
  `scripts/compiler-perf.py` reads those lines and measures cold build, warm
  rebuild, check-only time, peak compiler memory and how monomorphization
  scales; `--write-baseline` records them in `docs/compiler-perf-baseline.json`
  and `--check` fails when one has moved by more than 1.5x. Monomorphization is
  linear -- about 2.4 ms per instantiation over a 40x range -- on a fixed cost
  of three seconds that is `std` being compiled whole-program before the build
  reaches the program.

- **Provenance, a toolchain bill of materials, and release notes, published
  with the archives.** Every release archive is attested with
  `actions/attest-build-provenance`, so `gh attestation verify <file> --repo
  <repo>` says which workflow at which commit produced those bytes; there is no
  maintainer key to trust or to leak. `scripts/toolchain-sbom.py` renders the
  compiler's own dependencies as CycloneDX 1.5 from `cargo metadata --locked`,
  including the pinned LLVM and the Rust toolchain that built it, and it is
  attached as `khora-<version>.cdx.json` with a checksum. `khora sbom` already
  answered the same question for a Khora package. Release notes are cut from
  this file by `scripts/release-notes.sh` rather than written twice, and a
  version with no entry here stops the release instead of shipping a blank
  body.

- **A load generator that is not what it is measuring.** `bench/loadgen.rs` is
  a few threads each driving many non-blocking connections, instead of a thread
  or a process per connection. The change that mattered was not the language: a
  blocking read parks the thread and the kernel wakes it on every response,
  about 120 microseconds on a round trip whose median is 29, so the same
  connection spinning on a non-blocking socket answers five times as many
  requests. It reports latency percentiles from a probe connection competing
  with the load, samples the server's resident memory with `--watch-pid`, and
  prints the machine and the date beside the figure. `bench/measure.py` walks
  the ladder, repeats the rung, checks all four conditions from
  `/docs/performance/` and prints what failed instead of a number when one does
  not hold.

- **Server guidance for traps.** A trap in a request handler ends the server
  process, not the request: it does not unwind, so the `catch` the router
  wraps a handler in never sees it. That was documented only as one sentence
  in a section about C exports, while the code around a handler reads as
  though a request-level safety net existed. `/docs/reference/traps/` now has
  what it means for a service and what to do about it, the HTTP cookbook
  carries the short form, and `tests/traps_in_a_server.rs` holds the claim
  from both sides: a raise is a 500 and the server carries on, a trap in the
  next handler ends it with status 134.

- **A derived schema carries the type's `///`.** `derive(Decode)` reads the
  doc comment above the type and above each record field into
  `Schema::described`, so the JSON Schema a model is prompted with, and the
  one an API is documented by, say what the author already wrote next to
  the field. The comment is the description; there is nothing to write
  twice, and nothing to drift.
- **`Raw::of_arguments_for(shape, arguments)`** reads a command line the
  way a shape says: a flag whose field is a `Bool` is a switch and never
  takes the word after it, so `khq -c '.name' f.json` reads `c` as `true`
  and the query as the first argument. `-c` is a flag as well as `--c`.
  `Raw::of_arguments` is the shape-blind reading, kept for a program without
  a schema in hand. `khq` reads its flags through a schema now, which
  deleted its hand-written flag grammar.
- **`Shape::to_json_schema`** renders a schema's shape as a JSON Schema
  document, draft 2020-12: a derived type is a `$defs` entry and a `$ref`,
  which is what terminates a type that mentions itself; a rule is its
  keyword; a secret is `writeOnly`; an optional or defaulted field is left
  out of `required`; a variant is an `enum` or a `oneOf` over the two forms
  the decoder reads. What a model is prompted with, and what an API is
  documented by.
- **`derive(Decode)` and `derive(Encode)`.** The declaration is the schema:
  a record reads its fields under their names, a variant reads a bare string
  for a payload-free case and an object tagged with `type` for the rest, a
  newtype is transparent, a generic type bounds its parameters by the trait,
  and a type that mentions itself needs nothing written, because a derived
  schema is built when it is first read. A field whose type has no `Decode`
  is refused at the derive line, and so is a case whose payload has no field
  names, because the wire needs a key. `Encode` is the mirror, and a record
  holding a `Redacted` derives `Decode` and refuses `Encode`.
- **`std::schema` reaches a schema through the type.** `Decode` is the trait
  whose one function is `schema() -> Schema<Self>`; `std` implements it for
  its own types and a program implements it for a record or a variant with
  the constructors, and every schema that contains that type then finds the
  impl through the trait. `Settings::schema()` names the type; `decode(raw)`
  is chosen by the type the surrounding expression asks for. `Encode` is the
  other direction, `encode(self) -> Raw`, kept apart from the schema because
  a secret has no representation on the wire: `Redacted` implements `Decode`
  and not `Encode`. `Rejection` implements `Encode`, so a list of problems is
  a response body with a `path` and a `message` per problem. `Decimal` is
  written as text, which is how money travels on most wires, and `None` is an
  absent key.
- **Every hand-written documentation example is compiled by the gate.** All 580
  blocks in the Guide, the Reference and the Cookbook, which nothing had ever
  compiled — 55 of them did not. Most were legitimate shapes the checker had to
  learn (a bodyless signature, a list of types, the arms of a `match`, one entry
  of a handler); six were rot, including a `handler for Db` that went stale two
  commits earlier. `scripts/check-docs.sh` takes page arguments, so one page can
  be checked while it is being written.

- **A package's other programs go in `src/bin/`.** One file per program, each
  built with the package's modules and not with the others, each named after
  its file: `src/bin/backfill.kh` becomes `build/backfill.exe` beside
  `build/<package>.exe`. `khora build .` builds all of them; `khora run .` runs
  the package's own and names the others when there is no `src/main.kh`.

- **`examples/khq`**, a query language over JSON: about 3,600 lines across ten
  modules, with thirty-four tests. The largest Khora program written, and the
  first to put weight on the language rather than demonstrate a feature. What
  it found on the way is in errata 67 and Roadmap #164.
- **Every documentation page says which commit it was built from**, linked to
  that commit on GitHub, with the release beside it.
- **Short paths on the site**: `/install`, `/guide`, `/reference`, `/stdlib`,
  `/versioning`, `/limitations`, `/releases`, `/source`, `/security`,
  `/contributing`, `/changelog`.
- **`/docs/performance/`**, which publishes the benchmark methodology and no
  numbers — the load generator is currently the limit and the same
  configuration does not repeat to within 1.85×.

- **`Float::of_string`.** `Float` was the only primitive with no way in from
  text. It existed privately in `std::json`, whose comment said it belonged in
  `core` as soon as a second caller appeared; `examples/khq` is that caller.
  The shape is JSON's, so `+1`, `1.`, `1e`, `1 2` and `inf` are refused.
- **`String::chars_between`.** How many characters lie between two byte
  offsets — what a caret needs, and what neither `char_length` nor
  `byte_length` could answer.

- **The rollback-failure policy is tested, and its one gap is written down.**
  `std::db` discards a failed rollback on purpose: the caller is told
  `RolledBack` with the body's reason, because the engine's complaint about a
  rollback is a worse thing to report than the reason it was needed. On the
  cancellation path there is no caller to tell, so a connection can return to a
  pool having neither committed nor, as far as anything knows, rolled back.
  Named in `std::db`'s module documentation.
- **A pooled connection is proved to come back rolled back.** `with_db` opens a
  region for the lease and `transaction` opens one for the rollback inside it;
  the whole of the pool's correctness is that the inner finalizer runs first.
  Asserted now as an ordered transcript.

- **A `Char` type, written `'a'`.** One Unicode scalar value in thirty-two bits,
  with the string escapes and `Eq`, `Ord`, `Show`, `Hash`. `Char::code` and
  `Char::from_code` cross to and from `Int`; `from_code` stops the program on a
  surrogate or a number past `0x10FFFF`, because neither is a character.
- **A character-boundary string API**: `String::is_char_boundary`,
  `next_boundary`, `previous_boundary`, `char_at`, `chars` and `char_length`.
  `String::slice` counts bytes and stops the program when a cut lands inside a
  character; until now there was no way to *ask*, so a program truncating text
  it did not write was one non-ASCII input from dying.
- **`std::schema`**: a `Schema<A>` that describes a value and knows nothing
  about where the bytes came from, reporting every problem rather than the
  first. `std::config`'s types are its first client.
- **A misplaced-main lint.** A `main` outside `src/main.kh` or `src/bin/` is
  reported, which is the convention the toolchain assumes.
- **`Eq`, `Ord`, `Show` and `Hash` for the fixed-width integers** — 28 impls
  that were missing.
- **Auto-import quick fix** in the editor: the compiler already named the
  module, and now the editor can apply the import.
- **A toolchain indicator in the editor's status bar**, showing which compiler
  answered and why that one.
- **`khora --version` carries the commit and the target triple**, so a bug
  report names a compiler somebody else can find.
- **`CONTRIBUTING.md`, this file, and a public compatibility policy.**

### Changed

- **The published documentation stopped explaining itself to its authors.**
  About thirty passages of implementation history — what a function used to do,
  which attempt fixed it, what a previous version of the note said — are gone
  from the standard-library pages, along with sixty-six references to files
  inside the repository. A `docs/design/*.md` path that made a claim about what
  the document contains is now a link to GitHub; one that was the whole
  sentence is gone. Rationale for current behaviour stays, and so do migration
  notes that happen to be phrased as history. `scripts/no-maintainer-notes.sh`
  is a gate step so it does not come back.

- **The Guide is gone; the Language Reference absorbed it.** Fourteen of the
  Guide's fifteen pages were a second telling of a Reference page — the same
  constructs, one register apart — and keeping both in step with a compiler
  that is still moving was a standing cost that bought a reader nothing. Every
  `/docs/guide/*` path redirects to the page that took its material, the
  Reference's index opens with the reading order the Guide provided, and the
  Cookbook, which never overlapped either, is untouched. `khora test` and
  `khora bench` are now `/docs/reference/testing`, and packages, dependencies
  and the lockfile are `/docs/reference/modules-and-packages`.

- **`std::schema` and `std::decimal` have prose pages.** `/docs/stdlib/schema`
  says what a `Schema` is and why it is a decoder and an untyped `Shape` in one
  record, which the generated API page could show but not explain;
  `/docs/stdlib/decimal` carries the arithmetic-by-method rule, what scale does
  to a long chain, and why the significand is 128 bits. Both were reachable
  only through a generated signature list before.

- **A capability whose type is not imported says so.** Importing `nursery` and
  not `Nursery` gave ``Nursery has no method `adopt` ``, which is false twice
  over. The message now names the import to write. A misspelled method on a
  type that *is* imported is still reported as a misspelling.
- **A capability that shadows a function of the same name says which is
  which.** `fn f() -> () with { nursery: Nursery }` binds `nursery` in the
  body, so the body's `nursery(..)` calls the capability; the message was
  ``Nursery is not a function``, about a type the reader never wrote.

- **`/docs/reference/modules-and-packages` shows a dependency**, rather than
  describing one: `git`, `rev` and `subdir`, what `khora install` does that
  editing the manifest cannot, a real lockfile entry, that the checksum is
  verified rather than only recorded, and what `publish` means.

- **An impl's bounds are part of whether it applies.** Finding
  `impl<A: Show> Show for Result<A, E>` by its head said a `Result` *can* be
  shown; whether this one can depends on what is in it.
- **`khora new` scaffolds a two-line `.gitignore`** naming `build/`, replacing
  four patterns that named the files a build used to leave among the sources.
- **The trap message is a whole sentence**, written by whoever raises it,
  rather than assembled from fragments.

## 0.1.0-rc.3 — 2026-08-27

The third release candidate. No changelog was kept for the candidates; `git log`
between the tags is the record, and the entries above cover everything since.

## 0.1.0-rc.2 — 2026-08-26

## 0.1.0-rc.1 — 2026-08-26

The first packaged build: installers, checksums, and a release workflow that
compiles a program with the artifact before publishing it.
