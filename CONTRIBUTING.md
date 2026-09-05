# Contributing to Khora

Khora is one person's language with the door open. That is worth saying plainly
at the top, because it decides what you should expect: a change that fits gets
read and merged, a change that reshapes the language gets an argument first, and
nobody here is paid to answer within a working day.

## Before a change

**Open an issue for anything that changes what a program means.** Syntax, the
type system, an effect's semantics, a `std` signature. The compiler is small
enough that writing the code is rarely the expensive part; agreeing what it
should do is. A patch that arrives without that conversation may be right and
still be turned down, which wastes your afternoon rather than mine.

**Just send a pull request for the rest.** A bug with a test, a diagnostic that
reads badly, a documentation page that is wrong, a missing `std` method whose
shape is obvious from the ones beside it.

## Building it

You need Rust (stable) and LLVM 22.1.8. `docs/llvm-setup.md` is the whole
story, including the two things that go wrong on Windows.

```sh
git clone https://github.com/codyspate/khoralang
cd khoralang
sh scripts/setup-llvm.sh       # writes .cargo/config.toml for your machine
cargo build -p khora-cli --features llvm
```

The `llvm` feature is what turns on code generation. Without it the compiler
parses, resolves and type checks, which is enough for a great deal of the work
and much faster to build.

**The front end fits a small machine, and that is a supported way to work on
Khora.** Measured on a two-core container with 2 GB of memory: a cold
`cargo test --workspace --no-run` -- every crate, every dependency and all of
its test binaries -- took 93 seconds and peaked at 893 MB. Nothing swapped.
If you are working on the parser, the resolver, the type checker, the formatter
or the language server, you need no LLVM and no large machine, and
`sh scripts/setup-llvm.sh` is a step you can skip entirely.

The backend is heavier but not by as much as it looks: with LLVM it peaks
around 1 GB and wants roughly 4 GB to be comfortable alongside a test run.

## The gate

```sh
sh scripts/baseline.sh
```

**This is what a change has to pass, and it is not optional.** It runs the test
suite (a little over two thousand tests), the doctests, clippy, the formatter
over `std` and every corpus member, the generated standard-library reference
against its source, the packages' own tests, all four reference applications,
the build cache's byte-for-byte claim, HTTP conformance, and — on Windows —
the runtime again under Linux through WSL2.

It takes about twenty-five minutes. Run it before you push, and read its exit
status rather than its last line; a failing step in the middle scrolls away.

Two smaller loops for while you work:

```sh
cargo nextest run -p khora-types                        # one crate
cargo nextest run --features llvm -E 'test(/^fibers::/)' # one file of end-to-end tests
```

**That filter used to be `binary(fibers)`.** `khora-codegen-llvm`'s end-to-end
tests were sixty-eight executables, each linking the whole compiler and all of
LLVM; they are modules of one binary now, so a file is selected by the module
its tests are in rather than by a binary name.

## House rules

These are not style preferences; each one is a mistake that reached a commit.

- **`khora fmt` is canonical for `.kh` files** and the gate checks it. Rust
  files are hand-formatted — do not run `cargo fmt`, which rewrites two
  thousand lines it has no opinion worth having about.
- **A comment says what a reader needs now.** How a bug was found, what the
  first attempt was, what the symptom looked like: that belongs in
  `docs/errata.md` and in the commit message. Leave a line naming the invariant
  and pointing at the entry.
- **Length in proportion to surprise.** A paragraph above a three-line function
  that does what its name says is noise. The same paragraph above a line whose
  deletion silently reintroduces a heisenbug is the most valuable thing on the
  screen.
- **Every `unsafe` block carries a `# Safety` comment** naming the invariant
  that makes it sound. `docs/design/soundness.md` is where the arguments live.
- **A doc comment reaches its item.** `scripts/no-stranded-docs.sh` catches the
  case where an edit inserts something between a paragraph and the thing it
  describes, which has happened more than once.

## Commit messages

Write the *why*. The diff already says what changed; a message that repeats it
has said nothing. The convention here is a one-line summary in the imperative
or the present tense, then prose — often several paragraphs — explaining what
was believed, what turned out to be true, and what the alternative was.

`git log` is the best documentation this project has. Keep it that way.

## When you find something that was wrong

`docs/errata.md` records what was believed and turned out to be false. If your
change corrects a mistaken belief — in the code, in a comment, or in a design
document — add an entry. The section that matters is **What generalises**: the
class of mistake, so the next one is recognisable.

Errata 62 is a good model. The bug had a diagnosis written down in a commit
message and a roadmap entry, and the diagnosis was wrong; four attempts to
reproduce it from that description all passed. The entry says so.

## Review

I read everything. What I look for, roughly in order:

1. **Is it true?** Does the test fail without the change.
2. **Is the boundary right?** A fix in the checker for something the backend
   drops is worse than no fix — it turns a compile error into a silent wrong
   answer, which is errata 62's whole lesson.
3. **Does the gate pass?**
4. **Would a stranger understand why this is here in a year?**

Expect questions about the second one. They are not an objection.

## What is out of scope

- Reformatting, renaming or reorganising code without a behavioural reason.
- New `std` modules without a design document and an argument for why the
  vocabulary belongs in `std` rather than in a package. `docs/design/effect-survey.md` §3.2 is the rule.
- Performance changes without a measurement, on a stated machine, in one
  sitting. `bench/README.md` explains why a number only travels that far.

## Governance

One maintainer, final say, no committee. Language changes are decided in the
issue thread and recorded in `docs/roadmap.md` or a design document under
`docs/design/`. If that arrangement changes — because more people are doing the
work — it will be written here before it is true elsewhere.
