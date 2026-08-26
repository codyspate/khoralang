# Security

## Reporting a vulnerability

**Use GitHub's private vulnerability reporting**, on this repository:
*Security* → *Report a vulnerability*. That opens a private thread visible only
to the maintainers, and it is the only channel this project asks you to use —
there is no address to email, deliberately.

Please do not open a public issue for something exploitable. A public issue is
the disclosure.

## What is worth reporting

Khora is a compiler, a runtime and a standard library, so "a bug" and "a
vulnerability" overlap more than they do in an application. The ones that
matter most here:

- **Memory unsafety reachable from safe Khora.** A program with no `extern fn`
  that can be made to read or write memory it does not own. The runtime's
  `unsafe` surface is inventoried in `docs/design/soundness.md`; anything that
  invalidates an argument in that document is this.
- **A data race in generated code**, including one caused by the compiler
  choosing non-atomic reference counting for a program that can reach two
  threads.
- **An escape from the permissions table.** `khora.toml` says which parts of
  the outside world a package may reach; code that reaches further without
  saying so is a vulnerability rather than a bug.
- **Anything in `packages/postgres` that lets a value become SQL.** Bound
  parameters exist so that a value cannot; a path where one does is the whole
  class this was built to close.
- **The package manager fetching or trusting something it should not** —
  `docs/design/distribution.md` describes what is checked and what is not.

## What is known and not a report

These are written down rather than hidden, and a report of one will be closed
as a duplicate of the document that already describes it:

- **`publish = true` is an intent marker, not a permission.** Anyone can set
  it; anyone can write a dependency entry by hand. It prevents an accident, not
  an adversary. `docs/design/distribution.md`.
- **There is no registry**, so there is no signing, no yanking and no
  provenance yet. A git URL is the whole of a package's identity, pinned by
  commit and by the hash of what that commit produced. Roadmap 13.21.
- **The scheduler is not covered by ThreadSanitizer.** The tool cannot see
  through a stack switch; `docs/design/soundness.md` explains what that leaves
  unchecked.
- **A trap ends the process by default.** That is the decision in
  `docs/design/traps.md`, not an oversight.

## Supported versions

Khora has not had a release. Until it does, the supported version is `main`,
and a fix lands there.

## What to expect

An acknowledgement, and then either a fix or an explanation of why the
behaviour is intended — in which case it gets written into the design document
that should have said so. This is a one-person project; the honest promise is
attention rather than a service level.
