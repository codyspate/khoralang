---
title: Database transactions
sidebar:
  order: 2
---

Khora keeps the transaction contract in `std::db` while concrete database engines live in packages. Application code depends on the `Db` **capability**, not on a database value threaded through every function call.

That distinction is the point of the API. A function that talks to the database says so in its type:

```khora
fn load_account(id: Int) -> Result<List<Row>, DbError>
  with { db: Db }
{
  db.query(
    "select id, balance from accounts where id = ?",
    [Cell::Number(id)],
  )
}
```

There is no `db: Db` parameter. `with { db: Db }` is the function's authority to perform database operations, and the caller supplies that authority at a boundary.

`transaction` follows the same rule. It requires `db: Db` through its capability row, so transaction boundaries do not turn the capability back into explicit dependency plumbing.

## Complete example

This complete module transfers money between two accounts. Both application functions require `Db` through their capability rows. The concrete `demo_db` handler appears only at the wiring boundary in `main`.

```khora
module main;

import std::core::{List, Result, Show, print};
import std::db::{Cell, Db, DbError, transaction};

fn demo_db() -> Db {
  handler for Db {
    query: fn (_sql, _params) =>
      Result::Ok(List::Nil),

    execute: fn (sql, _params) => {
      print("execute: ${sql}");
      Result::Ok(1)
    },

    begin: fn () => {
      print("BEGIN");
      Result::Ok(())
    },

    commit: fn () => {
      print("COMMIT");
      Result::Ok(())
    },

    rollback: fn () => {
      print("ROLLBACK");
      Result::Ok(())
    },
  }
}

fn transfer_body(
  from_account: Int,
  to_account: Int,
  amount: Int,
) -> Result<(), DbError>
  with { db: Db }
{
  let debited = db.execute(
    "update accounts set balance = balance - ? where id = ?",
    [Cell::Number(amount), Cell::Number(from_account)],
  );

  match debited {
    Result::Err(error) => Result::Err(error),
    Result::Ok(_) => {
      let credited = db.execute(
        "update accounts set balance = balance + ? where id = ?",
        [Cell::Number(amount), Cell::Number(to_account)],
      );

      match credited {
        Result::Err(error) => Result::Err(error),
        Result::Ok(_) => Result::Ok(()),
      }
    },
  }
}

fn transfer(
  from_account: Int,
  to_account: Int,
  amount: Int,
) -> Result<(), DbError>
  with { db: Db }
{
  transaction(fn () =>
    transfer_body(from_account, to_account, amount)
  )
}

pub fn main() {
  with { db: demo_db() } {
    match transfer(10, 20, 2500) {
      Result::Ok(_) => print("transfer committed"),
      Result::Err(error) => print("transfer failed: ${error.show()}"),
    }
  }
}
```

The dependency flow is visible directly in the signatures:

```text
main installs db
     ↓
transfer      with { db: Db }
     ↓
transaction   with { db: Db }
     ↓
transfer_body with { db: Db }
     ↓
db.execute(...)
```

Neither `transfer` nor `transfer_body` knows whether `db` is PostgreSQL, SQLite, D1, an in-memory test handler, or something else. They know only that a `Db` capability is available.

## Failure behavior

If either `execute` returns `Result::Err`, `transfer_body` returns that error. `transaction` sees the failed result and rolls the transaction back instead of committing it.

If the body is cancelled at a cancellation point, the transaction's internal region finalizer performs the rollback during unwinding. A caller does not need a second cancellation-specific transaction API.

If `commit` itself fails, the commit error is returned. The helper does not report success for a transaction the database did not commit.

## Install a real database at the boundary

The portable application contract is the capability row:

```khora
fn transfer(from_account: Int, to_account: Int, amount: Int)
  -> Result<(), DbError>
  with { db: Db }
```

A PostgreSQL, SQLite, D1, or other package constructs a handler that satisfies `Db`. Install that handler at the application's composition boundary:

```khora
with { db: postgres_db } {
  transfer(10, 20, 2500)
}
```

The PostgreSQL pool helper does the same installation for a leased connection. Its callback requires `db: Db`; `with_db` supplies it and removes that requirement from the caller:

```khora
with_db(pool, fn () =>
  transfer(10, 20, 2500)
)
```

That is the useful layering: the pool owns connection acquisition and release, while application code simply declares the database authority it needs.

## Testing becomes substitution, not plumbing

Because the dependency is a capability, the same function can be tested with a different handler without changing its arguments:

```khora
with { db: recording_db() } {
  transfer(10, 20, 2500)
}
```

That is the intended Khora architecture: business functions advertise external authority in `with`, while concrete handlers are assembled at narrow boundaries.

For exact `Db`, `Cell`, `DbError`, and `transaction` declarations, see the [database API reference](/docs/stdlib/api/db/). For the capability model itself, see [Effects and capabilities](/docs/guide/effects-and-capabilities/). For the cleanup mechanism underneath cancellation-safe transactions, see [Resources and regions](/docs/guide/resources-and-regions/).
