---
title: Logging, and correlating it with traces
sidebar:
  order: 13
---

A log line is read by a program before it is read by a person: it goes to a
collector, gets indexed, and is queried months later by somebody who was not
there. So `std::log` emits one JSON object per line, on standard error.

```khora
module orders::main;

import std::clock::{Clock};
import std::log::{Severity, Log, info, warn};

fn charge(amount: Int) -> Int with { log: Log } {
  info("charging");
  if amount > 1000 { warn("unusually large") } else { () };
  amount
}

pub fn main() -> Int {
  with { log: Log::json(Severity::Info) } {
    charge(1200)
  }
}
```

```text
{"timestamp":1757021376817,"level":"info","message":"charging"}
{"timestamp":1757021376819,"level":"warn","message":"unusually large"}
```

## Why it is a capability

Because everything else that reaches outside is. A function that logs writes to
a stream somebody else owns, and Khora's argument throughout is that such a
function says so:

```khora
fn charge(account: Account, amount: Decimal) -> Receipt
  with { log: Log, db: Db }
  raises DbError
```

That buys three things. A test installs a logger that collects into a list and
asserts on what was said, with no global to reset between cases. A library
cannot log without its caller knowing, because the row would say so. And the
caller chooses the format and the destination, which is a decision no library
should make for the program embedding it.

`print` is not a capability. It predates the argument rather than refuting it,
and its own documentation says so.

## Structure beats formatting

Anything worth searching for later goes in a field rather than in the sentence:

```khora
import std::trace::{number, text};

log.record(Severity::Error, "charge failed", [
  text("account", account.id),
  number("amount_minor", amount),
]);
```

```text
{"timestamp":1757021376823,"level":"error","message":"charge failed","account":"acct_19","amount_minor":1200}
```

A message that interpolates the account id reads the same to a person and is
useless to a collector, because every line is a distinct string. A field is
what an index can group by.

Attributes are `std::trace`'s `Attribute`, deliberately: the same vocabulary
annotates a span, so a value does not change shape depending on which of the
two you send it to.

## Correlating with a trace

Nothing to do. A line logged inside a span carries that span's ids:

```khora
fn work() -> () with { log: Log } {
  info("processing");
}

around(tracer, "request", fn () => work())
```

```text
{"timestamp":1757021376823,"level":"info","message":"processing","trace_id":"4bf92f3577b34da6a3ce929d0e0e4736","span_id":"00f067aa0ba902b7"}
```

`work` says nothing about tracing, takes no span, and is not passed the tracer.
The handler asks [`current`](/docs/stdlib/api/trace/) what the fiber is inside
and writes the ids if there is an answer — and writes neither field if there is
not, rather than thirty-two zeros a collector would group every unparented line
under.

The field names are the ones OpenTelemetry expects, so a collector shows the
lines beside the span without being configured to.

**This works across a spawn**, which is the case it exists for: a fiber
inherits its spawner's current span when it is created, so a request that fans
out into four fibers is one trace. It inherits a copy — a span the child opens
is the child's, and does not become the parent's when the child returns.

A line logged outside every span carries no ids, which is correct and is why
the field is absent rather than empty.

## Choosing a level

Five, in the order everybody already filters on:

| Severity | For |
| --- | --- |
| `Trace` | The finest detail, off outside a debugging session. |
| `Debug` | What a developer wants while working on this code. |
| `Info` | What an operator wants while the program is behaving. |
| `Warn` | Something is wrong and the program is carrying on. |
| `Error` | Something is wrong and something did not happen. |

**The handler drops what is below the minimum, not the caller.** A caller that
checked the level itself would need to know the configuration, which is exactly
what the capability keeps away from it. Take the minimum from the environment
where an operator can change it:

```khora
import std::core::{Option};
import std::env::{Env, variable_or};

let wanted = Severity::of_name(variable_or("LOG_LEVEL", "info")!);
let minimum = match wanted { Option::Some(level) => level, Option::None => Severity::Info };
```

## Testing what a function logged

A logger is a value, so a test installs one that remembers:

```khora
test "a large charge is warned about" {
  let said = Shared::of(List::Nil);
  let collecting = handler for Log {
    record: fn (level, message, _attributes) =>
      Shared::update(said, fn lines => List::Cons(message, lines)),
  };
  with { log: collecting } { charge(1200) };
  assert(List::length(Shared::get(said)) == 2);
}
```

No global to reset, no capture to install, and two tests running at once cannot
see each other's lines — which is the whole reason this is an effect rather
than a function that writes to a stream.

## When you want a terminal, not a collector

`Log::plain` writes `LEVEL message key=value` instead, for a small program
whose log is read by whoever ran it:

```text
INFO  charging
WARN  unusually large
```

Anything that ships its logs anywhere wants `Log::json`.

## Timestamps and testing

`Log::json` stamps from the real clock. `Log::json_using` takes one instead,
which is what makes a log line assertable: a fixed clock produces a
byte-for-byte predictable object, and a line with a real timestamp in it can
only be matched with a regular expression — which is a test of the regular
expression.

```khora
let fixed = handler for Clock {
  unix_seconds: fn () => 1757021376,
  unix_millis: fn () => 1757021376817,
  monotonic_millis: fn () => 0,
  sleep: fn _ms => (),
};
with { log: Log::json_using(Severity::Info, fixed) } { work() }
```

## Just writing to standard error

For a usage message, before any capability exists to install:

```khora
import std::log::{eprint};

eprint("usage: report <events.json>");
```

`print` writes a program's *answer*, and an answer is what `> out.txt` is for.
A diagnostic written there goes into the file with it, which is how a
command-line tool ends up silently discarding the reason it failed.
