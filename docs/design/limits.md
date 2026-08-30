# Limits

What the compiler will not accept, why the number is what it is, and what it
would take to change it. Every entry here is a number rather than a rule: a
program is not wrong for exceeding one, it is only bigger than something we
built.

## The compiler's own stack

**The limit.** A single expression may be about twenty thousand levels deep.
Past that the process dies with

```text
thread 'khora' (45396) has overflowed its stack
```

and nothing else: no file, no line, no note, and an exit status (127 on
Windows) that says only that something went wrong.

**Where the depth comes from.** Mostly not from nesting anybody wrote. A list
literal desugars right-nested — `[1, 2, 3]` is `Cons(1, Cons(2, Cons(3, Nil)))`
— so *n* elements on one line is a tree *n* deep. A chain of `+` is left-nested
and does the same. Every pass that walks an expression tree recurses once per
level: parsing, name resolution, inference, the reference-counting plan, code
generation.

**Why it is a stack size and not a rewrite.** `khora` runs its work on a
thread it spawns with a 512 MB stack (`COMPILER_STACK` in `khora-cli`'s
`main.rs`), because `main`'s own stack is fixed by the loader before any code
of ours runs and cannot be asked for. rustc, clang and swiftc all do exactly
this, for exactly this reason. The stack is reserved address space, committed
page by page as it is touched, so a one-line program pays nothing for it.

Measured on a list literal, debug build: one megabyte held sixty-nine elements,
sixty-four megabytes held five hundred but not five thousand, and half a
gigabyte holds twenty thousand but not sixty thousand.

**What it would take to remove.** Making every walk iterative — an explicit
worklist instead of the call stack, in a dozen passes across five crates,
including the ones where the recursion is the clearest way to write the pass.
That is a real cost paid against a limit that hand-written code does not reach,
so it has not been paid. Two cheaper improvements are worth doing before it:

- A depth counter in the parser that reports `this expression nests too
  deeply` with a span, so the failure is a diagnostic rather than a dead
  process. This bounds *parsed* depth, which bounds every later pass.
- Desugaring a list literal to an iterative build rather than a right-nested
  `Cons` chain, which removes the single largest source of accidental depth.

If a generated file hits this, the fix available today is to break the literal
into pieces and join them, or to raise `COMPILER_STACK`.

## The instantiation depth of a generic

Monomorphisation gives up after 64 nested instantiations
(`MAX_DEPTH`, `khora-types`'s `mono.rs`) and reports it. Unlike the stack
limit, this one is a diagnostic with a span, and it is a limit on purpose:
a generic that instantiates itself at a larger type has no fixed point, and
without the cap the compiler would expand for ever rather than say so.
