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

### `pub` became `export`, and then became `pub` again

**Both directions were right when they were taken**, and what changed in
between is the construct rather than anybody's taste. Question 1 before
question 3, exactly as the test above says.

The audit's reasoning was coherence, and it was good. Khora chose `import` over
Rust's `use` and then paired it with Rust's `pub`: the module system's verb from
TypeScript and its visibility marker from Rust, which showed at every
declaration. `import` and `export` are a matched pair this audience reads
without thinking; `pub` paired with nothing. Same accuracy, better coherence —
question 3, and the only rename the tables produced.

**Then 13.11 gave the keyword a second place to appear.** `export` inside an
`impl` had been parsed and read by nothing; making it mean something put the
word on methods, where the pairing argument does not reach:

```khora
impl Map<K, V> {
  pub fn get(self, key: K) -> Option<V> { .. }
  fn slot(self, key: K) -> Int { .. }
}
```

Nobody *imports* `Map::get`. It is reached by having a `Map`, which is why
`import_inherent` brings a type's methods across whether or not the type was
named in an import. The symmetry `export` was chosen for is a fact about
module-level declarations, and a method is not one of those in the way the
argument needed — `import`/`export` still reads well over a `type`, and reads
like a category error over a method on it.

Two smaller things, both question 3's "equally accurate and more coherent":

- **Frequency.** The word went from 46 occurrences in `std` to 287 in one
  commit, because 241 methods needed it. A three-letter word at that density is
  a different proposition from a six-letter one at the old density.
- **The other half is unchanged.** `import` did not need a partner to make
  sense; it names an action a file takes. `pub` names a property a declaration
  has. They were never doing the same kind of work, which is the part the
  original argument slightly overstated.

**What this cost.** 556 keywords in `.kh` files, 903 in Khora written inside
Rust test fixtures, 422 in prose, one snapshot and one syntax-highlighting
pattern — mechanical, and done in one commit precisely so that no branch has to
be rebased across a half-finished rename. Nothing about visibility changed;
this is spelling only.

**The old word is a diagnostic, not a silence.** `export` is an ordinary
identifier now, so a file written before the rename would otherwise get
"expected a declaration" — true, unhelpful, and pointing at the one place a
reader with older Khora in front of them will not think to look:

```text
error: `export` is spelled `pub`
 --> src/main.kh:3:1
  |
3 | export fn f() -> Int { 1 }
  | ^^^^^^
```

It recovers, so a file with fifty of them reports fifty renames rather than one
rename and forty-nine cascading confusions. This is the second time the audit
has leaned on a diagnostic to carry a decision — `let` at module level is the
first — and it is the same argument both times.

**And what it should have cost.** `docs/design/compatibility.md` says a change
that breaks source gets a migration note, and that if the migration is
mechanical it gets an *edition* instead, with the edition machinery landing
"with the first change that needs it". This is a mechanical source break and it
did not get an edition, because there is no Khora outside this repository to
migrate: no registry, no release, and 13.19's external alpha not started. The
policy is about protecting code somebody else wrote, and there is none. The
first change that needs the machinery will be the first one that lands after
somebody outside has written a program.

The rename below came later, from somebody reading the code rather than the
list.

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
when it helps. Nothing else about the construct changed: `pub const` exports
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
