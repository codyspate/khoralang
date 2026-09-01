# khq

A query language over JSON, in Khora.

```console
$ khq '.users[] | select(.age > 30) | {name, city}'
{
  "city": "London",
  "name": "Ada"
}
{
  "city": "New York",
  "name": "Grace"
}
```

Run it with no file and it uses a small sample document, so every example on
this page can be typed as it stands.

## Why it exists

Two reasons, and the second is the one that pays.

It is a working tool: `jq`'s shape, small enough to read in an afternoon.

And it is the largest Khora program written to date — about 3,600 lines across
seven modules — which makes it the first thing to put real weight on the
language rather than demonstrating a feature at a time. A lexer, a parser, a
tree, an evaluator over streams and forty builtins is the shape of a compiler,
which is what Khora says it is for.

**What it found while being written** is the part that mattered:

- A **compiler panic**: a non-ASCII character beside a `${..}` hole. The first
  line of the first version printed an accented word. Errata 67.
- **`Float::of_string` did not exist.** `Float` was the only primitive with no
  way in from text.
- **`String::chars_between` did not exist**, so a caret could not be placed
  under a span in text containing anything but ASCII.
- **`String::next_boundary` invites an infinite loop.** It answers the boundary
  *at or after* an offset, so stepping with it never advances. Its
  documentation now shows the loop that does not terminate.
- **`List::sort_by` takes a comparator with no effect row**, so a comparator
  that has to run something cannot be passed to it — every other combinator
  taking a closure grew one, and that one did not.
- **Two modules cannot import each other**, which for an evaluator and its
  builtins is a real constraint. The evaluator is handed to the builtins as a
  value; `eval.kh`'s `Call` arm says so.

## The language

A filter takes one value and produces a **stream** of values, which may be
empty. Everything else follows from that.

```
.                     the value itself
.name  ."with space"  a field; `null` if it is missing
.[0]   .[-1]          an element, counted from the end if negative
.[]                   every element of an array, or value of an object
.[1:3] .[2:] .[:2]    a slice of an array or a string
a | b                 b, applied to every value a produced
a, b                  everything a produced, then everything b produced
[ f ]                 everything f produced, gathered into an array
{ a: f, b }           an object; `{ b }` is short for `{ b: .b }`
f?                    f's values, or none if f failed
a // b                a's true values, or b's if it had none
```

Operators, loosest first: `//`, `or`, `and`, comparison, `+ -`, `* / %`.
Comparison does not chain. `if c then a else b end` is the conditional.

`+` adds numbers, joins strings, concatenates arrays and merges objects with
the right side winning. `null + x` is `x`.

**Only `false` and `null` are false.** `0`, `""`, `[]` and `{}` are all true,
so `select(.count)` does not silently drop the zeroes.

### Functions

```
length keys values type add empty not sort unique reverse first last min max
flatten to_entries from_entries tostring tonumber floor abs paths
ascii_downcase ascii_upcase

select(f) map(f) has(k) contains(v) sort_by(f) group_by(f) any(f) all(f)
join(s) split(s) startswith(s) endswith(s) error(m) range(n) del(k)
with_entries(f)
```

A name that is not one of these is reported with the nearest one that is, which
is what `lenght` is for.

### Numbers

**Two integers are combined as integers; anything else goes through a float.**

```console
$ khq '9007199254740993 + 0'
9007199254740993
$ khq '0.1 + 0.2'
0.30000000000000004
$ khq '8 / 4'
2
$ khq '10 / 4'
2.5
```

The first line is why `std::json` keeps a number's own text rather than parsing
it to a `Float`: through a double it is `9007199254740992`. The second is what
binary floating point is, and saying so is better than a tool that rounds and
hopes.

### Errors

A query that cannot be carried out says where:

```console
$ khq '.users[0].name | lenght'
error: `lenght` is not a function; did you mean `length`?
  |
1 | .users[0].name | lenght
  |                  ^^^^^^
```

The caret is placed by counting **characters**, so it lands in the right column
in a query containing anything but ASCII. That is what `String::chars_between`
is for and why it now exists.

## What it does not do

No variables (`$x`), no `def` for user-defined functions, no `reduce` or
`foreach`, no paths as values (`getpath`, `setpath`), no assignment (`.a = 1`),
no regular expressions.

Each of those is absent because it would add machinery rather than exercise
more of the language. One more is absent for a different reason: a document is
read whole, because `std::json::parse` takes a `String`, so this cannot stream
a file larger than memory.

## Running it

```console
$ khora build .
$ ./build/khq.exe '<filter>' [file] [-c] [-r] [--explain]
```

`-c` writes one line per value, for piping. `-r` writes a string without its
quotes, which is what a shell script wants. `--explain` parses and stops.

`khora test .` runs the thirty-four tests in `src/khq_test.kh`. Half of them
are refusals: a query language's failure mode is producing nothing and looking
like it worked.

## The modules

| file | what it holds |
| --- | --- |
| `diag.kh` | spans, problems, and the caret |
| `lex.kh` | the query text as tokens |
| `ast.kh` | what a query is, once read |
| `parse.kh` | tokens to a tree, by precedence climbing |
| `value.kh` | order, equality and arithmetic over `Json` |
| `eval.kh` | a filter over a value, as streams |
| `builtin.kh` | the functions a query can call |
| `render.kh` | values, written out |
| `main.kh` | the command line |
