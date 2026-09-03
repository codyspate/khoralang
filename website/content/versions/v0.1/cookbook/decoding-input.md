---
title: Decode untrusted input
sidebar:
  order: 6
---

Write the type once. The compiler writes the schema from it, and the schema
decodes a request body, a test fixture or the deployment's variables without
knowing which.

```khora
derive(Show, Decode)
pub type Settings = {
  listen: Listen,
  password: Redacted<String>,
  rate: Decimal,
  debug: Option<Bool>,
  mode: Mode,
};

let settings = Settings::schema().decode(Raw::of_json(document));
```

The alternative is a reader per source, and the signature of the one this
library replaced shows why that does not generalise:

```khora
pub fn string(name: String) -> Validated<String, ConfigError> with { env: Env }
```

`string(name)` is not "this field is text". It is *go to the environment, fetch
this variable, and give me the text or a reason* — the shape and the reading
are one function. So a JSON body needs its own vocabulary, and so does a CLI
argument, and so does a database row. Now `std::config::read(schema)` is one
source among several, and the same `Settings` reads all of them.

## Every problem, not the first

A record with four bad fields reports four. A person fixing a deployment wants
the list, not one line per restart:

```text
refused:
  listen.hostname is not set
  listen.port must be between 1 and 65535
  password should be text
  rate should be an exact decimal, and is "about seven percent"
  debug should be true or false, and is "maybe"
  mode.type should be one of `Local`, `Remote`, and is "Cloud"
```

That is why `decode` answers a `Validated` rather than raising. Paths read the
way somebody would write them, and a rule supplies its own sentence, so
`between(int(), 1, 65535)` becomes *listen.port must be between 1 and 65535*.
Text is quoted and a number is bare, so `and is "8080"` says the value arrived
as text.

`Validated::to_result` is one call for a caller who would rather stop at the
first.

## The declaration is the schema

Every customization is a type. `Option<Bool>` is optional, `Redacted<String>`
is a secret, `Decimal` is exact, `List<A>` is a list, and a nested type is
found through the `Decode` trait — which is where a rule the declaration
cannot say goes:

```khora
derive(Show)
pub type Port = Int;

impl Decode for Port {
  fn schema() -> Schema<Port> { Schema::map(between(int(), 1, 65535), fn n => Port(n)) }
}
```

A `Port` that came through the schema passed the rule, and `Settings` picks
the impl up through `Listen` without being told. A variant derives to a bare
string for a payload-free case and an object tagged with `type` for the rest,
so `"mode": "Local"` and `"mode": { "type": "Remote", "url": ".." }` are the
two forms.

## A record written by hand

When the wire spells a field differently from the type, the record's schema
is written as the record's own literal with a schema where each value would
go:

```khora
impl Decode for Listen {
  fn schema() -> Schema<Listen> {
    struct({ host: key("hostname", string()), port: Port::schema() })
  }
}
```

The literal is checked against `Listen` from the declared return type, the
way any record literal is. A field whose schema decodes the wrong type is
reported at that schema, and a field the record does not have is reported at
the call.

## A secret is a combinator

`Redacted<String>` in the type is `secret(string())` in the schema, and
`secret` wraps any schema, so `secret(int())` works too. The decoded value
shows as `<redacted>`. More importantly, **a failure inside a secret does not
quote what it saw**:

```text
password should be text
```

Quoting the bad value is most of what makes a decode error worth reading, and
it is also the easiest imaginable way to put a password in a log. The wrapper
is unconditional inside the error type so no future variant can forget it, and
only the message decides whether to expose it. A record holding a secret
derives `Decode` and refuses `Encode`, so it reads and does not write.

## Numbers keep their text

`Raw::Number` holds the token rather than a `Float`, so `decimal()` parses it
with `Decimal::of_string` and `0.0725` stays `0.0725`. A price read through a
double is the wrong price. `decimal()` reads text too, because money travels as
a string on most wires, and `Decimal` encodes as one.

## Ask what a configuration needs, without running it

A schema carries an untyped `Shape` beside its decoder, so its structure can be
walked:

```khora
print("it needs: ${Shape::keys(Settings::schema().shape)}");
```

```text
it needs: [listen, password, rate, debug, mode]
```

The question a deployment asks. Top-level keys only — walk into
`Shape::Struct` for the nested ones. A schema that were only a closure could
not answer this at all, which is why it is two halves in one record.

## Complete example

```khora
module service::main;

import std::core::{List, Option, Redacted, Result, Show, Validated, print};
import std::decimal::{Decimal};
import std::json::{parse};
import std::schema::{Decode, Raw, Rejection, Schema, Shape, between, int, key, string, struct};

/// A port a socket will accept. A newtype, so the rule lives on the type and
/// nothing downstream checks again.
derive(Show)
pub type Port = Int;

impl Decode for Port {
  fn schema() -> Schema<Port> { Schema::map(between(int(), 1, 65535), fn n => Port(n)) }
}

/// Written by hand, because the wire spells the host `hostname`. Everything
/// that contains a `Listen` finds this through the trait.
derive(Show)
pub type Listen = { host: String, port: Port };

impl Decode for Listen {
  fn schema() -> Schema<Listen> {
    struct({ host: key("hostname", string()), port: Port::schema() })
  }
}

/// Local, or an upstream by URL.
derive(Show, Decode)
pub type Mode = | Local | Remote(url: String);

/// What the service needs, written once. The declaration is the schema.
derive(Show, Decode)
pub type Settings = {
  listen: Listen,
  password: Redacted<String>,
  rate: Decimal,
  debug: Option<Bool>,
  mode: Mode,
};

fn report(text: String) -> String {
  match parse(text) {
    Result::Err(_why) => "not JSON",
    Result::Ok(document) =>
      match Validated::to_result(Settings::schema().decode(Raw::of_json(document))) {
        Result::Ok(s) =>
          "listening on ${s.listen.host}:${s.listen.port} at ${s.rate}, password ${s.password}, debug ${s.debug}, mode ${s.mode}",
        Result::Err(problems) =>
          List::fold(problems, "refused:", fn (acc, r) => acc + "\n  " + Rejection::describe(r)),
      },
  }
}

pub fn main() -> Int {
  print(report(`{"listen": {"hostname": "localhost", "port": 8080}, "password": "hunter2", "rate": "0.0725", "mode": "Local"}`));
  print(report(`{"listen": {"port": 99999}, "password": 42, "rate": "about seven percent", "debug": "maybe", "mode": {"type": "Cloud"}}`));
  print("it needs: ${Shape::keys(Settings::schema().shape)}");
  0
}
```

Its output:

```text
listening on localhost:Port(8080) at 0.0725, password <redacted>, debug None, mode Mode::Local
refused:
  listen.hostname is not set
  listen.port must be between 1 and 65535
  password should be text
  rate should be an exact decimal, and is "about seven percent"
  debug should be true or false, and is "maybe"
  mode.type should be one of `Local`, `Remote`, and is "Cloud"
it needs: [listen, password, rate, debug, mode]
```

## See also

- [Load application configuration](/docs/cookbook/configuration/) — the same
  schema read out of the environment, with every problem spelled as the
  variable it came from.
- [Build a typed JSON API](/docs/cookbook/json-api/) — `std::json`'s own
  decoders, which are separate from this; `Raw::of_json` is the bridge from a
  parsed document to a schema.
