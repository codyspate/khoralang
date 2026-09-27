# TechEmpower read tests: Khora against Go, Node and Bun

Four of the TechEmpower Framework Benchmarks test types, implemented to their
rules, on five server configurations that do the same work behind every
request.

| test | path | work per request |
| --- | --- | --- |
| JSON serialization | `/json` | build `{"message":"Hello, World!"}`, serialise it |
| Single query | `/db` | one random `World` row by id, as JSON |
| Multiple queries | `/queries?queries=N` | N random rows, one query each, clamped to 1..500 (timed at 20) |
| Fortunes | `/fortunes` | all `Fortune` rows, one added in memory, sorted, rendered to HTML with escaping |

The rules are TechEmpower's:
<https://github.com/TechEmpower/FrameworkBenchmarks/wiki/Project-Information-Framework-Tests-Overview>.
Updates, Caching and Plaintext are not implemented.

## The servers

| directory | configuration | HTTP | database |
| --- | --- | --- | --- |
| `khora/` | Khora, `KHORA_FIBERS=threads` | `std::net::http` `Router` | `packages/postgres` pool, `Db` capability |
| `khora/` | Khora, `KHORA_FIBERS=scheduler` | the same binary | the same |
| `go/` | Go, static binary | `net/http`, `encoding/json`, `html/template` | `jackc/pgx/v5` `pgxpool` |
| `node/` | Node | `node:http` | `pg` (node-postgres) `Pool` |
| `bun/` | Bun | `Bun.serve`, `Bun.escapeHTML` | `Bun.sql` |

What every server does the same way:

- **one process, pool of 16 connections** (`POOL` overrides it);
- **multiple queries run one after another**, each its own statement; no
  `IN (...)`, no batching, no fan-out;
- **no caching** of rows, pages or serialised bodies;
- `Server` and `Date` on every response, `Content-Length` on every response;
- prepared statements **as the driver does by default**. The documented
  defaults: pgx caches prepared statements per connection, and Bun.sql
  prepares (`prepare: true`); node-postgres prepares only a query given a
  `name`, and none is; Khora's driver sends the unnamed statement each time
  (parse, bind and execute in one round trip). That is a real difference in
  database work per query, left as each driver ships;
- the release build: Khora `--release`, Go `CGO_ENABLED=0 -trimpath`, Node
  and Bun as they run.

Where they differ, because each library does:

- Fortunes escaping. Khora and Node write `&quot;` and `&apos;`, byte for
  byte TechEmpower's example page. Go's `html/template` writes `&#34;` and
  `&#39;`; `Bun.escapeHTML` writes `&quot;` and `&#x27;`. All are the same
  page to a browser and to TechEmpower's verifier, and differ by at most a
  byte in length.
- The `Date` header. Khora renders it per request, in the app. Go, Node and
  Bun write it from their HTTP libraries; whether each re-renders per request
  or once a second (the rules allow either) was not checked.

Khora gaps that the app works around (noted in the report, not fixed in
`std`): no HTTP-date formatter, no HTML escaping, no template facility, no
way to set `Server`/`Date` for every route at once.

## Setting up (no root)

Postgres 17, from Debian's packages without installing them:

    mkdir -p /general/khora-tmp/pg/debs && cd /general/khora-tmp/pg/debs
    apt-get download postgresql-17 postgresql-client-17 libpq5 postgresql-common postgresql-client-common
    for d in *.deb; do dpkg -x "$d" ../root; done

`init.sh`, `start.sh` and `stop.sh` in `/general/khora-tmp/pg/` do the rest:
`initdb` with user `benchmarkdbuser` / `benchmarkdbpass` and SCRAM, database
`hello_world`, 127.0.0.1:5432 only, TechEmpower's `postgresql.conf` settings
(less `io_workers`, which is PostgreSQL 18's), their
`toolset/databases/postgres/create-postgres.sql` loaded unchanged, and the
server started pinned to CPU 7 with `taskset`. `pg_stat_statements` is
loaded, because the verifier counts queries through it the way
TechEmpower's does.

wrk, built from source without root (it builds its own LuaJIT):

    git clone --depth 1 https://github.com/wg/wrk.git /general/khora-tmp/wrk
    make -C /general/khora-tmp/wrk --jobs 2

The servers, from the repository root. `KHORA_STD` makes the tree's compiler
use the tree's `std`: with `KHORA_HOME` pointing at an installed toolchain,
it otherwise resolves that toolchain's `std`, and `bench/service` fails the
same way.

    KHORA_STD=$PWD/std khora build bench/peers/techempower/khora --release
    sh bench/peers/techempower/go/build.sh       # Go toolchain at /general/toolchains/go
    sh bench/peers/techempower/node/install.sh   # npm install pg

Bun needs nothing beyond `bun` on the machine.

## Verifying

    sh bench/peers/techempower/verify.sh

Starts each of the five in turn on port 8080 and applies TechEmpower's
verifier rules (`verify.py`, a standard-library port of their
`toolset/test_types`): status, `Server`/`Date`/`Content-Type`/length
headers, a `Date` that changes and is accurate, JSON shape and integer types,
`queries` clamping for `2`, `0`, `foo`, `501`, empty and absent, ids in range,
the Fortunes page through their normalising HTML parser, and — through
`pg_stat_statements` — that every request's queries actually reached the
database (a cache or an `IN (...)` shows up as too few). Some findings that
TechEmpower only warns about fail here; each is marked "stricter" in
`verify.py`.

`selftest/selftest.sh` runs `verify.py` against a stand-in server with one
rule broken at a time, to show that each check can fail.

## Measuring

    sh bench/peers/techempower/run.sh

Refuses to run unless Postgres is up and pinned to CPU 7, then runs
`verify.sh`, then `run.py`: servers on CPUs 1,3,5, wrk (`-t2`) on 9,10, 64
connections, 5 rounds of every server and every test, interleaved, 10 s each
after a 5 s warm-up. About half an hour.

Per test and server it reports requests a second (median and range), p50
and p99 latency, peak resident memory, server CPU time per request (from
`/proc/<pid>/stat`), and **Postgres's CPU**. A database test in which
Postgres used 90% or more of its one CPU is flagged `POSTGRES-BOUND`: in that
row every server is waiting on the same database, and the number measures
Postgres rather than the language.

`bench/loadgen` is not used because it can only send `GET /health`.
