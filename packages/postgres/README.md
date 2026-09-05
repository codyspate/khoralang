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

## Using it as a `Db` capability

Application code should normally depend on `std::db::Db`, not on a PostgreSQL
connection value. The pool owns the concrete connections and `with_db` installs
a leased connection as the `db: Db` capability for the callback:

```khora
import std::core::{Fibers, List, Result, print};
import std::db::{Cell, Db, DbError, transaction};
import postgres::db::{Settings};
import postgres::pool::{close, open, with_db};

fn insert_person(name: String) -> Result<Int, DbError>
  with { db: Db }
{
  db.execute(
    "insert into people (name) values ($1)",
    List::Cons(Cell::Text(name), List::Nil),
  )
}

fn store_person(name: String) -> Result<Int, DbError>
  with { db: Db }
{
  transaction(fn () => insert_person(name))
}

fn main() -> Int {
  let settings: Settings = {
    host: "127.0.0.1",
    port: 5432,
    user: "user",
    database: "database",
    secret: "secret",
  };

  let crew = Fibers::open();
  let pool = open(crew, settings, 4);

  let result = with_db(pool, fn () => store_person("Ada"));
  match result {
    Result::Err(_) => print("no database connection"),
    Result::Ok(inner) => match inner {
      Result::Err(_) => print("insert failed"),
      Result::Ok(_) => print("stored"),
    },
  };

  close(pool);
  0
}
```

The application functions have no `db: Db` parameter. Their external authority
is part of their type via `with { db: Db }`. `with_db` is the composition
boundary that satisfies that requirement for the duration of a connection
lease.

## Using a connection directly

The lower-level connection API is available when writing database
infrastructure or when the portable `Db` contract is not enough:

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

## Testing

The protocol layer is pure bytes and tested without a server:

```
khora test packages/postgres
```

The driver is tested against a real one, which is skipped unless asked for:

```
KHORA_POSTGRES=1 cargo test -p khora-codegen-llvm --features llvm --test suite -- postgres::
```

`docker-compose.yml` brings a server up on **5433**, deliberately not 5432, so
a database somebody already runs cannot be used by accident.
