---
title: Effects, capabilities, and failure rows
sidebar:
  order: 19
---

Khora function types may carry two orthogonal row-polymorphic dimensions beyond arguments and result values.

A `with` row describes required capabilities/effects. A `raises` row describes recoverable failures the computation may produce.

```khora
fn load_user(id: Id) -> User
  with { db: Db }
  raises DbError
```

A call may propagate a compatible typed failure with postfix `!`. Otherwise the current scope must handle it.

Handlers provide effect implementations for a lexical scope. Capability requirements remain statically visible even though application code calls operations directly.

Rows are polymorphic where appropriate so a generic abstraction can preserve effects/failures it does not itself interpret instead of hard-coding every caller's environment.

Traps such as bounds/overflow failures are not members of a normal `raises` row. They represent violated program invariants rather than expected domain outcomes.
