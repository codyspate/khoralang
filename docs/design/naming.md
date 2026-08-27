# Naming

Rules the standard library already follows and had written down nowhere.

`docs/design/std-surface.md` Finding 3 observed the constructor rule and said
it "belongs in a style note rather than in the compiler". This is that note.
Roadmap 14.25.

## Constructors

Four names, and the choice between them is not arbitrary:

| | | |
| --- | --- | --- |
| `new` | an empty one of something that **grows** | `Map::new`, `Dict::new`, `Vector::new`, `Router::new`, `Prompt::new` |
| `empty` | an empty one of something that **does not grow** | `Array::empty`, `Params::empty` |
| `of` | a **conversion** from something else | `Method::of`, `Request::of`, `I32::of`, `Offset::of_minutes` |
| `root` | the **outermost** one, of which there is one | `Scope::root`, `Region::root` |

Two consequences follow from the table, and they are the part a machine can
check:

- **`of` takes what it converts from.** A conversion with no argument is not a
  conversion. `of_minutes(n)`, `of_string(text)`, `of(value)`.
- **`new`, `empty` and `root` take nothing.** They name a thing there is one
  obvious version of; an argument means the name is describing something else.

The distinction between `new` and `empty` — whether the thing grows — is not
checkable and is the reason this is a note rather than only a lint.

### The one place `std` disagrees with itself

```khora
pub fn new(length: Int, fill: A) -> Array<A>;
```

`Array` does not grow, which is why `Array::empty` exists beside it; and this
`new` takes arguments, so it is not "an empty one" of anything. By the table it
is a conversion or a construction — `Array::filled(length, fill)` says what it
does.

**Not renamed here.** It is a change to a published `std` signature, which is a
decision about compatibility rather than about style. Recorded so the decision
is deliberate whenever it is made.

## What `inconsistent-constructor` checks

Only the two consequences above, and only for functions that chose one of the
four names. **Naming a function `make` or `create` reports nothing** — the lint
speaks about names that claim to follow the convention, which is why it can
default to `warn` without being presumptuous about anybody's package.

## What is not decided here

- **Whether `std` should rename `Array::new`.** See above.
- **Any other naming rule.** Method naming, module naming, the `is_`/`has_`
  question and the `to_`/`into_` question are all real and none of them has
  been observed carefully enough to write down. A rule invented here rather
  than read off the existing surface would be a rule nobody follows.
