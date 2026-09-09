---
title: Coming to Khora
sidebar:
  order: 0
---

These notes map familiar concepts from other ecosystems onto Khora. They assume
you have read enough of the [Language Reference](/docs/reference/) to recognise
the syntax — [Expressions](/docs/reference/expressions/),
[Failures](/docs/reference/failures/) and
[Capabilities](/docs/reference/capabilities/) are the three that carry most of
the difference — and they spend their space on the places where a habit from
your last language is the thing that will trip you.

They are not syntax cheat sheets. The goal is to explain where a familiar
concept has a direct analogue, where Khora deliberately chooses a different
abstraction, and which habits should not be carried over unchanged.

- [From TypeScript + Effect](/docs/migration/from-typescript-effect/) — the
  closest fit in intent, and the one where the vocabulary maps almost term for
  term: `Effect<A, E, R>` is a return type, a `raises` row and a `with` row.
- [From Go](/docs/migration/from-go/) — the closest fit in *deployment*: one
  native binary, no VM. The differences are that failure is in the type rather
  than in a second return value, and that a fiber has an owner.
- [From Rust](/docs/migration/from-rust/) — native and statically typed
  without lifetimes or a borrow checker, because Perceus reference counting
  answers the same question at run time.

Three, and not more. A comparison page is only worth reading if its code is
real, and every hand-written Khora block on these pages goes through the same
gate as the Reference's — `scripts/check-docs.sh` parses all of them against
this compiler, and checks the ones that declare their own `module`.
