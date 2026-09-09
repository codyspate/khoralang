# An agent built a CSV-to-JSON transformer from the public docs alone

Rules: `website/content/docs/**` and `README.md` only. No compiler source, no
`std` source. It finished in about 25 minutes and four compile-error rounds,
every one fixed from the message alone, and the result passes `check`, `fmt
--check`, `test` and `build`.

**Two of its blockers were this session's fault** and are not findings: `std/fs_native.kh`
was mid-edit while the agent was compiling, and the runtime archive was stale
behind a new symbol. What that exposed is real, though, and is listed below.

## The one to fix first

**`khora check` accepts a program `khora build` refuses**, and then says
something unrelated about it. An undetermined type parameter:

    pub fn main() -> Int { let v = decode(Raw::Absent); 0 }

    $ khora check .    checked 22 file(s): no errors
    $ khora build .
    error: `Decode::schema` has no body, so there is nothing to call. Give it
    one, or write `extern fn` if it is a C symbol to be found at link time
     --> probe/src/main.kh:6:1
      |
    6 |
      | ^

Three things at once: the message describes writing a bodiless `fn`, which the
author did not do; the span is the blank line after the end of the program; and
the two commands disagree, which the README says they cannot. What it should
say is that `decode`'s type parameter is not determined here.

## Other bad messages

- **A record type is reported as a positional constructor.** `pattern
  `Priced(_, _, _, _, _, _, _)` not covered` for a record with named fields --
  a pattern the language will not accept. The real cause was a `catch` arm
  forcing the scrutinee to `Priced` instead of `Option<Priced>`.
- **`Decimal * Decimal` says "expected `Int`, found `Decimal`"**, twice, once
  per operand, as though the author had asked for an `Int`. `*` is simply not
  defined for `Decimal`; `>` is, through `Ord`. The message should name
  `Decimal::mul`, and the operator story -- comparisons are traits, arithmetic
  is not -- is written nowhere.
- **A cascade that contradicts its own first line.** The `catch` arm binding a
  multi-type failure produces an excellent message, and then a second one
  telling the author to add `raises A` to a function whose purpose is not to
  raise it. Fixing the second first goes backwards.
- **`Env::arguments()` says `Env` has no such function** and does not say the
  spelling is `env.arguments()`. Every other "you wrote it wrong" message here
  names the fix.
- **A broken `std` is reported as the user's fault**: nineteen diagnostics
  inside `std/fs_native.kh`, footed with `19 error(s) across 22 file(s)`, which
  reads as "you broke twenty-two files". One line saying the errors are outside
  the user's package would cost nothing.

## Where the type system fought it

- **`attempt` takes one error type, so `raises A + B` is declarable and not
  reifiable.** The natural design -- a per-row `attempt` over two genuinely
  different failure kinds -- is refused, and the fix is to collapse them into
  one variant type. The type system pushed toward a worse domain model.
- **A `catch` arm must produce the operand's type**, so "record why and carry
  on" makes the *success* path return `Option<A>`: the callee's return type is
  distorted by the caller's recovery strategy. This is the shape of every batch
  program, and it is exactly what `Validated` exists to serve.
- **No `catch { why: Show => .. }`.** A function raising three types cannot log
  why it failed without an arm per constructor.

## Documentation

- **`stdlib/schema` says "the assemblers stop at five fields". They do not** --
  six and twelve both check clean. The agent designed its record around a limit
  that does not exist.
- **`Encode` output is alphabetized and nothing says so.** `Raw::to_json` goes
  through a `Map`, which sorts; `std::json` says `object` "builds an object from
  fields in declaration order" and then, two sentences later, that the map
  "intentionally forgets that order". For this program it is a visible change in
  the deliverable, found by reading the output.
- **`Raw::of_map` is the bridge from any tabular source to a schema**, is one
  line in the generated API, and appears in no cookbook entry. The Schemas page
  claims the same `Schema<A>` reads the environment, a request body and a test
  fixture, and shows only JSON.
- **`derive(Show, Eq)` is printed without its imports in every generated API
  page**, so the hundred snippets a reader copies all contradict the one
  sentence stating the rule. Same for `assert`, `attempt`, `Split`, `Iterator`,
  `Step`, `Pair`.
- **`catch` on a non-call expression is undocumented** -- every example is
  `f(x)! catch`, and the prose says "the expression", so a block is a guess.
- **`KHORA_STD` and `KHORA_RT_LIB` appear only in `scripts/package.sh`.** They
  are the escape hatch for a broken toolchain and are invisible from the docs.
- **`docs/llvm-setup.md` is named by an error message and is not on the
  website**, so somebody who installed through `install.sh` cannot act on it.

## What it praised

`derive(Decode)` with `Raw::of_map`: one declaration bought typed decoding,
per-column error paths and `Validated` accumulation with no glue, and two bad
columns in one row reported as two problems. A hand-written `impl Decode for
Region` over a newtype composed through `derive(Decode)` on the first try. The
`!` mark making the fallible call sites visible. And the honesty of the
documentation -- the limitations page, the errata culture, the README's own
retraction of every throughput figure published before September 2026.
