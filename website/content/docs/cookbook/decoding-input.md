---
title: Decode untrusted input
sidebar:
  order: 6
---

Describe the shape of a value once. Decode it from the environment, from a
request body, from a test fixture — the description does not know which, which
is what makes it worth writing down.

```khora
let settings = Schema::decode(schema(), incoming)!;
```

The alternative is what `std::config` does today, and reading its signature
shows why it does not generalise:

```khora
pub fn string(name: String) -> Validated<String, ConfigError> with { env: Env }
```

`string(name)` is not "this field is text". It is *go to the environment, fetch
this variable, and give me the text or a reason* — the shape and the reading
are one function. So a JSON body needs its own vocabulary, and so does a CLI
argument, and so does a database row.

## Every problem, not the first

A record with four bad fields reports four. A person fixing a deployment wants
the list, not one line per restart:

```text
refused:
  listen.host is not set
  listen.port must be between 1 and 65535
  password is not set
  rate should be an exact decimal, and is `about seven percent`
```

That is why `decode` answers a `Validated` rather than raising. Paths read the
way somebody would write them, and a refinement supplies its own sentence, so
`"between 1 and 65535"` becomes *listen.port must be between 1 and 65535*.

`Validated::to_result` is one call for a caller who would rather stop at the
first.

## A secret is a combinator

`secret` wraps another schema rather than being a leaf of its own, so
`secret(whole())` and `secret(text())` both work and redaction composes with
everything else:

```khora
"password", secret(text()),
```

The decoded value is a `Redacted<String>`, which shows as `<redacted>`. More
importantly, **a failure inside a secret does not quote what it saw**:

```text
public should be a whole number, and is `not a number`
token should be a whole number
```

Quoting the bad value is most of what makes a decode error worth reading, and
it is also the easiest imaginable way to put a password in a log. The wrapper
is unconditional inside the error type so no future variant can forget it, and
only the message decides whether to expose it.

## Numbers keep their text

`Raw::Num` holds the token rather than a `Float`, so `exact()` parses it with
`Decimal::of_string` and `0.0725` stays `0.0725`. A price read through a double
is the wrong price.

`std::json` works the same way: `Json::Number` carries the token's text, and
`Json::integer` reads it with `Int::of_string`, so `9007199254740993` comes back
as itself. Use `Json::number` when a `Float` is what you want and the fifteen
significant digits are enough; use `Json::literal` when you want to hand the
text to `Decimal::of_string` yourself.

## Ask what a configuration needs, without running it

A schema carries an untyped `Shape` beside its decoder, so its structure can be
walked:

```khora
print("it needs: ${Shape::keys(schema().shape)}");
```

```text
it needs: [listen, password, rate, debug]
```

The question a deployment asks. Top-level keys only — walk into
`Shape::Struct_` for the nested ones. A schema that were only a closure could
not answer this at all, which is why it is two halves in one record.

## Complete example

```khora
module service::main;

import std::core::{List, Option, Pair, Redacted, Result, Show, Validated, print};
import std::decimal::{Decimal};
import std::schema::{Raw, Rejection, Schema, Shape, exact, optional, refine, secret, struct2,
                     struct4, text, truth, whole};

/// What the service needs, written once.
pub type Listen = { host: String, port: Int };

pub type Settings = {
  listen: Listen,
  password: Redacted<String>,
  rate: Decimal,
  debug: Option<Bool>,
};

fn listen(host: String, port: Int) -> Listen { { host: host, port: port } }

fn settings(
  listen: Listen,
  password: Redacted<String>,
  rate: Decimal,
  debug: Option<Bool>,
) -> Settings {
  { listen: listen, password: password, rate: rate, debug: debug }
}

/// A port is a whole number in a range, and the range is part of the shape.
fn port() -> Schema<Int> {
  refine(whole(), "between 1 and 65535", fn p => p > 0 && p < 65536)
}

pub fn schema() -> Schema<Settings> {
  struct4(
    "listen", struct2("host", text(), "port", port(), listen),
    "password", secret(text()),
    "rate", exact(),
    "debug", optional(truth()),
    settings,
  )
}

/// A record, as a source would hand it over.
fn field(key: String, value: Raw) -> Pair<String, Raw> { { key: key, value: value } }

fn sample() -> Raw {
  Raw::Record([
    field("listen", Raw::Record([
      field("host", Raw::Text_("localhost")),
      field("port", Raw::Num("8080")),
    ])),
    field("password", Raw::Text_("hunter2")),
    field("rate", Raw::Num("0.0725")),
  ])
}

fn broken() -> Raw {
  Raw::Record([
    field("listen", Raw::Record([field("port", Raw::Num("99999"))])),
    field("rate", Raw::Text_("about seven percent")),
  ])
}

fn report(from: Raw) -> String {
  match Validated::to_result(Schema::decode(schema(), from)) {
    Result::Ok(s) =>
      "listening on ${s.listen.host}:${s.listen.port} at ${s.rate}, password ${s.password}, debug ${s.debug}",
    Result::Err(problems) =>
      List::fold(problems, "refused:", fn (acc, r) => acc + "\n  " + Rejection::describe(r)),
  }
}

pub fn main() -> Int {
  print(report(sample()));
  print(report(broken()));
  print("it needs: ${Shape::keys(schema().shape)}");
  0
}
```

Its output:

```text
listening on localhost:8080 at 0.0725, password <redacted>, debug None
refused:
  listen.host is not set
  listen.port must be between 1 and 65535
  password is not set
  rate should be an exact decimal, and is `about seven percent`
it needs: [listen, password, rate, debug]
```

## The assembler, and why it is there

`struct2` through `struct5` take a function that puts the decoded pieces
together. The spelling a reader reaches for first is this:

```khora
Schema::struct({ port: whole(), host: text() })   // does not type-check
```

Its argument is a record of *schemas*, and the result would have to be a schema
of the record of what they decode — a type-level map from one to the other,
which Khora does not have. So something has to say how the pieces become the
record, and here that is a function. Naming it, rather than passing a lambda,
usually reads better:

```khora
fn listen(host: String, port: Int) -> Listen { { host: host, port: port } }

struct2("host", text(), "port", port(), listen)
```

Beyond five fields, nest a record rather than reaching for a wider combinator —
which is usually what the shape of the data was telling you anyway.

`derive(Schema)` will remove the assembler entirely by generating it from the
type. It is not shipped yet; `docs/design/schema.md` records why it is required
rather than a convenience.

## See also

- [Load application configuration](/docs/cookbook/configuration/) — what
  `std::config` does today, and still the shortest path for reading settings
  out of the environment.
- [Build a typed JSON API](/docs/cookbook/json-api/) — `std::json`'s own
  decoders, which are separate from this and stay so until they share a `Raw`.
