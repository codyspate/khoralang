# An agent built a CLI log analyser from the public docs alone

Rules: `website/content/docs/**` and `README.md` only. No compiler source, no
`std` source. It finished — two `khora check` cycles — and the program is
correct, including under `[permissions] default = "deny"` with an `*.log` glob.

## Real defects, in priority order

**1. `fold_lines` on an unreadable file reports `NotFound`.** `chmod 000` then
read: `IoError::NotFound`. `stdlib/api/fs.md` sells the three-way split as
"'it was not there' against 'it did not work'", and `Denied` exists — a
permission bit gets the wrong one, so a program following the documentation
prints a misleading message. `fopen` returns NULL for both; either `errno` is
consulted or the contract is rewritten.

**2. `fold_lines` on a directory silently succeeds with zero lines.** Exit 0,
`lines: 0`. On Linux `fopen` on a directory succeeds and the read fails
`EISDIR`; the module doc says "a short read means something went wrong", so
this path is meant to be covered.

**3. An arithmetic error is invented on an operand whose type is already an
error.** After a real error made `one.head`'s type unknown:

    error: arithmetic: expected `Int`, found `String`
    63 |       Option::Some(two) => one.head + " " + two.head,
       |                                       ^^^

The line is correct — `+` concatenates strings. Inference defaulted `+` to
arithmetic and put the caret under the string literal. The agent briefly
believed `+` did not concatenate. Suppress arithmetic errors when an operand's
type came from an error.

**4. A spurious `!` suggestion when an argument error leaves a row free.**

    error: this argument: expected 1 argument(s), found 2
    error: `List::map` can leave this function, so the call needs `!`

The second is false — `List::map` with a pure function needs no `!`. `Functor::map`
carries `raises 'er`; the argument failed to unify, the row stayed free, and the
compiler concluded the call can fail. Following the advice produces a new error.
Suppress row-unresolved errors when an argument in the same call already failed.

**5. "expected 1 argument(s), found 2" blames the call, not the callback.** It
is the *function passed here* that takes two parameters. Say that.

## Documentation

- **You must import a record type to read its fields, and no page says so.**
  `String::split_once` returns a `Split`; the caller never writes the name, so
  nothing prompts the import, and six errors follow. The rule is written down
  once, in `reference/lexical-structure.md`'s interpolation section, as a fact
  about `Show`. It belongs in `reference/modules-and-packages.md` as a rule, and
  in every stdlib signature returning a record the caller never names.
- **`reference/patterns.md`'s `for` example has no `Step`/`Iterator` import**, so
  the first `for` a newcomer copies does not compile. The rule *is* in
  `reference/control-flow.md`; the example contradicts it.
- **`List::sort`'s doc comment says `sort_by` "does not exist yet"**; it is
  documented 220 lines below in the same file and works. `khora doc --check`
  compares pages to source, not source to reality.
- **`reference/failures.md`'s "a failure that reaches `main`" example writes
  errors to stdout with `print`** — the exact bug `std::log`'s module doc opens
  by saying `eprint` exists to fix. Use `eprint` there, and mention it in
  `getting-started/first-project.md`.
- **`std::core` has no function index, and `List` has no `map`.** `map` is on
  `Functor`, documented as `fn map<A, B>(self: Self<A>, ..)`. The reader has to
  infer that `List::map(xs, f)` is callable and that `Functor`, unlike
  `Iterator`, need not be imported. `stdlib/index.md` should carry a table of
  which operations live on the type, which come from a trait, and which traits
  must be in scope.
- **`Dict::update`'s doc says "counting things into a map is the most common
  thing a log analyser does"** and `stdlib/index.md` does not say whether to
  reach for `Dict` or `Map` when folding.
- **No example of reading a positional argument.** `stdlib/api/env.md` only
  shows `variable_or`. Three lines would cover the first thing anyone writes.
- **No `Ordering::reverse(Int::cmp(a, b))` idiom shown**, so the agent
  hand-wrote a five-branch comparator.

## Formatter

A record literal as the sole argument of a call gets a four-space-relative field
indent and a closing brace indented past the fields' parent, and `khora fmt`
then calls it formatted.

## What worked

`Dict::update` with the documented counting motivation; `fold_lines` handling
`\r\n`, the unterminated final line, and streaming; the `split_whitespace` vs
`split(" ")` explanation using a log line; exhaustive `catch` over `IoError`;
the capability row making `run()`'s authority visible in its signature;
`main` refusing a `with` clause with the reason given; permissions matching the
manifest reference exactly, `Denied` included. Three error messages were quoted
as telling the author exactly what to type.
