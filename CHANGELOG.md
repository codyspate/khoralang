# Changelog

What changed, and what it breaks. Before 1.0 the language may break; it may not
break quietly, which is what this file is for. `/docs/reference/compatibility`
has the policy.

Entries are grouped by what a reader needs to know first: **Breaking** is what
may stop your program compiling or change what it does, **Fixed** is a wrong
answer that is now right, then the rest. A bug that produced a *silently wrong*
answer is listed under Breaking as well as Fixed, because code written around
it will behave differently now.

## Unreleased

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
  corrected. `std::net::http` answers **174,360 req/s**, p50 156us, p99 543us,
  peak RSS 576 KB, which is just under Go's `net/http` and a third under
  Kestrel where this repository had claimed 10x Go and 6x Kestrel.

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
