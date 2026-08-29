---
title: Grammar and precedence
sidebar:
  order: 12
---

This page summarizes the concrete grammar rules that most often matter when reading or writing Khora. Individual reference pages show the complete examples for each construct.

## Declaration starts

A source file is a sequence of declarations. Common forms are:

```text
module Path ;
import Path::{...} ;
import Path::* ;

pub? const Pattern (: Type)? = Expr ;
derive(...)? pub? type Name<TypeParams>? (= TypeDef)? ;
pub? fn Name<TypeParams>? (Params) (-> Type)? EffectClause* (Block | ;) 
pub? effect Name<TypeParams>? { Field, ... }
pub? context Name { name: Expr, ... }
pub? trait Name<TypeParams>? (: Bounds)? { TraitItem* }
impl<TypeParams>? Type (for Type)? { TraitItem* }

test "name" Block
bench "name" Block
extern fn name(Params) (-> Type)? EffectClause* ;
```

Current public visibility is spelled `pub`.

## Paths and fields

Compile-time path:

```khora
app::model::User
Result::Ok
```

Runtime projection:

```khora
user.name
response.status
```

`::` and `.` are intentionally separate tokens with separate roles.

## Type grammar shapes

Named/generic type:

```khora
User
List<User>
```

Tuple and unit:

```khora
()
(Int, String)
```

Record:

```khora
{ id: Int, name: String }
```

Record update, in expression position — the base first, then the fields that
replace its own:

```khora
{ ..base }
{ ..base, id: 7 }
{ ..base, id: 7, name: "Grace" }
```

Open row:

```khora
{ db: Db | 'ef }
```

Function type:

```khora
Request -> Response
  with { db: Db }
  raises DbError + HttpError
```

Explicit polymorphism:

```khora
forall<A>. A -> A
```

Variant type:

```khora
| None
| Some(value: A)
```

## Generic parameter grammar

Ordinary parameter:

```khora
<A>
```

Bound:

```khora
<A: Eq + Show>
```

Const parameter:

```khora
<const N: Int>
```

Row parameter:

```khora
<'ef>
```

Variance:

```khora
<+A>
<-A>
```

These forms may be mixed in one parameter list.

## Primary expression forms

```khora
42
"text"
path::name
{}
(a, b)
[a, b]
{ let x = 1; x + 1 }
fn x => x + 1
if condition { a } else { b }
match value { Pattern => Expr, }
while condition { body; }
for pattern in iterable { body; }
loop { body; }
raise error
handler for Effect { operation: fn () => value }
with Context { body; }
break
continue
return value
```

## Postfix forms

Starting from an expression, postfix forms bind tightly and may chain:

```khora
f(a, b)
value.field
fallible()!
fallible()! catch { Error::Case => recover(), }
operation() with { service: handler }
operation() with Production
```

Calls, field projection, postfix failure propagation, `catch`, and postfix `with` bind more tightly than binary operators.

## Lambda grammar

Single parameter:

```khora
fn value => expression
```

Ignored parameter:

```khora
fn _ => expression
```

Parameter list:

```khora
fn (left: Int, right: Int) => left + right
```

Block body:

```khora
fn value => {
  let next = value + 1;
  next * 2
}
```

## Pipeline grammar

First argument:

```khora
value |> f(a)
```

Explicit insertion point:

```khora
value |> f(a, _, b)
```

A stage may contain one `_` placeholder.

Flow lambda:

```khora
||> normalize
|> validate!
|> persist!
```

`||>` starts a unary lambda; following `|>` stages belong to that flow expression until grouping ends.

## Operator precedence

From loosest to tightest:

1. `=` — assignment, right-associative
2. `|>` — pipeline, left-associative
3. `||` — boolean OR
4. `&&` — boolean AND
5. `== != < > <= >=` — comparisons
6. `+ -`
7. `* / %`
8. prefix `- !`
9. postfix call, `.`, failure `!`, `catch`, and `with`

For example:

```khora
x = value + 1 |> double
```

parses as assigning the result of the full pipeline to `x`.

## Blocks versus record literals

These braces are a record because they begin with fields:

```khora
{ id: 1, name: "Ada" }
```

These braces are a block:

```khora
{
  let id = 1;
  id + 1
}
```

An empty `{}` is an empty record literal. In control-flow forms such as `match`, `if`, `while`, `for`, and `loop`, the grammar already knows when braces introduce the body or arm list.

## Match arms

```text
Pattern (if Expr)? => Expr ,?
```

Examples:

```khora
Result::Ok(value) => value,
value if value > 0 => value,
_ => 0,
```

## Pattern shapes

```text
_
Literal
identifier
Path
Path(Pattern, ...)
Path { field, field: Pattern, ... }
(Pattern, ...)
```

See [Patterns](./patterns/) for binding and exhaustiveness rules.

## Numeric literals

```khora
1_000
0.25
6.02e23
19.99d
```

The `d` suffix creates an exact `Decimal`; a fractional literal without it is `Float`.

## Semicolons

A semicolon terminates declarations such as `const`, `type`, imports, and signature-only functions, and turns an expression inside a block into a statement:

```khora
let value = compute();
log(value);
value
```

The last `value` is the block result because it has no semicolon. A function definition ends with its closing `}` rather than `};`.