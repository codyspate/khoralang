---
title: Expressions
sidebar:
  order: 4
---

Khora is expression-oriented. Literals, calls, blocks, conditionals, matches, pipelines, lambdas, handlers, and control-flow forms all appear in expression position according to their type.

## Literals

```khora
42
1_000_000
3.14
6.02e23
19.99d
true
false
"hello"
`multiline text`
```

See [Lexical structure](./lexical-structure/) for exact literal and interpolation forms.

## Paths and names

```khora
value
app::model::User
Result::Ok
```

`::` is compile-time namespacing for modules, types, constructors, and associated items.

## Calls

```khora
parse(input)
connect(host, port)
```

Arguments are positional and may have a trailing comma.

## Runtime field projection and methods

```khora
user.name
response.status
user.display_name()
```

`.` operates on a runtime value. It is distinct from `::` path lookup.

## Record literals

```khora
{
  id: 42,
  name: "Ada",
}
```

An empty record is:

```khora
{}
```

A braced form beginning with `name:` is a record literal; an ordinary braced sequence is a block.

## Record update

`{ ..base, field: value }` builds a new record from an existing one. Every
field not named comes from `base`:

```khora
let renamed = { ..user, name: "Grace" };
```

It is a **new record**: `user` is unchanged. What the syntax saves is writing
out the fields that do not change, which starts to matter at more than a few:

```khora
fn applied(counts: Counts, event: Event) -> Counts {
  match event {
    Event::Created => { ..counts, created: counts.created + 1 },
    Event::Deleted => { ..counts, deleted: counts.deleted + 1 },
    Event::Expired => { ..counts, expired: counts.expired + 1 },
  }
}
```

The base comes first and appears once. A field named twice is an error rather
than last-one-wins, so is a field the base's type does not have, and so is a
base that is not a record. `{ ..base }` with nothing after it is `base`.

A [`mut` field](./types/#record-types) is the other way to do this. The update
produces a new value; assigning a `mut` field changes the one already held.
Reach for the update where the old value still matters, and for `mut` where it
does not.

## Tuple and unit expressions

```khora
()
(1, "one")
(1,)
```

Parentheses without a comma group an expression:

```khora
(value + 1)
```

## List literals

```khora
[]
[1, 2, 3]
[
  "Ada",
  "Grace",
]
```

## Integer division and remainder

`/` **truncates toward zero** and `%` **takes the sign of the dividend**, which is what C, Rust, Go and the hardware instruction all do:

```khora
(0 - 7) / 2       // -3, not -4
7 / (0 - 2)       // -3
(0 - 7) / (0 - 2) //  3

(0 - 7) % 2       // -1
7 % (0 - 2)       //  1
(0 - 7) % (0 - 2) // -1
```

The two agree, so `a == (a / b) * b + (a % b)` holds for every pair that does not trap. `Float::to_int` truncates toward zero for the same reason, and says so.

Both trap on a zero divisor rather than answering anything — see [Traps](/docs/reference/traps/). For a quotient that rounds rather than truncating, do the rounding in `Decimal`, where the mode is a parameter and not a convention.

## Blocks

```khora
{
  let subtotal = 40;
  let tax = 2;
  subtotal + tax
}
```

The final expression without a semicolon is the block value. Statements before it are evaluated in order.

## Local bindings

```khora
let value = compute();
let value: Int = compute();
let mut count = 0;
let (left, right) = pair;
```

General form:

```text
let mut? Pattern (: Type)? = Expr ;
```

A binding is immutable unless it says `mut`, and a plain `let` cannot be
assigned to later. `mut` is fiber-local mutation only: state that several
fibers evolve is a `Shared` boundary instead, and
[Sharing](./sharing/) says why.

`let` is local. Module-level named expressions use `const`; see [Declarations](./declarations/#constants).

## Assignment

```khora
count = count + 1
```

Assignment is an expression of type `()` and is right-associative. The target must be writable, such as a `let mut` binding or a mutable record field.

## Lambdas

Single parameter:

```khora
fn value => value * 2
```

Ignored parameter:

```khora
fn _ => fixed_value
```

Several or annotated parameters:

```khora
fn (left: Int, right: Int) => left + right
```

Block body:

```khora
fn value => {
  let doubled = value * 2;
  doubled + 1
}
```

## Pipeline `|>`

First-argument insertion:

```khora
value |> transform(a, b)
```

is equivalent to:

```khora
transform(value, a, b)
```

One `_` placeholder selects another argument position:

```khora
value |> transform(a, _, b)
```

A stage may contain at most one placeholder.

Bare unary function:

```khora
value |> normalize |> validate
```

Fallible stage:

```khora
value |> parse! |> validate(config)!
```

A pipeline introduces no second error model: `!` still marks the exact call
where a typed failure may leave the function, and `catch` still applies either
to one stage or to the parenthesised pipeline as a whole.

```khora
let user = (raw |> parse! |> validate!) catch {
  ParseError::Invalid(message) => User::invalid(message),
};
```

## Flow lambda `||>`

`||>` starts a unary anonymous pipeline:

```khora
||> normalize
|> validate!
|> persist!
```

For example:

```khora
items |> List::map(
  ||> normalize
  |> validate!
)
```

It is equivalent in shape to `fn value => value |> ...`, and infers its
effects, failures and captures the same way.

Following `|>` stages belong to the flow lambda until grouping ends, so piping
the function value itself takes parentheses:

```khora
(||> normalize) |> apply_twice
```

`||>` is always unary. An anonymous function of several parameters is `fn`.
A named function needs neither: `items |> List::map(normalize)`.

Reach for a pipeline when the value moving through the expression is the thing
to follow, and for an ordinary call when the operation itself is the point of
the line.

## Operators and precedence

From loosest to tightest:

1. assignment `=` — right-associative
2. pipeline `|>` — left-associative
3. logical OR `||`
4. logical AND `&&`
5. comparisons `== != < > <= >=`
6. addition/subtraction `+ -` — `+` also joins two `String`s, which is the only
   concatenation operator there is; there is no `++`
7. multiplication/division/remainder `* / %`
8. prefix negation `-` and boolean not `!`
9. postfix call, field access, failure `!`, `catch`, and `with`

Examples:

```khora
value + 1 |> double
ready && count > 0
!enabled
-total
```

Prefix `!value` is boolean negation. Postfix `call()!` marks failure propagation; position disambiguates them.

## Failure postfix `!`

```khora
load_user(id)!
```

The inner call keeps its normal value type while its declared failure row is allowed to leave the current computation. See [Failures](./failures/).

## `catch`

```khora
load_user(id)! catch {
  UserError::NotFound(_) => User::guest(),
  UserError::Unavailable(reason) => User::offline(reason),
}
```

`catch` is postfix on the expression whose typed failures it handles.

## Postfix capability installation

```khora
load_user(id)! with {
  store: test_store,
}
```

A named context can be supplied the same way:

```khora
load_user(id)! with Production
```

with overrides:

```khora
load_user(id)! with Production {
  store: test_store,
}
```

See [Capabilities](./capabilities/).

## Handler expressions

```khora
handler for Clock {
  now: fn () => fixed_instant,
}
```

A handler expression produces a value implementing the named effect.

## `with` blocks

```khora
with {
  clock: fixed_clock,
  store: test_store,
} {
  run_job()!
}
```

Named context:

```khora
with Production {
  run_server()!
}
```

## Control-flow expressions

The following forms are expressions or block-like expressions and have dedicated rules in [Control flow](./control-flow/):

```khora
if condition { a } else { b }
match value { Pattern => result, }
while condition { body; }
for pattern in iterable { body; }
loop { body; }
break
break value
continue
return
return value
raise error
```

Patterns used by `match`, `for`, destructuring `let`, and `catch` are listed in [Patterns](./patterns/).