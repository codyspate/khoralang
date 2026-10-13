---
title: Sign and verify a webhook
sidebar:
  order: 14
---

Sign every webhook you send, and refuse any webhook you receive whose
signature does not check out or whose timestamp is too old. The signature is
an HMAC-SHA-256, under a secret the two sides share, of the delivery time and
the raw body joined by a dot. It travels in one header. For the body
`{"id":"evt_1","event":"paid"}`, sent at Unix time 1760400000 under the secret
`new-secret`, the header is:

```text
Webhook-Signature: t=1760400000,v1=3068d7500001f81f4c97cdff1231f68174c7d61ab0f0327cae3fcb51076a186d
```

## Complete example

The program reads its secret from `WEBHOOK_SECRET`. `webhook new-secret`
prints a fresh one, which you run once by hand and give to both sides. With a
secret set, the program serves `POST /webhook` and accepts only deliveries
that are signed and recent. Without one, it says how to make one and exits
with status 2.

```khora
module main;

import std::clock::{Clock};
import std::core::{Array, ChildFailed, List, Option, Redacted, SharedFn, print};
import std::crypto::{Base64, Hex, Hmac, SecureRandom};
import std::env::{Env, EnvError};
import std::log::{eprint};
import std::net::http::{Call, HttpError, Request, Response, Router};

/// The header a signature travels in: `t=<unix seconds>,v1=<hex>`.
fn signature_header() -> String { "Webhook-Signature" }

/// How far a delivery's timestamp may be from this machine's clock, in seconds.
fn tolerance() -> Int { 300 }

/// The secrets a receiver accepts: the one senders sign with now, and,
/// while a rotation is under way, the one they signed with before.
///
/// Held as text rather than bytes: a handler shared between request fibers
/// may capture only values nothing can write, and an `Array` can be written.
type Keys = {
  current: Redacted<String>,
  previous: Option<Redacted<String>>,
};

/// A new secret: 32 bytes from the operating system, as URL-safe base64.
fn new_secret() -> String with { random: SecureRandom } {
  let bytes: Array<U8> = Array::new(32, U8::of(0));
  random.bytes(bytes);
  Base64::encode_url_safe(bytes)
}

/// The key the HMAC is computed under: the secret's bytes, still redacted.
fn key(secret: Redacted<String>) -> Redacted<Array<U8>> {
  Redacted::of(String::bytes(Redacted::expose(secret)))
}

/// The tag over `<timestamp>.<body>`. Signing the timestamp with the body is
/// what stops an attacker from replaying an old delivery under a new time.
fn tag(secret: Redacted<String>, timestamp: Int, body: String) -> Array<U8> {
  Hmac::sha256(key(secret), String::bytes("${timestamp}.${body}"))
}

/// The header value a sender puts on a delivery made at `timestamp`.
fn signature(secret: Redacted<String>, timestamp: Int, body: String) -> String {
  "t=${timestamp},v1=${Hex::encode(tag(secret, timestamp, body))}"
}

/// A delivery to `url`, signed now. Send it with an `HttpClient`.
fn delivery(secret: Redacted<String>, url: String, body: String) -> Call with { clock: Clock } {
  Call::post(url, body)
    |> Call::with_header(signature_header(), signature(secret, clock.unix_seconds(), body))
}

/// The value of `name=` in a header of comma-separated `name=value` pairs.
/// `v1` may appear more than once; this is the first, `signed` tries them all.
fn field(header: String, name: String) -> Option<String> {
  List::find(String::split(header, ","), fn part => String::starts_with(part, "${name}="))
    |> Option::map(fn part => String::slice(part, String::byte_length(name) + 1, String::byte_length(part)))
}

/// Whether any `v1=` in `header` is the tag of `<timestamp>.<body>` under
/// `secret`.
///
/// Each tag is decoded and checked with `verify_sha256`, never compared as
/// text with `==`: `==` stops at the first difference, and how long it took
/// tells a forger how much of a guess was right.
fn signed(secret: Redacted<String>, header: String, timestamp: Int, body: String) -> Bool {
  let message = String::bytes("${timestamp}.${body}");
  List::any(String::split(header, ","), fn part =>
    String::starts_with(part, "v1=") && match Hex::decode(String::slice(part, 3, String::byte_length(part))) {
      Option::None => false,
      Option::Some(given) => Hmac::verify_sha256(key(secret), message, given),
    })
}

/// Whether either accepted secret signed this delivery.
fn genuine(keys: Keys, header: String, timestamp: Int, body: String) -> Bool {
  signed(keys.current, header, timestamp, body) || match keys.previous {
    Option::Some(old) => signed(old, header, timestamp, body),
    Option::None => false,
  }
}

/// The receiving side. The timestamp is checked first: a delivery outside
/// the window is refused without spending an HMAC on it.
fn receive(keys: Keys, request: Request) -> Response with { clock: Clock } {
  match Request::header(request, signature_header()) {
    Option::None => Response::text(401, "unsigned"),
    Option::Some(header) => match field(header, "t") |> Option::and_then(fn t => Int::of_string(t)) {
      Option::None => Response::text(401, "no timestamp"),
      Option::Some(timestamp) => {
        // Compare against the clock rather than subtracting from an attacker's
        // number: `now - timestamp` overflows for a `t` near the bottom of
        // `Int`, and the trap would take the whole server down.
        let now = clock.unix_seconds();
        if timestamp < now - tolerance() || timestamp > now + tolerance() {
          Response::text(401, "too old or too far ahead")
        } else if genuine(keys, header, timestamp, request.body) {
          Response::text(200, "accepted")
        } else {
          Response::text(401, "bad signature")
        }
      },
    },
  }
}

/// The secrets from the environment, or `None` when `WEBHOOK_SECRET` is not
/// set. `WEBHOOK_SECRET_PREVIOUS` is set only during a rotation.
fn keys() -> Option<Keys> with { env: Env } raises EnvError {
  let previous = env.variable("WEBHOOK_SECRET_PREVIOUS")! |> Option::map(fn p => Redacted::of(p));
  env.variable("WEBHOOK_SECRET")! |> Option::map(fn current => { current: Redacted::of(current), previous: previous })
}

pub fn main() -> Int raises HttpError + ChildFailed + EnvError {
  let arguments = with { env: Env::real() } { env.arguments() };
  if List::nth(arguments, 1) == Option::Some("new-secret") {
    // Asked for by hand, once, to configure both sides.
    print(with { random: SecureRandom::real() } { new_secret() });
    0
  } else {
    match with { env: Env::real() } { keys()! } {
      Option::None => {
        eprint("WEBHOOK_SECRET is not set. Generate one with `webhook new-secret`, and set it in the environment of the sender and of this receiver.");
        2
      },
      Option::Some(keys) => {
        Router::new()
          |> Router::post("/webhook", SharedFn::of(fn request =>
            with { clock: Clock::real() } { receive(keys, request) }))
          |> Router::listen(8080)!;
        0
      },
    }
  }
}
```

Try it from a shell. Any HMAC implementation computes the same signature, so
`openssl` stands in for the sender:

```sh
export WEBHOOK_SECRET=$(./webhook new-secret)
./webhook &
body='{"id":"evt_1","event":"paid"}'
t=$(date +%s)
sig=$(printf '%s.%s' "$t" "$body" | openssl dgst -sha256 -hmac "$WEBHOOK_SECRET" -r | cut -d' ' -f1)
curl -s -X POST -H "Webhook-Signature: t=$t,v1=$sig" -d "$body" localhost:8080/webhook    # accepted
curl -s -X POST -H "Webhook-Signature: t=$t,v1=$sig" -d "$body " localhost:8080/webhook   # bad signature
```

## Why the timestamp is signed

A signature over the body alone proves who sent a delivery, not when. Anybody
who records one signed delivery, from a proxy log, a misdirected retry or a
compromised intermediary, can send it again a month later and it still checks
out. Signing `<timestamp>.<body>` and refusing a timestamp more than five
minutes from the receiver's clock limits a recorded delivery to that window,
and the timestamp cannot be moved forward without the secret, because it is
part of what was signed.

The receiver checks the timestamp **before** the signature. A delivery
outside the window is refused without computing an HMAC, and the order does
not matter for correctness: both checks must pass.

The tolerance has to cover clock drift between the two machines and the
sender's retries. Five minutes is the usual choice. A sender that retries a
failed delivery signs it again with a new timestamp rather than resending the
old header.

## The window does not stop a replay inside it

Within the five minutes, the same delivery sent twice is accepted twice. If
processing a delivery twice would do harm, such as charging a card or sending
an email, the receiver must also remember which deliveries it has handled.
Put an event id in the body, which the signature covers, and record each id
when you process it, in the same transaction as the work if you can. Refuse
an id you have already seen. The window bounds how long you need to remember
an id: anything outside it is refused by its timestamp. The window reaches
five minutes either side of the receiver's clock, so a delivery can be
accepted for up to ten minutes; keep ids for at least twice the tolerance.

Neither check replaces the other. Remembering ids without a window means
remembering every id forever. A window without remembering ids lets a recorded
delivery through as often as it can be sent within five minutes.

## Sign the bytes you send, and check the bytes you received

The signature covers the body exactly as it travels. Sign the string you put
on the wire, not a value you will encode again afterwards, and on receipt
check `request.body` before you parse it. Re-encoding a parsed body can
reorder keys or change whitespace, and the signature then fails for a body
nobody altered.

## Check with `verify_sha256`, never with `==`

`Hmac::verify_sha256` computes the expected tag and compares the two in time
that does not depend on where they differ. Comparing the hex strings with `==`
returns sooner the earlier a guess goes wrong, and an attacker who can time
the responses can recover a valid tag one byte at a time.

Decode each `v1=` value with `Hex::decode` first. It accepts either case and
answers `None` for anything that is not hex, which is a refusal like any
other. A tag that is too short is refused too, so a sender cannot get a
truncated signature accepted. The header may carry several `v1=` values, and
the delivery is accepted if any of them checks out; that is how a sender
signs with two secrets during a rotation.

## Checking Stripe's webhooks

The format is Stripe's: Stripe signs `<timestamp>.<body>` with HMAC-SHA-256
and sends `t=<timestamp>,v1=<hex>`, with one `v1=` per active secret, and its
own libraries refuse a timestamp more than five minutes old. To check
deliveries from Stripe, change `signature_header()` to return
`"Stripe-Signature"` and set `WEBHOOK_SECRET` to the endpoint's signing secret
exactly as Stripe shows it, `whsec_` prefix included. Other providers use
different headers and messages; GitHub, for example, signs the body alone in
`X-Hub-Signature-256: sha256=<hex>`, with no timestamp, so this program does
not check GitHub's deliveries without changes.

## Keep the secret out of logs

The secret is a `Redacted<String>` from the moment it is read, and the HMAC
functions take the key as a `Redacted<Array<U8>>`. A `Redacted` value prints
as `<redacted>` in `print`, in an interpolation and inside a derived `Show`,
and a record holding one cannot derive `Encode`, so it cannot end up in a
response body or a JSON log line. `Redacted::expose` is the one way to the
value, and the program calls it in one place, `key`, where the secret becomes
the bytes the HMAC is computed under.

The program never prints a secret it was given. It prints one only when asked
with `new-secret`, which you run by hand at a terminal. A service that printed
a fresh secret whenever its configuration was missing would write a usable
secret into its own logs the first time it was deployed without one.

The secret is kept as a string because the router runs `receive` on several
fibers at once, and a handler shared that way may capture only values that
cannot be written. An `Array<U8>` can be written; a `String` cannot.

## Rotate a secret without dropping deliveries

Rotation is two deploys:

1. Generate a new secret. Deploy the receiver with the new one in
   `WEBHOOK_SECRET` and the old one in `WEBHOOK_SECRET_PREVIOUS`. It now
   accepts both.
2. Switch the sender to the new secret. Once no delivery signed with the old
   one can still arrive, which with a five-minute window means five minutes
   after the last retry, remove `WEBHOOK_SECRET_PREVIOUS` and deploy again.

The receiver never sees a signature it cannot check, so nothing is refused
during the switch.

## Generate secrets with `SecureRandom`

`SecureRandom::real()` reads the operating system's cryptographic generator.
32 bytes is the width of a SHA-256 tag, which is what RFC 2104 recommends as
the smallest HMAC key. URL-safe base64 makes the secret safe to paste into an
environment variable, a URL or a YAML file.

Do not use `std::random::Random` for a secret. It is fast and reproducible,
which suits a test or a shuffle, and anybody who sees one of its outputs can
predict the rest.

`SecureRandom` is a capability, so a test can pin the secret by supplying its
own handler:

```khora
let fixed = handler for SecureRandom {
  bytes: fn buffer => (),
};
let secret = with { random: fixed } { new_secret() };
// 32 zero bytes: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
```

The [`std::crypto` reference](/docs/stdlib/api/crypto/) has the full API,
including SHA-512, `constant_time_eq` for comparing tokens, and both base64
alphabets.
