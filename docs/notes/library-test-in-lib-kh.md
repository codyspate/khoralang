# A library with a test in `src/lib.kh` cannot be imported

Status: **live regression on `main`, caused by `de51379`.** Reverted my three
attempted fixes because each was a different guess; the choice below is yours.

## What happens

```sh
khora new csv --lib          # writes module csv + pub fn hello into src/lib.kh
# add a `test` block to src/lib.kh, which is where reference/testing shows one
```

```
2 error(s) across 22 file(s)
  --> app/src/main.kh:6
6 | pub fn main() -> Int { print(hello()); 0 }
  |                              ^^^^^
```

The consumer's `import csv::{hello}` fails and the diagnostic points at the
consumer's call site. Nothing mentions tests. A stranger has no reason to
suspect their *test* broke their *dependency*, and the v033 trial agent only
found it because it was deliberately bisecting compiler behaviour.

## Why

`de51379` fixed a real defect: a dependency's test modules were part of the
consuming build, so `import lib_test::{..}` resolved, `khora test` in a
consumer ran every dependency's suite, and `khora doc` published a page per
test module. That fix drops any dependency file that `holds_a_test`.

`khora new --lib` writes your code **and** your tests into one file. So the
file dropped is the library.

## What I tried, and why each is wrong

**1. Drop files holding a test *and nothing else*.** Keeps `lib.kh`; correct
for the scaffold. But a test module that also declares a `pub type` for its
fixtures -- which `/tmp/leak/lib/src/lib_test.kh` does -- counts as having
something reachable and comes back in. Verified: the pure-test-module case
stopped being blocked, and the consumer's `khora test` reported `1 passed`
for a test it does not own.

**2. Nothing at all (revert `de51379`).** Restores three defects, one of which
makes somebody else's failing test fail your run.

**3. Compare each module against the dependency's package name.** A package
named `csv` owns `csv` and `csv::*`; a module named `lib_test` is a sibling
nobody imports on purpose. This is the one I would build -- but it is a
decision about what a package's module namespace *means*, which is a language
question and not mine to answer quietly.

## The question for you

Does a package own exactly the module tree rooted at its name?

If yes, option 3 is mechanical: a dependency contributes files whose module is
the package name or beneath it, and anything else is the author's private
business. It also gives `khora doc` and the import resolver one rule to share,
which is probably worth more than this bug.

If no -- if a package may legitimately declare sibling modules a consumer
imports -- then test modules need a marker of their own, and the honest
version is a `#[test]`-style attribute on the module rather than a naming
convention the compiler infers.

## Meanwhile

`main` has the regression. It is strictly less bad than what it replaced (a
library that fails loudly at the consumer's build, versus dependency tests
silently joining your suite), but it is live and the diagnostic is misleading.
If you would rather not sit on it, say so and I will land option 1 as a
stopgap -- it fixes the scaffold case, which is the one a newcomer meets, and
leaves the `pub`-in-a-test-module hole documented.

Repro kept at `/tmp/reg`; the trial report is
`/general/khora-agents/runs/v033-library-publish/REPORT.md`.
