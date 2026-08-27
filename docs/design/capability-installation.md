# Installing a capability by its type

**Built**, except the two lints and the unlabelled `context`, which are
marked below. This was the specification; it now describes what exists.

## The problem

A `with` clause names a capability twice: once by label, once by type.

```khora
fn transfer(amount: Decimal) -> () with { db: PostgresDb } raises DbError
fn reconcile(amount: Decimal) -> () with { database: PostgresDb } raises DbError
```

The type is the same. The labels are not, and the label is what a caller has to
supply:

```khora
pub context MyDatabase {
  db: handler for PostgresDb { .. },
}

with MyDatabase {
  transfer(100.00d)!;
  reconcile(100.00d)!;   // error: `reconcile` needs `database: PostgresDb`
}
```

The caller did provide a `PostgresDb`. It provided it under the wrong name.

## Why the label is not a lookup key

The obvious reading is that `db` is a key and the fix is to be consistent about
keys. That reading is wrong, and the compiler says so:

```khora
fn reconcile(..) -> () with { database: PostgresDb } {
  db.execute_sql("..")   // error: cannot find `db` in this scope
}
```

**The label is the binding's name inside the body.** `docs/design/capability-passing.md`
settles what that means: "`with` is a block of `let`s. A capability is an
ordinary binding", and a function's labels "become extra parameters. A call site
supplies them from whatever is visible there."

So a capability label is a **parameter name**. And the question this document
answers is the one every language answers about parameter names:

> Why does the caller have to know what the callee called its parameter?

Nowhere else does it. Ordinary arguments are positional. This is the one place
in Khora where a caller must reproduce a name chosen inside a function it is
calling — and, worse, must reproduce *every* such name chosen by every function
it transitively calls.

## Why consistency is not the answer

The workaround today is to supply both names:

```khora
pub context BothNames {
  db:       handler for PostgresDb { .. },
  database: handler for PostgresDb { .. },
}
```

This compiles. It does not scale, for three separate reasons:

- **You must know every label.** Not only in your own code — in every package
  you depend on. A registry makes that other people's choices.
- **A context cannot leave its file.** `context_bindings` looks in the
  file's own declarations, and importing one reports "cannot find a `context`
  named `Prod` in this file". So the workaround cannot even be shared.
- **It duplicates the handler**, or binds one constant twice, for no reason a
  reader can see.

A lint that demanded consistent labels would fix the first case and neither of
the others, and it would be a lint about somebody else's package.

## The decision

**A path after `with` may name a handler value, and it is bound to every label
of its type that the body requires.**

```khora
const MyDatabase = handler for PostgresDb { .. };

with MyDatabase {
  transfer(100.00d)!;    // binds `db`
  reconcile(100.00d)!;   // binds `database`
}
```

Several, comma-separated:

```khora
with MyDatabase, SystemClock { .. }
```

The declaration side does not change. A signature still says
`with { db: PostgresDb }`, because there the label *is* the parameter name and
the body reads it.

### The type is the capability; the label is a nickname

This is the rule the decision rests on, and it is worth stating plainly because
it is a change in emphasis. Today the pair `(label, type)` identifies a
capability at an installation. After this, at an installation the **type**
identifies it and the label is a local convenience.

The consequence to accept: two genuinely different capabilities of the same
type — a primary and a replica database — cannot be told apart by the
shorthand, because nothing distinguishes them but a name the installer is no
longer writing.

```khora
fn settle() -> () with { primary: Db, replica: Db }

with OneDatabase { settle() }   // binds *both* to the same handler
```

Three things make that acceptable rather than dangerous:

1. **The explicit form is unchanged and always available.**
   `with { primary: a, replica: b }` means exactly what it says. The shorthand
   is opt-in; nothing that works today changes.
2. **Distinct capabilities should be distinct types.** If a primary and a
   replica are not interchangeable, they are not the same capability, and
   saying so in the type is what makes every other part of the language help.
   The shorthand nudges towards the encoding that was already better.
3. **A lint reports the fan-out**, below.

### `capability-fans-out`, a lint

When one value binds more than one label, say so:

```
warning: `OneDatabase` is installed as both `primary` and `replica`
  = they are both `Db`, so one value fills both. If they are meant to be
    different databases, name them: with { primary: .., replica: .. }
```

Default `warn`. It is not an error, because binding two labels from one value
is exactly what the feature is for when the labels are `db` and `database`; it
is only suspicious when the labels read like different things, and no compiler
can tell those apart.

### What happens when nothing matches

`with MyDatabase { print("hi") }` binds nothing. That is a mistake — the
installation is dead code — and it is reported the way `useless-allow` reports
a suppression that suppressed nothing.

### What a `context` becomes

A `context` today is a row of `label: value`, and it exists because there was
no other way to install several capabilities under one name. Once a handler
value can be installed on its own, a context is **a named bundle of handlers**
and nothing else -- which is a smaller and more honest job than it has now.

That suggests where it goes next, and the change is small enough to state:

```khora
pub context Production {
  env_config(),
  Scope::root,
  postgres_db()!,
  sql_ledger(),
  openai_classifier()!,
}
```

Entries with no label bind by type, exactly as `with <value>` does. Entries
that keep a label keep today's meaning, so nothing written now stops working
and the two forms may be mixed where one capability genuinely needs pinning.

**Sequential composition survives this, and that is the part worth checking.**
A context's bindings are sequential so that a handler may use the ones above
it, which is what keeps composition flat instead of nesting one `with` per
service. It is fair to ask how an entry reaches the one above it once neither
has a name.

It already does not use the name. `docs/design/effects.md`'s own example is
the proof:

```khora
pub fn postgres_db() -> Db with { config: Config, scope: Scope } raises ConfigError
pub fn sql_ledger() -> Ledger with { db: Db }
```

`postgres_db()` reaches `config` through its **own capability row**, not by
mentioning a binding called `config`. So in the labelled version the labels
had to match what these functions call their requirements; in the unlabelled
version the types match instead, and the same chain composes:
`env_config()` satisfies `postgres_db`'s `config`, `postgres_db()!` satisfies
`sql_ledger`'s `db`.

The labels were carrying no information the types did not already have. That
is the whole argument for removing them, and it is the same argument as the
one for `with <value>`, applied one level up.

**This is worth doing for a reason beyond tidiness.** A context cannot leave
its file, because `context_bindings` looks only in the file's own
declarations. A `const` holding a handler *can* be imported. So a context of
values is one that can be assembled from other modules' handlers, which is the
thing the current one cannot do and the reason the workaround in the previous
section does not scale.

Not built here. Recorded because it is the shape the rest of this points at,
and because deciding it now stops `context` growing a second purpose that
would have to be removed later.

### Which wins, where a name is both

Where a path names both a `context` and a value, the context wins. That is
what the name means today, and silently changing it would be worse than an
error.

## Why this cannot be done in lowering

`lower_installation` turns `with { db: v } { body }` into a block whose
statements are `let db = v` and whose tail is the body, and records the labels
in `Body::installs`. Everything is known from the source text.

By-type installation is not. The labels come from the *requirement rows of
whatever the body calls*, which is a fact about types, and HIR has none. So the
resolution has to happen in the checker, which is the first place that knows
both the value's type and the body's requirements — and it already computes the
second, because subtracting it is what `with` does.

### The shape to build

1. **Parser.** `with` already accepts a path (`context_row`). Extend it to a
   comma-separated list. Both the block form and the postfix form.
2. **Lowering.** A path that is not a `context` becomes an installation of an
   ordinary expression: `Stmt::Let` binding a fresh local to the value, plus an
   entry in a new `Body::installs_by_type` naming that local. No labels.
3. **Checker.** When inferring an install block, for each by-type local: take
   its type, take the body's required row, and every label in that row whose
   type unifies with it is supplied. Add those labels to `installed` — the
   subtraction in `check/effects.rs` then works unchanged — and record the
   resolved `(label, local)` pairs in `Checked`.
4. **Codegen.** Read the resolved pairs and bind each label to the local, so
   that the name lookup which passes capabilities to calls finds it. This is
   the only step that needs the labels to be real bindings, and it is the step
   that already knows how to make them.
5. **Lints.** `capability-fans-out`, and the nothing-matched case.

The order matters for the checker: the body is inferred first — it must be,
because its requirements are the input — and the installation discharges them
afterwards. That is what `with` already does, so nothing new is being asked of
inference. An open row variable (`{ 'e | .. }`) contributes only its concrete
part, which is the same limit every other row operation has.

## What is not decided here

- **Whether contexts should be importable.** Named here as the other half of
  the problem, deliberately not solved.
- **Whether `handler for X` values should be inferable from the type alone**,
  so that `with PostgresDb` could find *the* implementation. That is a coherent
  next step and a much larger one: it needs a notion of a canonical instance,
  which Khora does not have and which is where trait systems get complicated.
