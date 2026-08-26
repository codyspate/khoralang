---
title: Collections and strings
sidebar:
  order: 13
---

Khora's standard library provides persistent-style collection APIs and Unicode strings while the compiler/runtime optimize ownership and reuse where they can prove it is safe.

The programmer model stays functional: transforming a collection produces the next value. You do not write ownership annotations merely to let the compiler reuse storage.

## Strings

Strings are text values, not byte arrays with a text-shaped API. String interpolation is available for ordinary formatting, while structured encoders such as JSON should be used for machine-readable output.

## Multiline strings

A quoted literal is one line. For text whose shape is worth keeping — embedded SQL, a shell command, a help message — use backticks:

```khora
const SCHEMA: String = `
  create table if not exists entries (
    id serial primary key,
    memo text not null
  )
`;
```

The indentation that lines the literal up with the code around it is **not part of the string**. The common prefix of the non-blank lines is removed, so relative indentation inside the text survives, and a delimiter on its own line contributes no blank line of its own.

A backtick literal is an ordinary `String` in every other way. `${...}` interpolates exactly as it does in a quoted literal, `\n` and friends still escape, and a literal backtick is written `` \` ``.

A literal that opens on the same line as its content strips nothing, because that line shares no indentation. Put the delimiter on its own line when you want the stripping.

Use `Show` for human-readable representations and dedicated encoding traits such as `ToJson`/`FromJson` when an external format has a contract of its own.

## Collection transforms

Prefer high-level transforms when they communicate intent clearly. Whole-program ownership analysis may turn some apparently persistent transformations into in-place reuse when the input is uniquely owned.

That optimization is deliberately invisible to source code: the meaning of the program does not depend on whether storage was reused.

## Shared collections

If several fibers need to coordinate around one evolving collection, place the collection behind an explicit `Shared` boundary or an external capability rather than smuggling mutation into otherwise pure code.
