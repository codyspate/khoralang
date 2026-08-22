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
