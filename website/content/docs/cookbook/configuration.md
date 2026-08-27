---
title: Configuration
sidebar:
  order: 7
---

Read process configuration at the application boundary, validate it once, and turn it into a typed value. Khora's `Env` capability makes the dependency visible and lets tests supply a different environment without mutating process-global state.

## Complete example

This program accepts an optional `PORT`, requires `DATABASE_URL`, and reports invalid startup configuration before doing application work:

```khora
module main;

import std::core::{Option, print};
import std::env::{Env, variable_or};

pub type Config = {
  port: Int,
  database_url: String,
};

pub type ConfigError =
  | InvalidPort(value: String)
  | MissingDatabaseUrl;

fn load_config() -> Config
  with { env: Env }
  raises ConfigError
{
  let raw_port = variable_or("PORT", "8080");

  let port = match Int::of_string(raw_port) {
    Option::Some(value) => value,
    Option::None => raise ConfigError::InvalidPort(raw_port),
  };

  let database_url = match env.variable("DATABASE_URL") {
    Option::Some(value) => value,
    Option::None => raise ConfigError::MissingDatabaseUrl,
  };

  {
    port: port,
    database_url: database_url,
  }
}

fn run() -> ()
  with { env: Env }
{
  let config = load_config()! catch {
    ConfigError::InvalidPort(value) => {
      print("PORT must be an integer; got ${value}");
      return;
    },
    ConfigError::MissingDatabaseUrl => {
      print("DATABASE_URL is required");
      return;
    },
  };

  print("configuration accepted");
  print("port = ${Int::to_string(config.port)}");
}

pub fn main() {
  with { env: Env::real() } {
    run()
  }
}
```

`load_config` is explicit about both parts of its contract:

```khora
fn load_config() -> Config
  with { env: Env }
  raises ConfigError
```

It needs environment authority, and it may reject invalid configuration. Once it returns a `Config`, the rest of the program can work with validated fields instead of repeatedly parsing strings.

## Defaults and required values

Use `variable_or` for a real default:

```khora
let raw_port = variable_or("PORT", "8080");
```

Use `env.variable` when absence is an error or has domain meaning:

```khora
match env.variable("DATABASE_URL") {
  Option::Some(value) => value,
  Option::None => raise ConfigError::MissingDatabaseUrl,
}
```

Keep secrets out of logs, trace attributes, and error messages. The example deliberately reports that `DATABASE_URL` is missing without printing a database URL value.

## Testing the loader

Because `Env` is a capability, a test can provide a small handler instead of changing the machine environment. The [Testing capabilities](/docs/cookbook/testing-capabilities/) recipe shows that pattern in full.

For the shipped `Env` operations, see the [environment API reference](/docs/stdlib/api/env/).
