# Tests

Phase 5.4. `test "name" { .. }` has been in the grammar since phase 1 and did
nothing until now; this is what it does and why.

## A test is a function body

`test "halving works" { .. }` lowers to an ordinary function body, keyed
`#test$<n>` — `#` cannot occur in an identifier, so the key cannot collide with
anything a program declared. It takes nothing, returns nothing, and is
monomorphized, reference counted and code-generated exactly as any other body
is.

Numbered by position rather than by name. A name is what a person reads in a
report; nothing stops two tests sharing one, and a compiler that assumed
otherwise would produce a confusing failure the first time somebody
copy-pasted.

Being a body is not a technicality. It means a test is *checked* — which it was
not before, and the reference application's tests had been quietly wrong for
some time as a result — and it means everything the language can do works
inside one with no special cases: capabilities, `with Mock`, `catch`, closures.

## A test's error row is open

An error escaping a test is a **failing test**, not a program that does not
compile. So a test's `raises` row is a bare row variable: whatever the body
demands, the row absorbs.

That is the one thing about a test's signature that is interesting, and it is
worth being explicit that it is a choice. The alternative — requiring a test to
handle everything it calls — would make the most common kind of failing test
impossible to write.

## `assert` is the mark

```
test "halving an even number" {
  assert(halve(8)! == 4);
}
```

A false assertion leaves the test the way a raise leaves a function: release
what the frame owns, and return with a tag. The tag is reserved, beside the
cancellation and outside the range error-type ids come from, so no `catch` can
name a failed assertion and only the runner reads it.

**`assert` needs no `!`, and only inside a `test` block.** Both halves of that
are deliberate.

Every test framework the audience knows — Go's `t.Fatal`, Rust's `assert!`,
Jest's `expect` — ends the test on a failed assertion without annotating it,
and `docs/vision.md`'s tie-breaker says to match what a reader already expects
when the behaviour is the same. An assertion is also the one place a reader of
a test *already* looks for control leaving, and it is written at every one of
them, so the mark would be noise rather than information.

The bend is bounded by refusing `assert` anywhere else. Outside a test there is
no test to fail, and `raise` says the same thing while saying where it goes —
so the rule that control leaves only where the source marks it holds
everywhere ordinary code can reach.

## One fiber each

`khora test` compiles the program with a different entry point: instead of
calling `main`, it registers every test and hands them to the runner, which
gives each one a fiber of its own and waits for all of them.

That is phase 5's exit criterion, and it is not arbitrary. Tests are the first
thing anyone writes that is embarrassingly parallel, and **a test that only
passes when it runs alone is a test that is lying** — running them together is
how that gets found. Isolation is by construction rather than by discipline: a
fiber has its own cancellation flag, and nothing else is shared but what the
program itself shares.

A test that ends any way other than "returned" did not pass. Which way it was —
`FAILED`, `raised`, `cancelled`, `panicked` — is in the report, because it
tells the reader where to look, and not in the count, because it does not
change what to do.

## Only scalars cross into the runtime

The runner cannot call a test directly. A tagged return is a 16-byte aggregate,
and how one of those comes back is a target decision that LLVM makes for
`{ i32, i64 }` and rustc makes for a `repr(C)` struct of the same shape — on
x86-64 Windows they disagree, silently, and the tag reads as zero. Every
failing test passed. Errata 35.

So a trampoline on the generated side takes the pair apart, where both halves
of the call are LLVM's and agree by construction, and hands the runtime an
`i32` and a pointer to write through. The rule it leaves behind is worth
keeping: **only scalars and pointers cross between generated code and the
runtime.**

## Not yet

- **`bench`** parses and is dropped. It needs a clock, which is I/O.
- **Filtering, and running one test.** The runner takes no arguments yet.
- **Reporting *why* an assertion failed.** `assert` is handed a `Bool`, so
  there is nothing left to say about it by the time it fails. Saying more means
  taking the comparison apart, which is a macro-shaped problem and not one to
  solve before there are macros.

## One findable test per promise

From an outside review, and worth adopting as a standing rule rather than a
one-off audit:

> For every defining Khora promise, have one obvious test somebody can find.

The point is not coverage. It is that a claim in a document and a claim in the
test suite should be the *same* claim, so that a reader who doubts the prose can
go and read the executable version, and so that a promise cannot quietly stop
being true. `memory.md` promised no program could leak and that promise expired
unnoticed for two phases — errata 48 — precisely because no test was named after
it.

**A test earns its place here by being discriminating, not by passing.** The
identity case is the lesson: `two_modules_may_declare_one_name` asserted that
two same-named types keep their own fields, which would still pass if nominal
identity were dropped, because two records of `{ label: String }` unify fine
when they are secretly one type. The test that actually holds the promise is
`two_declarations_of_one_shape_do_not_unify`. Both are needed and only the
second is evidence.

| Promise | Test |
| --- | --- |
| The parser never loses a byte | `khora-syntax`, `formatting_never_loses_a_token` and the round-trip cases |
| Same-shaped declarations stay distinct | `khora-types/tests/identity.rs`, `two_declarations_of_one_shape_do_not_unify` |
| A mutable value cannot cross into a fiber | `khora-types/tests/vouching.rs`, and `sharing.md` |
| A scope that is cancelled still runs its finalizers | `khora-codegen-llvm/tests/regions.rs` |
| Effect requirements subtract through a handler | `khora-codegen-llvm/tests/effects.rs` |
| `map` over a uniquely-owned list allocates nothing | `khora-codegen-llvm/tests/reuse.rs`, `a_uniquely_owned_walk_allocates_nothing` |
| The formatter is idempotent | `khora-fmt/tests/format.rs` |
| HTTP works without the `Router` | `khora-codegen-llvm/tests/http_layers.rs` |
| A capability is read where nothing mentions it | `khora-perceus/tests/rc.rs`, and `effects.rs` |
| `std` type-checks for every target | `khora-types/tests/portability.rs` |
| An edit inside a body does not reach another file | `khora-hir/tests/incremental.rs` |

**Named but not yet held by a test**, and each is a gap rather than a decision:

- *`extern fn` bypassing `[permissions]` is detected rather than accidentally
  allowed.* `permissions.md` says the gate over Khora code is total and that
  `extern` goes around it. That is a documented hole, so the test to write is
  that the hole is exactly where it is said to be and no wider. Scheduled
  beside the `extern` allow-list in Phase 10.2.

### What writing the first one found

The incrementality entry above was on this list, and it is worth recording what
happened when somebody finally went to write it, because it is the argument for
the whole section.

The promise as worded here — *editing a body does not invalidate item collection
for unrelated modules* — turned out to be **trivially true and not worth
asserting**. `item_map` reads exactly one file, so another file's item
collection was never reachable from the edit. A test of that claim would have
passed on day one and proved nothing.

The claim one layer out was **false**. `Item` carries a `TextRange`, so
inserting a character into the first function of a file shifts the span of every
declaration below it; `ItemMap` compares unequal; and salsa correctly propagates
that to `module_graph` and to the `file_scope` of every importer. A diagnostic
run showed the two maps differing in nothing but spans — *equal ignoring ranges:
true* — while a one-character edit re-resolved an importing file.

`file_scope` even carried a doc comment asserting the property: "editing a
function cannot invalidate another file's scope." It had been there, wrong,
since the query was written.

Two lessons, both of which generalise:

- **A promise worth testing is one where a reasonable person could be wrong
  about the answer.** The version on this list was safe, and being safe is what
  made it useless. The version one layer out was the one nobody had checked.
- **A comment claiming an invariant is the strongest possible signal that the
  invariant is untested**, because a claim that had a test would cite it. Both
  errata 48 and this were found by reading a confident sentence and asking what
  would happen if it were false.

