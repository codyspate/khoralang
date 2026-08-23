# Keyword audit

Every reserved word in Khora, checked against the tie-breaker in
`docs/vision.md` as it is now stated: **behavior first, spelling second.**

Prompted by errata entry 21, where the rule had been read as license to copy
whichever of Go, Rust or TypeScript spelled a thing most recognizably — which in
practice meant copying Rust, because Rust is the one of the three with the
closest feature set.

## The test

For each word, three questions in order:

1. **Does the construct behave the way this audience expects?** If not, that is a
   defect in the construct, and no spelling fixes it.
2. **Does the word predict that behavior?** A word that promises semantics
   Khora does not have is the wrong word *because* it is familiar.
3. **Is there a better word?** Better means more accurate, or equally accurate
   and more coherent with the rest of the language. Not merely different — a
   rename that buys nothing costs every reader who already learned the old one.

Being Rust's word is not a mark against a keyword. Being Rust's word *and
nothing else* is what question 3 is for.

## Verdicts

### Kept, because the word is already the accurate one

| Word | What it does | Why it stands |
| --- | --- | --- |
| `import`, `type`, `if`, `else`, `while`, `return`, `break`, `continue`, `true`, `false` | the obvious thing | Universal. Not Rust's in any meaningful sense. |
| `fn` | declares a function; also opens a lambda | Go abbreviates to `func`, TypeScript to `function`, Rust to `fn`. All three abbreviate the same word and the behavior is identical; the shortest wins on no other grounds than length, which is fine when nothing else separates them. |
| `match` | exhaustive, expression-valued, destructuring | `switch` would be the familiar word and would mispredict: `switch` is statement-shaped with fallthrough in both Go and TypeScript. `match` correctly signals "not that". |
| `loop` | loops forever | Clearer than `while true`, and the word says exactly what happens. |
| `module` | declares this file's module path | Spelled out, unlike Rust's `mod`. Go's `package` is equally good and no better. |
| `trait` | a nominal, explicitly implemented set of operations | `interface` is more familiar and structural in both Go and TypeScript, so it would promise automatic satisfaction. See `docs/design/typeclasses.md` §1. |
| `impl` | attaches implementations to a type | `impl Eq for Int` reads as the sentence it is. `implement` is longer and no clearer; Go has no equivalent construct to borrow from. |
| `as` | renames an import | TypeScript and Python both spell it this way. |
| `const` | a named constant at module level, and a compile-time integer type parameter | Both positions read the way Rust's do, and they cannot be confused: one is inside `<>` and the other begins a declaration. See "`const` for a module-level constant" below. |
| `Self` | the implementing type | The weakest item here: it differs from the receiver `self` by capitalization alone. Rust and Swift both do this and both audiences that have the concept read it correctly; TypeScript's `this` is used for *both* the value and the type, which is worse. No better option found. |

### Kept, because the rule is silent

`effect`, `with`, `raises`, `raise`, `catch`, `handler`, `context`, `forall`.

These name things none of Go, Rust or TypeScript has. The tie-breaker explicitly
does not apply where Khora is doing something the competitive set cannot; the
non-negotiables decide, and unfamiliarity is inherent rather than chosen.

### Examined and kept, with the trade recorded

**`let` / `let mut`.** Khora's `let` is immutable and `mut` opts in. That is
Rust's model and it *is* a behavioral difference from the other two: a
TypeScript developer expects `let x = 1; x = 2;` to work, and a Go developer
expects the same of `:=`.

The alternative that removes the surprise is TypeScript's exactly — `const` for
immutable, `let` for mutable — and it was rejected for three reasons. It trades a
TypeScript misprediction for a Rust one rather than removing it. It collides with
`const` as a type-parameter marker. And immutability-by-default is close to a
non-negotiable: it is what the functional half of the thesis rests on, and the
rule is silent where the non-negotiables decide.

What makes the residue acceptable is that the surprise is loud and immediate.
The diagnostic already names the fix:

```text
error: cannot assign to `x`, which is not declared `mut`
```

A first-five-minutes error that states its own remedy is a very different cost
from a silent misbehavior. This is the one place the language leans on a
diagnostic to carry a design decision, which is worth knowing.

## What this pass changed

### `pub` became `export` — done

Khora chose `import` over Rust's `use`, then paired it with Rust's `pub`. The
module system's verb came from TypeScript and its visibility marker from Rust,
which was incoherent in a way that showed at every declaration.

`import` and `export` are a matched pair this entire audience reads without
thinking. `pub` paired with nothing in the language and was Rust-specific
jargon. The behavior is identical either way, so this was question 3: the same
accuracy, better coherence.

This is the only rename the audit produced from the tables above; the one below
came later, from somebody reading the code rather than the list.

### `const` for a module-level constant — done

A binding at module level was spelled `let`, and it never was one. A `let` is a
place inside a body, evaluated once where it is written. A module-level binding
is a *named expression*, lowered afresh at every mention — Rust's `const` rather
than its `static`, chosen so there is no initialization order to get wrong, no
global to release at exit, and no shared state for two fibers to reach. The
implementation had said so in a comment since it was written. The surface said
`let`.

That is question 1 rather than question 3: not two spellings of one idea, but
one spelling covering two different ideas, so a reader had no way to tell which
they were looking at except by the indentation.

The obvious objection is the section above, which rejected `const` for immutable
locals. It does not apply. That proposal was `const x = 1` *inside a function*,
where it would compete with `let` and mispredict for a Rust reader; this is a
declaration position where `let` no longer appears at all. Rust makes exactly
this distinction with exactly these two words.

`const mut` is refused where it is written, because a constant is not a place:

```text
error: a `const` cannot be `mut` — it is a named expression rather than a place,
       and there is no mutable global to make it one. A value that changes and is
       reached from more than one fiber is a `Shared`
```

So is a `let` that should be a `const`, since carrying the habit in from inside
a function is the mistake worth catching by name:

```text
error: a binding at module level is a `const`, not a `let`
```

**A constant's type is inferred**, like everything else — `const alphabet =
"bcdfghjkmnpqrstvwxyz23456789";` needs no annotation, and one can be written
when it helps. Nothing else about the construct changed: `export const` exports
it, which it had not actually done before, because visibility was hard-coded to
private on the one declaration kind nobody had revisited.

### A type can now have a method without a trait — done

Not a spelling question, and the more important of the two findings. In Go,
TypeScript and Rust alike, adding a method to your own type is the ordinary
first thing a developer does and requires no abstraction. In Khora it was a
syntax error:

```khora
impl User {
  fn age(self) -> Int { match self { User::Of(a) => a } }
  fn birthday(self) -> User { User::Of(self.age() + 1) }
}
```

The only route to `user.birthday()` was to declare a trait and implement it,
which meant every private helper needed a public abstraction invented for it.
That is a behavioral surprise on a daily action, for all three audiences at
once — question 1, and the reason the audit exists.

`impl Type { .. }` with no `for` now declares a type's own methods. Rust's
shape, chosen because Khora needs `impl Trait for Type` anyway and one construct
covering both beats two. Go's receiver form (`func (u User) Name() string`) is
flatter but cannot express "these methods implement this trait" nominally, so it
would have had to coexist with the block form rather than replace it.

Three rules, each picked so a reader can predict it:

- **A type's own method wins over a trait method of the same name.** Adding a
  trait to a program must not silently change what an existing call does.
- **A type may have several impl blocks**, because splitting methods up is
  ordinary, but **one name may not be declared twice for it**, because a call
  could not say which it meant.
- **`impl Eq Int { .. }`** — the trait form with `for` left out — is caught and
  named. The inherent form makes it parse far enough that the default error
  would otherwise be a confusing `expected {`.
