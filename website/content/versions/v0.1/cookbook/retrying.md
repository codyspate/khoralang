---
title: Retrying a flaky call
sidebar:
  order: 9
---

Somebody else's service is down for four seconds. You would like your service not to be. `std::resilience` has the policy (`Schedule`) and the two ways to run one (`retry`, `repeat`).

## Complete example

A GET that backs off, gives up after five attempts, and does not waste any of them on a failure that will never come good:

```khora
module main;

import std::clock::{Clock};
import std::core::{Result, assert, attempt, print};
import std::net::http::{Answer, Call, CallError, HttpClient};
import std::random::{Random};
import std::resilience::{Schedule, retry_while};

fn fetch(url: String) -> Answer
  with { http: HttpClient }
  raises CallError
{
  match http.send(Call::get(url)) {
    Result::Ok(answer) => answer,
    Result::Err(why) => raise why,
  }
}

fn policy() -> Schedule {
  Schedule::Intersect(Schedule::backoff(100, 30000), Schedule::times(5))
}

fn worth_retrying(why: CallError) -> Bool {
  match why {
    CallError::Denied(_host) => false,
    CallError::BadUrl(_url) => false,
    CallError::Unreachable(_at) => true,
    CallError::Insecure(_why) => false,
    CallError::Closed(_why) => true,
    CallError::Malformed(_why) => false,
    CallError::TooLarge(_limit) => false,
  }
}

pub fn quote(url: String) -> Answer
  with { http: HttpClient, clock: Clock, random: Random }
  raises CallError
{
  retry_while(policy(), worth_retrying, fn () => fetch(url)!)!
}

pub fn main() {
  with { clock: Clock::real(), random: Random::real(), http: HttpClient::real() } {
    match attempt(fn () => quote("https://api.example.com/quote")!) {
      Result::Ok(answer) => print(Int::to_string(answer.status)),
      Result::Err(_why) => print("the quote service never answered"),
    }
  }
}
```

The row says what retrying costs: `clock` because it waits, `random` because the schedule is jittered. Neither is hidden, and neither is ambient — which is what makes the test below possible.

## Say what the policy is

`Schedule::backoff(100, 30000)` is the one to reach for: wait 100ms, then 200, 400, 800, capped at thirty seconds, each delay scaled by a random 50–100%.

That last part is the jitter, and it is not decoration. A thousand clients that all failed at the same instant and all back off by exactly 100ms retry at the same instant, which is the outage again.

The pieces underneath are an ordinary ADT, so a policy nobody anticipated is still spellable:

| | |
| --- | --- |
| `Times(n)` | at most `n` attempts, no delay |
| `Spaced(millis)` | every `millis`, anchored to the start so it does not drift |
| `Exponential(base, factor_pct, cap)` | doubling (at `200`) from the last failure |
| `Fibonacci(base)` | grows more gently than doubling |
| `Jittered(inner, low_pct, high_pct)` | `inner`, each delay scaled by a random percentage |
| `Union(a, b)` | runs while *either* would |
| `Intersect(a, b)` | runs while *both* would |
| `AndThen(first, then)` | `first` until it stops, then `then` |
| `UpTo(inner, millis)` | `inner`, but nothing after `millis` from the beginning |

`Intersect(backoff(..), times(5))` is "back off, but no more than five attempts". `AndThen(Times(3), Spaced(60000))` is "try hard three times, then keep trying once a minute" — and no library had to think of that one.

`UpTo` is the wall-clock budget, which is usually what somebody means by "give up after a minute":

```khora
Schedule::UpTo(Schedule::backoff(100, 30000), 60000)
```

A `Schedule` is a description and holds no clock, so it can be compared, printed, sent to another fiber, and written down in a test. That is the reason it is an ADT rather than the `(attempt, elapsed) -> Option<delay>` closure this type usually is: a log line saying `Exponential { base: 100, .. }` is worth more than one saying nothing.

## Do not retry a 404

`retry` tries again on any failure. `retry_while` takes a predicate, and this is where most of the value is: a request denied by `khora.toml`, a malformed URL, a TLS handshake that failed — none of those get better on the second attempt, and retrying them five times just makes the error arrive later.

The predicate is a closure over your own error type rather than a `Schedule` case, on purpose. *When to try again* is a policy somebody configures. *Whether this failure is worth trying again* is knowledge about the error. Keeping them apart is what lets `Schedule` stay a plain comparable value.

## Poll with `repeat`

`repeat` is the other half: run on the schedule until the body fails, and answer how many runs succeeded.

```khora
fn poll_forever() -> Int
  with { clock: Clock, random: Random, http: HttpClient }
{
  repeat(Schedule::Spaced(30000), fn () => fetch("https://api.example.com/health")!)
}
```

"Every thirty seconds until something breaks." The count comes back so a caller can tell "the schedule ended" from "it never ran once".

## Testing it takes no time at all

Waiting is an operation on `Clock`, so a test that pins the clock gets a retry loop that runs instantly:

```khora
const instant = handler for Clock {
  unix_seconds: fn () => 0,
  unix_millis: fn () => 0,
  monotonic_millis: fn () => 0,
  sleep: fn _millis => (),
};

test "it gives up after five attempts" {
  let flaky = handler for HttpClient {
    send: fn _call => Result::Err(CallError::Unreachable("api.example.com:443")),
  };
  let answer = attempt(fn () => quote("https://api.example.com/quote")!)
    with { clock: instant, random: Random::seeded(1), http: flaky };
  assert(Result::is_err(answer));
}
```

Five attempts with a thirty-second cap, and the test finishes in under a millisecond. There is no test runtime, no special mode, and no rule about which fiber may advance time — `sleep` is a capability operation, so a fake clock is four lines.

`Random::seeded` pins the jitter the same way, which is the same argument about the other unrepeatable input.

See the [`std::resilience` reference](/docs/stdlib/api/resilience/) for exact signatures, and [Testing capabilities](/docs/cookbook/testing-capabilities/) for the pattern in general.
