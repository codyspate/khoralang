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
happens when a fiber is cancelled mid-statement:

```khora
import postgres::pool::{open, with_db};

let pool = open(crew, settings, 8);

with_db(pool, fn () =>
  transfer(10, 20, 2500)
)
```

`with_db` leases a connection, installs it as the `db` capability for the
duration, and returns it afterwards — including when the body raises.
`transfer` never names a connection, which is what keeps the capability from
turning back into a parameter threaded through every signature.

A connection can also be used directly, without a pool, for a script or a
migration. The README covers that shape.

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

## What it does not do yet

Named here because a driver's gaps decide whether it fits a service:

- **TLS.** `SSLRequest` is one message and `std::net::tls` exists, so this is
  closer than it sounds. Until then, a password does not cross the network in
  the clear under SCRAM — but **every row you read does**. On an untrusted
  network, this is not ready.
- **MD5 authentication.** Refused by name rather than hung. `ring` does not
  carry MD5, deliberately.
- **Named prepared statements.** Every query uses the unnamed statement, so a
  hot query pays a parse each time. Round trips are unchanged.
- **Binary result format, `COPY`, notifications, cursors.**

## Status

Versioned with the compiler for convenience, not as a promise — see
[Packages](/docs/packages/) on what the compatibility statement does and does
not cover. It is tested against a real PostgreSQL 17 in the repository's own
suite, and the SCRAM implementation is checked against the RFC 7677 worked
example as well as against a live server.
