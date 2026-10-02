---
title: postgres
description: A PostgreSQL client that speaks the wire protocol directly.
---

A PostgreSQL client written in Khora. It speaks the v3 wire protocol over a
socket — there is no `libpq` to install and no C to link — and it supplies the
[`Db`](/docs/stdlib/api/db/) capability that `std::db`'s transaction contract
is written against.

```toml
[dependencies]
postgres = { git = "https://github.com/codyspate/khoralang", rev = "v0.3.0", subdir = "packages/postgres" }
```

**[`packages/postgres/README.md`](https://github.com/codyspate/khoralang/blob/main/packages/postgres/README.md)
is the reference** — every function, both usage styles, and the honest list of
what is missing. This page is the overview.

## As a capability

The point of the package is that a caller does not hold a connection. It holds
`Db`, and `std::db`'s [`transaction`](/docs/stdlib/api/db/) decides what
happens when a fiber is canceled mid-statement:

```khora
import postgres::pool::{open, with_db};

let pool = open(crew, settings, 8);

with_db(pool, fn () =>
  transfer(10, 20, 2500)
)
```

A `transaction` inside another one on the same lease is a savepoint on that
connection (`SAVEPOINT khora_sp_1`, `khora_sp_2`, and so on, one per level),
released when the inner body answers `Ok` and rolled back to when it does
not. Each connection keeps its own depth, so fibers on different leases nest
independently. A `COMMIT` that PostgreSQL answers with a rollback, because a
statement in the transaction failed, is reported as `DbError::RolledBack`.

`with_db` leases a connection, installs it as the `db` capability for the
duration, and returns it afterwards — including when the body raises, and
including when the fiber is canceled, whether in the body or while it is
still waiting for a connection.
`transfer` never names a connection, which is what keeps the capability from
turning back into a parameter threaded through every signature.

A connection can also be used directly, without a pool, for a script or a
migration. The README covers that shape.

## Lost connections

A pool reconnects a connection it loses, and never lends one that is down.

- **What counts as lost.** A reply cut off partway, a `ROLLBACK` that did not
  arrive (`std::db` calls `broken`), a connection the server closed or reset
  while it sat idle, and one left inside a transaction. Each connection is
  checked before it is lent, so one the server closed between leases is
  caught there rather than by the next query. A notification, a notice or a
  parameter change the server sent to an idle connection does not count: the
  connection is lent as it is, with its session. A message of any other kind
  counts as soon as its first byte is in, so a connection the server is in
  the middle of ending is not lent.
- **A stopped caller costs nothing.** A fiber stopped by `cancel`, `abort` or
  `cancel_within` anywhere in `with_db` — waiting for a connection, holding
  one, in the middle of a statement, inside a transaction's `ROLLBACK`, or
  while fibers it started are still using `db` — gives its connection back,
  and the pool stays its full size. Statements those fibers had already
  queued are answered first and their answers dropped; anything they send
  afterwards is refused with `Disconnected`, and never reaches the next
  borrower.
- **Reconnecting.** The old socket is closed first and nothing more is read
  from it. The new one is tried on a backoff: 50 ms, doubling to 5 s, each
  delay drawn at 50-100% of its value, for up to 30 s. `with_db` waits
  through it, the same way it waits when every connection is busy. A
  connection that opens and fails its check before it is ever lent counts
  as a failed attempt on the same backoff, so a server that accepts
  connections and spoils each one sees the backoff's handful of attempts,
  not a slot reconnecting as fast as it can. The 30 s counts time spent
  reconnecting, not the time a connection sat open, so one left idle for
  longer and then closed by a server restart is retried on the backoff too.
- **Shrinking.** A connection that has not come back after 30 s leaves the
  pool. It is not lent, and it tries again every 30 s (±20%) until it
  connects, when it rejoins. So a pool that lost its server grows back to
  full size within one retry interval of the server's return.
- **An empty pool answers at once.** When no connection is live or
  reconnecting, `with_db` returns `Err(DbError::Disconnected(reason))` with
  the reason the last attempt failed, and so do the callers that were
  already waiting. It does not wait for the next 30-second retry.

```khora
import std::resilience::{Schedule};
import postgres::pool::{Reconnect, health, open_with};

let plan: Reconnect = {
  fast: Schedule::UpTo(Schedule::backoff(100, 2000), 10000),
  slow: Option::Some(60000),
  handshake: 5000,
};
let pool = open_with(crew, settings, 8, plan);
let now = health(pool);   // { live, reconnecting, down }
```

`Reconnect::default()` is what `open` uses. `Reconnect::never()` makes one
attempt and never retries, and `slow: Option::None` keeps a connection that
gave up out of the pool for good. `handshake` is how long, in milliseconds,
an attempt waits for the server to finish the startup exchange before it
counts as failed and the backoff goes on; the default is 10 s. A server that
accepts connections and never answers is therefore a server that is down,
not a pool that hangs. `health` is for a readiness check or a test: a pool
with `live` and `reconnecting` both 0 is failing its callers.

`open` returns before any connection has opened, and a pool lends nothing
until one does. A pool whose server is unreachable answers its first callers
`Disconnected` once the 30-second backoff has run out.

**Limits.**

- A connection that dies between the check and the borrower's first
  statement answers that statement `Disconnected`. The pool does not
  resend it, because it cannot know whether the server ran it. Retrying is
  the caller's decision, and the connection reconnects once the lease ends.
- `close` ends a connection that is waiting to retry within about 25 ms, and
  one in the middle of the startup exchange within the `handshake` bound. The
  TCP connect before that exchange has no bound of its own: an address that
  drops packets rather than refusing takes the operating system's connect
  timeout, which is minutes on Linux, and `close` waits for it.
- A connection whose peer vanished without closing (a cable pulled, a
  firewall dropping the flow) looks healthy until a statement times out on
  it. The check reads what has arrived and cannot see that.
- Each lease costs a request channel of its own and one message to the
  connection's serving fiber, the check, on top of the statements themselves.

## Authentication

`scram-sha-256` is the default on PostgreSQL 14 and later, and it is what this
speaks — a stock server needs nothing done to it first.

The password is never transmitted. The client proves it knows one by signing a
challenge built from both sides' nonces, so a recording of one exchange cannot
be replayed into another. **The server is made to prove itself too**: its
`ServerSignature` is checked rather than accepted, because a client that skips
that has proved itself to a peer it never made prove anything back.

`SCRAM-SHA-256-PLUS` is deliberately **not** offered. The channel-binding
variant exists so that a client refuses to authenticate through a proxy it
cannot see, and advertising it without binding to the channel throws that away.

## Prepared statements

Each connection parses a statement once and reuses the parse by name. The
first time a connection meets a statement it sends Parse, Bind, Describe,
Execute and Sync, and keeps the name and the columns the server described.
After that, running the same SQL sends only Bind, Execute and Sync. Round
trips are the same either way: one write, one read.

- **A refusal forgets the statement.** If the server refuses a prepared
  statement, the connection drops it and parses it again on the next call.
  This covers a statement the server has dropped (`DEALLOCATE`,
  `DISCARD ALL`), and a table altered so that the statement's result type
  changed. The call that meets the change still fails, as it does in pgx.
- **At most 512 per connection.** One more closes the statement used longest
  ago, on the server as well, so a program that builds its SQL out of values
  cannot fill the server's memory with plans.
- **Not behind a transaction-mode pooler.** A pooler that hands each
  transaction a different server connection (PgBouncer's transaction mode)
  does not carry the names across, so use session mode, as with pgx.

## Many lookups in one exchange

`db.query_each(sql, sets)` runs one statement once per set of values and
answers what `db.query` once per set would, in order. This handler pipelines
it: every set goes to the server in one write, as Bind, Execute and Sync,
and the replies are read in order. Twenty lookups cost one round trip
instead of twenty.

```khora
import std::core::{List, Result};
import std::db::{Cell, Db, DbError, Row};

fn users(ids: List<Int>) -> List<Result<List<Row>, DbError>> with { db: Db } {
  db.query_each(
    "select id, name from users where id = $1",
    List::map(ids, fn id => [Cell::Number(id)]),
  )
}
```

- **Each set is its own statement, with its own `Sync`.** Outside a
  transaction each runs in its own implicit transaction, so a set that fails
  fails alone and the sets after it still run. Inside a transaction a failed
  set aborts it, and the sets after it are refused, as separate `query`
  calls would be.
- **The first use of a statement on a connection prepares it** with the
  first set, the ordinary way, and pipelines the rest behind it.
- **A batch that is refused throughout is asked again.** A statement the
  server dropped refuses every set that names it; the sets after the first
  refusal are then run again after a fresh parse, which is what separate
  calls would have done.
- **A batch is one request to the connection's serving fiber**, which reads
  every reply before it answers anything else. A borrower canceled while its
  replies are arriving leaves the rest for that fiber to read, so the next
  borrower never receives them.

## What it does not do yet

Named here because a driver's gaps decide whether it fits a service:

- **TLS.** `SSLRequest` is one message and `std::net::tls` exists, so this is
  closer than it sounds. Until then, a password does not cross the network in
  the clear under SCRAM — but **every row you read does**. On an untrusted
  network, this is not ready.
- **MD5 authentication.** Refused by name rather than hung. `ring` does not
  carry MD5, deliberately.
- **Binary result format, `COPY`, notifications, cursors.**

## Status

Versioned with the compiler for convenience, not as a promise — see
[Packages](/docs/packages/) on what the compatibility statement does and does
not cover. It is tested against a real PostgreSQL 17 in the repository's own
suite, and the SCRAM implementation is checked against the RFC 7677 worked
example as well as against a live server.
