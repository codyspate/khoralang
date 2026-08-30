---
title: Configuration
sidebar:
  order: 7
---

Read every setting at start-up, report every bad one in a single message, and keep the password out of the logs. `std::config` does all three.

The alternative is what most services do: stop at the first missing variable, print it, and wait for somebody to redeploy so it can tell them the next one. Five restarts to learn five things it knew the first time.

## Complete example

```khora
module main;

import std::config::{ConfigError, integer, or_default, report, secret, string};
import std::core::{Redacted, Show, Validated, print};
import std::env::{Env};

derive(Show)
type Listen = {
  host: String,
  port: Int,
};

derive(Show)
type Settings = {
  listen: Listen,
  password: Redacted<String>,
};

fn listen() -> Validated<Listen, ConfigError> with { env: Env } {
  Validated::map2(
    or_default(string("HOST"), "0.0.0.0"),
    integer("PORT"),
    fn (host, port) => { host: host, port: port },
  )
}

fn settings() -> Validated<Settings, ConfigError> with { env: Env } {
  Validated::map2(
    listen(),
    secret("DB_PASSWORD"),
    fn (at, password) => { listen: at, password: password },
  )
}

pub fn main() {
  with { env: Env::real() } {
    match settings() {
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
PORT is not set
DB_PASSWORD is not set
```

Start it properly and the printed settings still have no password in them:

```text
Settings { listen: Listen { host: 0.0.0.0, port: 8080 }, password: <redacted> }
listening on 0.0.0.0:8080
```

If your `khora.toml` names an `env` list, the variables have to be in it, or you get a third kind of message that points at the manifest instead of at your deployment script:

```toml
[permissions]
env = ["HOST", "PORT", "DB_PASSWORD"]
```

A manifest with no `env` list grants every variable. Tightening is opt-in and each category is independent — naming `network` says nothing about `env`.

## Why nothing here raises

A `raises` stops at the first failure. That is right for a chain where the next step needs the last one's value, and wrong for configuration, where the keys have nothing to do with each other and you want the whole list.

So every reader answers `Validated<A, ConfigError>` instead, and `Validated::map2` keeps both sides' failures:

```khora
Validated::map2(a, b, fn (x, y) => combine(x, y))
```

If `a` and `b` both failed, the answer carries both errors and `combine` never runs. For a third field, split the record the way `listen` is split above: the halves compose, so a subsystem can own its own reader and you never nest `map2` more than one deep.

`Validated::and_then` is the fail-fast one, for a second step written in terms of the first's value. `integer` is `string` plus `and_then`: "not set" and "not a number" are never both true of one variable.

## The readers

`string`, `integer`, `boolean` and `secret` each read one variable and say what went wrong. `boolean` takes `true`, `false`, `1` and `0`, and nothing else — `yes`, `on` and `Y` all mean true somewhere, and a reader that accepts all of them accepts a typo as a `false`.

`or_default` fires on *missing* and on nothing else:

```khora
or_default(string("HOST"), "0.0.0.0")
```

`HOST` unset gives `0.0.0.0`. `PORT=eigthy` does not quietly become `8080` — a value that is present and wrong is still an error, which is the bug this module exists to catch.

## Three ways a key can be wrong

`ConfigError` keeps them apart because they send you to three different files:

| | What it means | Where the fix is |
| --- | --- | --- |
| `Missing` | nobody set it | the deployment script |
| `Malformed` | it is set to nonsense | the value |
| `Denied` | the manifest does not grant it | `khora.toml` |

`report` turns a list of them into one line each, in the order they were read. That is the string a service prints just before it stops.

## Secrets

`secret` gives back a `Redacted<String>`, and that is a compile-time thing rather than a convention:

- `Show` prints `<redacted>`, so a record holding one still derives `Show` and the start-up log stays useful.
- There is no `ToJson`, so a record holding one does **not** derive `ToJson`. The build stops, which is the right place to stop.
- `"${password}"` compiles and prints `<redacted>`, because a hole calls `Show` like anything else. It used to be refused outright, which read as stricter and was not: the way past a hole that will not compile is `expose`, and then the secret is in the log.

The only way out is `Redacted::expose`, which is a word a reviewer can search for. There is deliberately no `Eq`: comparing two secrets byte by byte is how a timing side channel gets written by somebody who was not writing one.

## Testing it

`Env` is a capability, so a test hands the readers a different environment instead of setting one on the machine:

```khora
const fake_env = handler for Env {
  variable: fn name => if name.eq("PORT") { Option::Some("8080") } else { Option::None },
  arguments: fn () => [],
};

test "a missing password is reported, not guessed" {
  let answer = settings() with { env: fake_env };
  assert(!Validated::is_valid(answer));
}
```

That is the whole reason `std::config` has no `Config<A>` description type. Elsewhere this idea needs a value denoting "read `PORT` as an integer", interpreted later by a swappable provider — the description layer exists to defer the read so a test can intercept it. Khora's provider is the `Env` handler and it was already swappable.

See [Testing capabilities](/docs/cookbook/testing-capabilities/) for the pattern in full, and the [`std::config` reference](/docs/stdlib/api/config/) for exact signatures.
