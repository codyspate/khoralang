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
postgres = { git = "https://github.com/codyspate/khoralang", rev = "v0.4.0", subdir = "packages/postgres" }
```

Use `v0.4.0-rc.2` instead when testing that published candidate with a
`0.4.0-rc.2` toolchain; the stable tag is available only after publication.

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
  one, in the middle of a statement or a batch, or inside a transaction's
  `ROLLBACK` — gives its connection back, and the pool stays its full size.
  A reply still arriving on it is read to the end before the connection is
  lent again, so the next borrower never receives it. A fiber the body
  spawns cannot use the body's `db`; it takes a lease of its own.
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
- A lease hands the borrower the connection itself: the borrower's fiber
  writes the socket and reads the reply, with no fiber in between. What a
  lease costs on top of its statements is taking the connection, the check
  (one non-blocking read), and giving it back.

## TLS

A connection to any host but this machine is encrypted, and the server's
certificate and host name are checked, unless you say otherwise. The driver
asks with PostgreSQL's `SSLRequest` and runs the TLS handshake on the same
connection, so the user name, the password exchange and every row go over
TLS. A server that does not offer TLS is refused, never retried in the clear.

- **The default** is plain TCP for `localhost` and for a loopback address
  written out in full (`127.0.0.1` to `127.255.255.255`, `::1`), and TLS
  with full verification for every other host, including a name that only
  starts like a loopback address (`127.0.0.1.nip.io` is a public name).
  `open`, `open_within`, `pool::open` and `pool::open_with` all use it.
- **Choosing** is `Tls::Off` or `Tls::Verify(roots)`, passed to
  `conn::open_tls`, `pool::open_tls` or `db::serve_tls`. `roots` is the PEM
  text of a certificate authority to trust besides the machine's own store,
  or `""` for none.
- **libpq's settings** go through `Tls::sslmode(mode, rootcert)`, so a
  provider's connection string can be used as it is. The mode is trimmed
  first, so a value read from a file with its newline still reads.

```khora
import postgres::conn::{Tls};
import postgres::pool::{Reconnect, open_tls};

// `bundle` is the text of the provider's CA file, read by the caller.
let tls = match Tls::sslmode("verify-full", bundle) {
  Result::Ok(t) => t,
  Result::Err(_) => Tls::Verify(bundle),
};
let pool = open_tls(crew, settings, tls, 8, Reconnect::default());
```

**`require` and `verify-ca` check more here than in libpq, never less.**
There is no TLS without verification: every mode that asks for TLS checks
the certificate chain and that the certificate names the host.

| `sslmode` | Here | In libpq |
| --- | --- | --- |
| `disable` | plain TCP | the same |
| `allow`, `prefer` | TLS, verified, or refused | falls back to cleartext when the server says no |
| `require` | TLS, verified, or refused | no certificate check |
| `verify-ca` | TLS, verified, or refused | no host name check |
| `verify-full` | TLS, verified, or refused | the same |

`prefer` refuses rather than falls back because a fallback lets anybody on
the network path switch TLS off, by answering no on the server's behalf.

**`sslrootcert` is the certificate's text, not a path**: the driver reads no
files, so it needs no file capability. Read the file and pass what it holds.
A path passed by mistake is refused as not PEM, and the message says to pass
the contents.

- **Amazon RDS and Aurora**: `verify-full`, with RDS's global CA bundle
  ([download](https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem))
  as `sslrootcert`, and the endpoint name as the host.
- **Google Cloud SQL**: depends on the instance's server CA mode. With the
  **shared CA** or a **customer-managed CA**, pass the server CA as
  `sslrootcert` and connect by the instance's DNS name (`dnsNames` in
  `gcloud sql instances describe`), which needs that name to resolve. With
  the **per-instance CA** (the older default), the certificate names no host
  you can connect by over a public IP, unless the instance uses Private
  Service Connect, and this driver always checks the name, so it cannot
  connect directly. Switch the instance to a shared or customer-managed CA,
  or connect through the Cloud SQL Auth Proxy on `127.0.0.1`, which does the
  TLS itself.
- **Neon and Azure Database for PostgreSQL**: publicly signed certificates,
  so `Tls::Verify("")` on a machine with an ordinary trust store. Azure's
  certificates chain to DigiCert Global Root G2 or Microsoft RSA Root CA
  2017; Microsoft is moving servers off the older DigiCert Global Root G1
  (CA) in 2026, so a machine whose store has only G1 needs G2 added.
- **Supabase**: download the certificate from the project's Database
  Settings (`prod-supabase.cer`) and pass its contents as `sslrootcert`. It
  must be PEM text (`-----BEGIN CERTIFICATE-----`); if the file is DER,
  convert it first with `openssl x509 -inform der -in prod-supabase.cer
  -out prod-supabase.pem`.
- **A provider that hands out a CA file to download**: that file, as PEM, as
  `sslrootcert`.

A refusal says which it was: the server does not offer TLS, its certificate
is not signed by a trusted authority, the certificate does not name the
host, or the handshake broke off or ran out of time. An answer of `N`
names the call that connects without TLS, for a server on a network you
trust. Through a pool the refusal is the `Disconnected` reason, and the
first two are answered at once: a pool does not retry, on its fast
schedule, a refusal that will be the same next time.

Not offered: client certificates, direct TLS (PostgreSQL 17's
`sslnegotiation=direct`; servers that offer it also answer `SSLRequest`),
and `SCRAM-SHA-256-PLUS`, below.

## Authentication

`scram-sha-256` is the default on PostgreSQL 14 and later, and it is what this
speaks — a stock server needs nothing done to it first.

The password is never transmitted. The client proves it knows one by signing a
challenge built from both sides' nonces, so a recording of one exchange cannot
be replayed into another. **The server is made to prove itself too**: its
`ServerSignature` is checked rather than accepted, because a client that skips
that has proved itself to a peer it never made prove anything back.

A server that asks for a cleartext password gets one. Over TLS or to a
loopback address that is reasonable; to a remote host with `Tls::Off` it
crosses the network readable.

`SCRAM-SHA-256-PLUS` is deliberately **not** offered. The channel-binding
variant exists so that a client refuses to authenticate through a proxy it
cannot see, and advertising it without binding to the channel throws that
away. Binding needs a hash of the server's certificate from the TLS session,
which `std::net::tls` does not hand out.

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
- **A borrower canceled while its batch's replies are arriving** leaves the
  rest on the connection, and the connection knows how many are still owed.
  They are read to the end before it is lent again, so the next borrower
  never receives them.

## What it does not do yet

Named here because a driver's gaps decide whether it fits a service:

- **MD5 authentication.** Refused by name rather than hung. `ring` does not
  carry MD5, deliberately.
- **Client certificates and `SCRAM-SHA-256-PLUS`.** A server that requires
  either refuses this driver.
- **Binary result format, `COPY`, notifications, cursors.**

## Status

Versioned with the compiler for convenience, not as a promise — see
[Packages](/docs/packages/) on what the compatibility statement does and does
not cover. It is tested against a real PostgreSQL 17 in the repository's own
suite, over TLS and without it, and the SCRAM implementation is checked
against the RFC 7677 worked example as well as against a live server.
