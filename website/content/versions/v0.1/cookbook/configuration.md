---
title: Configuration
sidebar:
  order: 7
---

Read every setting at start-up, report every bad one in a single message, and
keep the password out of the logs. Write the type once, and `std::config`
reads it out of the environment.

The alternative is what most services do: stop at the first missing variable,
print it, and wait for somebody to redeploy so it can tell them the next one.
Five restarts to learn five things it knew the first time.

## Complete example

```khora
module main;

import std::config::{read, report};
import std::core::{Redacted, Show, Validated, print};
import std::env::{Env};
import std::schema::{Decode, Schema, default, int, string, struct};

derive(Show)
type Listen = {
  host: String,
  port: Int,
};

/// Written by hand, so the host has a default; everything that contains a
/// `Listen` finds this through the trait.
impl Decode for Listen {
  fn schema() -> Schema<Listen> {
    struct({ host: default(string(), "0.0.0.0"), port: int() })
  }
}

derive(Show, Decode)
type Settings = {
  listen: Listen,
  db_password: Redacted<String>,
};

pub fn main() {
  with { env: Env::real() } {
    match read(Settings::schema()) {
      Validated::Invalid(problems) => print(report(problems)),
      Validated::Valid(config) => {
        print(config.show());
        serve(config)
      }
    }
  }
}

fn serve(config: Settings) -> () {
  print("listening on ${config.listen.host}:${config.listen.port}")
}
```

Start it with nothing set and it says everything at once:

```text
LISTEN_PORT is not set
DB_PASSWORD is not set
```

Start it properly and the printed settings still have no password in them:

```text
Settings { listen: Listen { host: 0.0.0.0, port: 8080 }, db_password: <redacted> }
listening on 0.0.0.0:8080
```

If your `khora.toml` names an `env` list, the variables have to be in it, or
you get a third kind of message that points at the manifest instead of at your
deployment script:

```toml
[permissions]
env = ["LISTEN_HOST", "LISTEN_PORT", "DB_PASSWORD"]
```

A manifest with no `env` list grants every variable. Tightening is opt-in and
each category is independent — naming `network` says nothing about `env`.

## The declaration is the schema

`read` takes any `Schema<A>`, and `derive(Decode)` writes one from the type,
so most settings records need nothing but the declaration. The shape decides
which variables are read and what they are called:

| the type says | the environment holds |
| --- | --- |
| a field `port` | `PORT` |
| a nested record `listen: Listen` with a `port` | `LISTEN_PORT` |
| `tags: List<String>` | `TAGS=a,b`, split on commas |
| `mode: Mode` with `Mode = \| Local \| Remote(url: String)` | `MODE=Local`, or `MODE=Remote` and `MODE_URL` |
| `debug: Option<Bool>` | `DEBUG`, or nothing |
| `password: Redacted<String>` | `PASSWORD`, and it never reaches a log |

`variables(Settings::schema().shape)` lists them all, in order, without
reading anything — the question a deployment asks, answered without starting
the program.

## Why nothing here raises

A `raises` stops at the first failure. That is right for a chain where the
next step needs the last one's value, and wrong for configuration, where the
keys have nothing to do with each other and you want the whole list.

So `read` answers `Validated<A, Rejection>`, and a record with three bad
fields reports three. `Validated::to_result` is one call for a caller who
would rather stop at the first.

## A default, and a rule

A field with a default, or a rule the declaration cannot say, is a record
written by hand with a `struct({ .. })` literal, as `Listen` is above. A
default fires on *missing* and on nothing else:

```khora
struct({ host: default(string(), "0.0.0.0"), port: between(int(), 1, 65535) })
```

`LISTEN_HOST` unset gives `0.0.0.0`. `LISTEN_PORT=eigthy` does not quietly
become `8080` — a value that is present and wrong is still an error, which is
the bug this module exists to catch. A flag takes `true`, `false`, `1` and
`0`, and nothing else — `yes`, `on` and `Y` all mean true somewhere, and a
reader that accepts all of them accepts a typo as a `false`.

## Three ways a key can be wrong

A `Rejection` keeps them apart because they send you to three different
files:

| | What it means | Where the fix is |
| --- | --- | --- |
| `Missing` | nobody set it | the deployment script |
| `Wrong` or `Refused` | it is set to nonsense | the value |
| `Denied` | the manifest does not grant it | `khora.toml` |

`report` turns a list of them into one line each, in the order the type
declares them, with each path spelled as the variable it came from. That is
the string a service prints just before it stops.

## Secrets

`Redacted<String>` in the type is a compile-time thing rather than a
convention:

- `Show` prints `<redacted>`, so a record holding one still derives `Show` and
  the start-up log stays useful.
- There is no `Encode`, so a record holding one does **not** derive `Encode`.
  The build stops, which is the right place to stop.
- A failure inside one never quotes what it saw: `DB_PASSWORD should be text`,
  and not what the deployment put there.
- `"${password}"` compiles and prints `<redacted>`, because a hole calls
  `Show` like anything else. Nothing needs `expose` to print one, which
  matters: `expose` hands over the secret itself, and it is what somebody
  reaches for when a hole will not compile.

The only way out is `Redacted::expose`, which is a word a reviewer can search
for. There is deliberately no `Eq`: comparing two secrets byte by byte is how
a timing side channel gets written by somebody who was not writing one.

## Testing it

`Env` is a capability, so a test hands `read` a different environment instead
of setting one on the machine:

```khora
const fake_env = handler for Env {
  variable: fn name => if name.eq("LISTEN_PORT") { Option::Some("8080") } else { Option::None },
  arguments: fn () => [],
};

test "a missing password is reported, not guessed" {
  let answer = read(Settings::schema()) with { env: fake_env };
  assert(!Validated::is_valid(answer));
}
```

Khora's provider is the `Env` handler and it was already swappable; the
schema is not there to defer the read, it is there so that the same
`Settings` reads a request body and a test fixture without knowing which.

See [Testing capabilities](/docs/cookbook/testing-capabilities/) for the
pattern in full, [Decode untrusted input](/docs/cookbook/decoding-input/) for
the schema itself, and the [`std::config` reference](/docs/stdlib/api/config/)
for exact signatures.
