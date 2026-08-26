# postgres

A PostgreSQL driver for Khora, written in Khora. No C client, no `libpq` — the
wire protocol, spoken directly over `std::net::socket`.

## Installing

```
khora install https://github.com/khora-lang/khora --subdir packages/postgres
```

`--subdir` because a git URL names a *repository*, and this package lives
inside one that is mostly a compiler. The command writes the entry, so
`khora.toml` ends up with:

```toml
[dependencies]
postgres = { git = "https://github.com/khora-lang/khora", rev = "main", subdir = "packages/postgres" }
```

## Using it

```khora
import std::core::{List, Result, print};
import std::db::{Cell, Row};
import postgres::conn::{PgError, ask, close, open};

fn main() -> Int {
  match open("127.0.0.1", 5432, "user", "database", "secret") {
    Result::Err(_) => 1,
    Result::Ok(c) => {
      let values = List::Cons(Cell::Number(42), List::Nil);
      match ask(c, "select name from people where id = $1", values) {
        Result::Ok(answer) => print(answer.tag),
        Result::Err(why) => match why {
          PgError::Refused(m) => print("refused: " + m),
          PgError::Unreachable(m) => print("unreachable: " + m),
          PgError::Closed(m) => print("closed: " + m),
          PgError::Unsupported(m) => print("unsupported: " + m),
        },
      };
      close(c);
      0
    },
  }
}
```

## `ask` or `run`

There are two, and it is not a preference.

**`ask(c, sql, values)`** takes parameters as `$1`, `$2`, numbered from one,
and sends them as values. Use it for anything with a parameter in it. The
values travel in their own protocol message with their own lengths, so a value
containing a quote, a semicolon or `--` is a value containing those characters
and never a second statement.

**`run(c, sql)`** takes one string and nothing else. Use it for statements with
no parameters — schema, `BEGIN`, a fixed query. It is the only one that can
carry several statements separated by semicolons, which is exactly why it must
never be handed anything a user typed.

Interpolating a value into `run` is the oldest hole there is. The only defence
a library can offer is to make the safe call the shorter one, and it is.

## What arrives

Every value comes back as text and becomes a `std::db::Cell`:

| PostgreSQL | `Cell` |
| --- | --- |
| `int2`, `int4`, `int8` | `Number` |
| `bool` | `Flag` |
| NULL | `Null` |
| everything else | `Text` |

`numeric` is `Text` rather than `Money`, deliberately: a `numeric` that failed
to parse would have to become either a wrong number or a lost value, and
neither is a decision to take quietly. The server's own digits are kept until
that is settled.

Going the other way, `ask` accepts all five `Cell` kinds including `Money`.

## What it does not do yet

- **MD5 and SCRAM-SHA-256 authentication.** Both need a hash the runtime does
  not expose to Khora. A server set to either is refused *by name* rather than
  hung, because "connection failed" against a default PostgreSQL install would
  send somebody looking at their network. Cleartext and trust work.
- **TLS.** `SSLRequest` is one message and `std::net::tls` exists, so this is
  closer than it sounds.
- **Named prepared statements.** Every `ask` uses the unnamed statement, so the
  parse is not reused across calls. Round trips are unchanged — one write, one
  read — but a hot query pays a parse each time.
- **Binary result format**, **`COPY`**, **notifications**, **cursors**.
- **The `std::db::Db` capability.** The functions here are direct; wiring them
  into `std::db`'s capability record is next.

## Testing

The protocol layer is pure bytes and tested without a server:

```
khora test packages/postgres
```

The driver is tested against a real one, which is skipped unless asked for:

```
KHORA_POSTGRES=1 cargo test -p khora-codegen-llvm --features llvm --test postgres
```

`docker-compose.yml` brings a server up on **5433**, deliberately not 5432, so
a database somebody already runs cannot be used by accident.
