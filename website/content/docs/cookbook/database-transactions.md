---
title: Database transactions
sidebar:
  order: 2
---

Khora's database abstraction keeps the transaction contract in `std` while database engines remain packages.

The important rule is behavioral:

- if the transaction body succeeds, commit;
- if the body returns a typed failure, roll back;
- if the body is cancelled, roll back before releasing the connection;
- if commit itself fails, report that failure rather than pretending the transaction succeeded.

## Keep transaction scope narrow

Do only the work that must be atomic while the transaction is open. Avoid remote API calls or unrelated waits while holding database locks.

## Convert driver errors at boundaries

A Postgres or SQLite package may have detailed engine errors. Application code should usually convert those into the smaller failure types its callers actually understand.

## Use exact database values

Database numeric values that represent exact decimal quantities should map to `Decimal`, not `Float`. Silent coercion between cell kinds hides schema mistakes and is intentionally discouraged.

## Cancellation safety is not optional

A request can disappear while its fiber is suspended. Returning that connection to a pool without rollback can leak locks or transaction state into the next borrower. Production drivers must integrate transaction cleanup with Khora's region/cancellation model.
