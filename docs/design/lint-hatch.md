# Saying "I meant this one"

`[lints]` sets a level per lint per package, and that is the only dial there
is. There is no way to say at a line that this particular case is deliberate.

Roadmap 14.22–14.27 add six lints — `unused-import`, `unused-binding`,
`unreachable-code`, `inconsistent-constructor`, `undocumented-export`, and a
sharpened `unused-capability`. The roadmap is explicit that the escape hatch
lands first, and why:

> Add six more lints to that and the predictable outcome is a `khora.toml` that
> turns half of them off, which is worse than not having them: a lint people
> switch off wholesale stops being evidence about anything.

**Decided: the checked pragma, spelled `// @klint allow <lint>`.** The rest of
this document is the argument that led there, kept because the reasoning is
what makes the answer reviewable. What changed from the recommendation is the
spelling — see "The spelling, and why it is loud" at the end.

## What has to be true of the answer

Four things, in the order they constrain the design.

**It must be checkable.** A suppression that silently fails to suppress — a
misspelled lint name, a pragma attached to the wrong thing — is worse than no
suppression, because the reader believes the line is handled. This repository
has spent recent work in exactly this direction: an unknown manifest key warns,
a removed one explains itself, a cache that misses says why. A hatch that fails
quietly would be the one place that does not.

**It must be narrow.** Suppression that covers a block covers things nobody
meant. Whatever the mechanism, it should apply to *one* statement or item.

**It must not cost a language feature the language did not want.** The hatch is
a small question. Answering it with a general extension point means the
language now has one, and everything that follows from that is a consequence of
a lint.

**It must be visible where it applies.** Someone reading the line should see
that the lint was considered, without going to the manifest.

## Option A — attributes

```khora
@allow(discarded-result)
save_to_disk(record);
```

**Attributes do not exist in Khora.** This means new syntax, a new CST node, a
rule for every position one may appear in, and a permanent feature.

*For:* familiar from Rust, Java and C#. Part of the tree, so it is checkable
for free, the formatter knows what it is, and an editor can offer it. Nothing
about it is a special case.

*Against:* it is a large answer to a small question, and it does not stay
small. The moment attributes exist, the next feature that wants metadata —
serialization, test parameters, deprecation, ABI hints — has an obvious place
to go, and the language acquires an extension point as a side effect of lint
suppression rather than by deciding to have one. That is a real cost and it is
paid later, by someone else, which is the kind of cost worth being suspicious
of.

## Option B — a checked comment pragma

```khora
// @klint allow discarded-result
save_to_disk(record);

save_to_disk(record); // @klint allow discarded-result
```

The lexer already emits `LINE_COMMENT` and `BLOCK_COMMENT` tokens and the CST
is lossless, so **this needs no grammar change at all** — the tokens are
already there to read.

*For:* costs the language nothing. Narrow by construction, since a comment
attaches to a line. Cheap enough to ship before the six lints rather than
after.

*Against:* a magic comment is a second, weaker syntax. The formatter has to
know not to move it. It is not part of the tree the type checker walks, so
every consumer that cares has to go and look for it.

**The usual objection does not apply, because this one is checked.** A pragma
naming a lint that does not exist is itself reported, against `LINTS`, which is
already a list of every name for exactly this purpose. That removes the failure
mode — silent non-suppression — that makes magic comments bad.

Precedent for the checked form: mypy's `# type: ignore[code]` and
golangci-lint's `//nolint:name`, both of which validate the name.

## Option C — no hatch, per-package only

The Go position: refuse the mechanism, keep the lints good enough that nobody
needs it.

*Against:* Go lost this argument in practice — `golangci-lint` added `//nolint`
because people needed it. And the roadmap already names the failure mode: six
lints with no line-level hatch produce a manifest that disables three of them,
which is a worse outcome than a pragma, because turning a lint off in the
manifest is invisible at every line it would have fired on.

## Option D — express it in the language, per lint

`discarded-result` already has one: `let _ = f();` says "deliberately dropped".
The roadmap notes this works "by luck rather than by design".

*Against:* it does not generalise. There is no natural expression for "this
capability is genuinely unused" or "this cycle is intended", and inventing one
per lint is six small language decisions instead of one.

## Decided: B, the checked comment pragma

Because the hatch has to exist before the six lints, and **attributes deserve
to be designed as attributes** — by someone deciding the language should have
metadata, weighing what else would use it, not as the thing that had to happen
before `unused-import` could ship. Introducing a general extension point as a
side effect of a lint is how languages end up with features nobody chose.

The pragma is small, checkable, and reversible: comments are mechanically
findable, so if attributes arrive later for their own reasons, rewriting every
pragma is a script rather than a migration.

**The honest counter-argument, recorded because it was close:** Khora's
character is to make things the compiler understands rather than things it
reads out of comments, and this is the one place that is not true. Someone who
weighed that above the cost of introducing attributes would have picked A and
would not have been wrong. What tipped it is that the pragma is *checked*, so
the usual price of a magic comment is not paid, and that it is reversible by
script if attributes ever arrive on their own terms.

## The spelling, and why it is loud

`// @klint allow <lint-name>`.

The recommendation here was `// khora: allow ...`, and it was wrong in a way
worth recording: it reads like an aside. A directive that changes what the
compiler reports about a line must not look like ordinary prose sitting beside
the code, because the failure mode is somebody's eye sliding past it while
reviewing exactly the line it excuses.

`@klint` is deliberately conspicuous. It is greppable — one string finds every
suppression in a repository — and it does not look like an attribute, which
would raise the question of why it is not one.

## The rest of the shape

**Attachment: the statement the comment precedes, or the one it trails.** Both
forms above mean the same thing. One statement, never a block — a block-level
allow is how a suppression grows to cover cases nobody looked at.

**An unknown lint name is reported.** As a lint of its own, `unknown-allow`,
defaulting to `warn`. It is checked against `khora_lint::LINTS`, which exists
so that a manifest naming a lint that does not exist can be told what does; the
pragma gets the same treatment for the same reason.

**A pragma that suppressed nothing is reported** — `useless-allow` — but
**defaulting to `allow`**, off. A stale suppression is real debt and worth
finding eventually; making it fire while six new lints are still settling would
mean churn in exactly the files people are already editing to satisfy them.
Turn it on once the six have stopped moving.

## What is not decided here

- **Whether attributes should exist**, for their own reasons. B does not
  foreclose it and does not argue against it.
- **A file-level or module-level allow.** Neither is in this proposal; the
  manifest already covers per-package, and everything between is a bigger
  surface than the problem.
- **Whether `let _ =` keeps working for `discarded-result`.** It should, and it
  is orthogonal: an expression that says what it means is better than a pragma
  either way.
