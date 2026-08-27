---
title: Database transactions
sidebar:
  order: 2
---

Khora keeps the transaction contract in `std::db` while concrete database engines live in packages. Application code can therefore express transaction behavior against `Db` without depending on a driver's connection type.

The contract of `transaction` is the important part: commit when the body returns `Result::Ok`, roll back when it returns `Result::Err`, and roll back during cancellation before the transaction scope is released.

## Complete example

The module below is complete and uses the real `Db` effect. The `demo_db` handler makes the transaction visible by printing `BEGIN`, statements, and `COMMIT`; in an application, the wiring boundary supplies a `Db` handler from the chosen database package instead.

```khora
module main;

import std::core::{List, Result, print};
import std::db::{Cell, Db, DbError, Row, transaction};

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
  db: Db,
  from_account: Int,
  to_account: Int,
  amount: Int,
) -> Result<(), DbError> {
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
  db: Db,
  from_account: Int,
  to_account: Int,
  amount: Int,
) -> Result<(), DbError> {
  transaction(
    db,
    fn () => transfer_body(db, from_account, to_account, amount),
  )
}

pub fn main() {
  let db = demo_db();

  match transfer(db, 10, 20, 2500) {
    Result::Ok(_) => print("transfer committed"),
    Result::Err(error) => print("transfer failed: ${error.show()}"),
  }
}
```

The two statements are inside the callback passed to `transaction`, so they are one transaction rather than two unrelated database calls.

## Failure behavior

If either `execute` returns `Result::Err`, `transfer_body` returns that error. `transaction` sees the failed result and rolls the transaction back instead of committing it.

If the body is cancelled at a cancellation point, the transaction's internal region finalizer performs the rollback during unwinding. A caller does not need a second cancellation-specific transaction API.

If `commit` itself fails, the commit error is returned. The helper does not report success for a transaction the database did not commit.

## Wire in a real database at the boundary

The portable application function only requires a `Db` value:

```khora
fn transfer(db: Db, from_account: Int, to_account: Int, amount: Int)
  -> Result<(), DbError>
```

A PostgreSQL, SQLite, D1, or other package is responsible for constructing a handler that satisfies the `Db` operations. Keep that provider-specific construction near application startup; keep transaction and domain logic against the standard capability when the standard contract is sufficient.

For exact `Db`, `Cell`, `Row`, `DbError`, and `transaction` declarations, see the [database API reference](/docs/stdlib/api/db/). For the cleanup mechanism underneath cancellation-safe transactions, see [Resources and regions](/docs/guide/resources-and-regions/).
