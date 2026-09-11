---
title: HTTP service
sidebar:
  order: 1
---

Khora's shipped HTTP router works directly with `Request`, `Response`, `Router`, and shareable handler functions. A small service does not need an application framework before it can route requests.

## Complete example

This service exposes `/health` and `/hello?name=...` and listens on port 8080:

```khora
module main;

import std::core::{ChildFailed, Option, SharedFn};
import std::net::http::{HttpError, Request, Response, Router};

fn health(_request: Request) -> Response {
  Response::text(200, "ok")
}

fn hello(request: Request) -> Response {
  let name = match request.query("name") {
    Option::Some(value) => value,
    Option::None => "world",
  };

  Response::text(200, "hello ${name}")
}

pub fn main()
  raises HttpError + ChildFailed
{
  Router::new()
    |> Router::get("/health", SharedFn::of(health))
    |> Router::get("/hello", SharedFn::of(hello))
    |> Router::listen(8080)!
}
```

A route handler is an ordinary direct-style function from `Request` to `Response`:

```khora
fn hello(request: Request) -> Response
```

`SharedFn::of` certifies the handler for the router's concurrent serving boundary. The router can then invoke the handler from request fibers without turning the handler into a special async type.

## Read request data once it reaches the handler

The HTTP layer parses the request before routing it. Handlers can read the normalized path, matched route parameters, query values, headers, and body directly from `Request`.

For example, a route with a path parameter can inspect it through `request.params`:

```khora
fn show_user(request: Request) -> Response {
  match request.params.get("id") {
    Option::Some(id) => Response::text(200, "user ${id}"),
    Option::None => Response::text(400, "missing id"),
  }
}
```

and mount it with:

```khora
Router::new()
  |> Router::get("/users/:id", SharedFn::of(show_user))
```

## Return transport decisions at the HTTP boundary

A handler should translate application outcomes into HTTP status codes and response bodies at the boundary. Domain functions below the handler can keep their own typed failures instead of knowing about status code 404 or 503.

For typed request/response bodies, continue with [JSON API](/docs/cookbook/json-api/). For failure translation before the HTTP boundary, see [Typed failure with raises](/docs/reference/failures/#translate-failure-types).

## Two verbs the router answers without being mounted

**A `GET` route is also a `HEAD` route.** The router runs the `GET` handler and
sends the headers alone — including the `Content-Length` of the body a `GET`
would have sent, which is the whole point of the method. RFC 9110 requires the
two answers to agree, and running the same handler is the only arrangement in
which they cannot drift. Nothing above needs a second mount for `curl -I`, a
cache, or a health checker that sends one.

**`OPTIONS` is answered with `204` and an `Allow` header** naming what the path
mounts, plus `OPTIONS` itself and `HEAD` wherever `GET` is mounted — the same
list a `405` gives, because a client cannot be told two things about one path
and act on both. A path nothing mounts is `404` rather than a `204` with an
empty `Allow`: "this path allows nothing" and "there is no such path" are
different answers.

Mount either explicitly and the default steps aside for that path. There are
two reasons to:

- `Router::head`, for a resource whose length or validators are cheap and whose
  body is not, where answering `HEAD` by building the body and throwing it away
  is the entire cost of the request. Mounting it separately gives up the
  guarantee that `HEAD` and `GET` agree, so it earns its keep only when that
  cost is real.
- `Router::options`, for CORS. The default answer is the correct reply to the
  question `OPTIONS` asks, but it carries no `Access-Control-Allow-Origin`: the
  library cannot know which origins a service trusts, and both "none" and "any"
  are wrong defaults. A browser calling this service from another origin sends
  a preflight first, and a handler mounted here is what answers it — including
  the `Allow` header the default would have sent, since the mount replaces the
  default rather than adding to it. The actual `GET` or `POST` response needs
  `Access-Control-Allow-Origin` too: the preflight authorises the request, it
  does not authorise the answer.

```khora
fn preflight(req: Request) -> Response {
  Response::text(204, "")
    |> Response::with_header("Access-Control-Allow-Origin", "https://app.example")
    |> Response::with_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
    |> Response::with_header("Access-Control-Allow-Headers", "content-type")
    |> Response::with_header("Allow", "GET, POST, HEAD, OPTIONS")
}
```

## State a handler shares between requests

A handler is called on a different fiber per connection, so anything they hold
in common has to be shareable. **`Shared<Dict<K, V>>` is the shape**, and it is
worth saying because the maps a handler already has in hand are not:
`request.params`, `request.queries` and `request.headers` are `Map`, `Map` is a
record with `mut` fields, and a mutable record cannot cross into a fiber:

```text
error: `Map<String, Int>` does not implement `Share`, which `Shared::of` requires
```

`Dict` is the persistent one — an insert gives back a new dictionary and leaves
the old one alone — so it holds nothing writable and goes into a cell. Build the
state before the router, and let each handler close over it:

```khora
module main;

import std::core::{ChildFailed, Dict, Option, Shared, SharedFn};
import std::net::http::{HttpError, Request, Response, Router};

/// One counter per name, in a cell every request fiber shares.
fn greet(visits: Shared<Dict<String, Int>>, request: Request) -> Response {
  let name = match request.query("name") {
    Option::Some(value) => value,
    Option::None => "world",
  };

  let seen = Shared::update(visits, fn table =>
    Dict::insert(table, name, match Dict::get(table, name) {
      Option::Some(n) => n + 1,
      Option::None => 1,
    }));

  match Dict::get(seen, name) {
    Option::Some(n) => Response::text(200, "hello ${name} (${n})"),
    Option::None => Response::text(500, "lost the count"),
  }
}

pub fn main()
  raises HttpError + ChildFailed
{
  let visits = Shared::of(Dict::new());

  Router::new()
    |> Router::get("/hello", SharedFn::of(fn request => greet(visits, request)))
    |> Router::listen(8080)!
}
```

`SharedFn::of` takes a closure literal, and the closure captures `visits` — so
the state reaches the handler as an ordinary parameter. A capability reaches one
the same way, which is the [parameter form](/docs/reference/capabilities/#a-capability-as-an-ordinary-parameter):
`SharedFn` has no capability row, so a handler that needs a database or an HTTP
client takes it as an argument and the closure closes over it.

`Shared::update` reads, changes and writes under one lock, which is what makes
the read-modify-write above correct when two requests for the same name arrive
at once; two separate `get` and `set` calls would not be. Use `Shared::modify`
when the change has something to report beyond the new state — the key it
generated, say.

**A cell is process-local.** It does not survive a restart and a second instance
does not see it, and [a trap in a handler ends the process](#a-trap-in-a-handler-ends-the-server).
So this is for what a restart can rebuild — a cache, a counter, a connection
registry — and anything else belongs in a database.

## Bound the resource that is actually constrained

**The server has one capacity number, not two.** `Router::listen` runs its
accept loop inside `bounded_nursery(256, ...)`, and an accepted connection is a
fiber that is inside your handler for as long as the handler runs. There is no
second, smaller pool that handlers queue for, so that one number is both the
most connections served at once and the most handlers running at once — 257
live, in fact, for the reason
[Bounded concurrency](/docs/cookbook/bounded-concurrency/) gives. It is not
configurable in this release.

That bound is not usually what limits throughput: a server saturates its cores
well before it runs out of connection slots, and past saturation, raising a
bound lengthens the queue while lowering it sheds load sooner — neither makes
the server faster. Where the saturation point actually is for your handler on
your machine is a measurement, and [Performance](/docs/performance/) sets out
what a number has to carry before it is worth quoting: the ladder, the
generator, the machine, the profile and the date. This page deliberately
prints none, because it had one and it carried none of them.

So bound the thing that is actually scarce. If a handler waits on a database
pool or a rate-limited API, put a smaller bound around *that* work rather than
lowering the connection limit, which would refuse connections that could have
been served. See [Bounded concurrency](/docs/cookbook/bounded-concurrency/).

The other number is per request rather than per connection, and it *is*
configurable. A router holds at most 8 KB of one request — headers and body
together — and answers `413` past it, before the handler runs.
`Router::holding(most)` sets another:

```khora
Router::new()
  |> Router::holding(1048576)
  |> Router::post("/documents", SharedFn::of(store))
  |> Router::listen(8080)!
```

The default is a policy about how much an unauthenticated client may make a
server hold, not a limit of the parser. The buffer is allocated once at that
size per connection, so the number you choose multiplies by the 256 above when
deciding what a full server costs — raise it to what the largest legitimate
document needs and not further.

## Stopping a service

`Router::listen` serves until the process stops, and `std` has no signal API in
this release — so a service that stops *itself* runs `listen` on a fiber and
lets go of that fiber when a route says to.

**The order matters, and getting it wrong ends the process rather than the
server.** Cancelling or detaching the listener while connections are still
being served aborts on `a cancellation reached a fiber's root`. Drain first and
detach last:

```khora
let stop = Shared::of(false);
let server = Fiber::spawn(fn () =>
  Router::new()
    |> Router::post("/shutdown", SharedFn::of(fn _r => {
        Shared::set(stop, true);
        Response::text(200, "stopping")
      }))
    |> Router::listen(port)!);

loop {
  clock.sleep(50);
  if Shared::get(stop) { break };
};

// Whatever the handlers feed: close it, and wait for the work already taken.
Channel::close(jobs);
Fiber::wait(worker);
// Only now.
Fiber::detach(server);
```

Two things are worth being deliberate about. A request that arrives during the
drain finds a closed channel, so `Channel::send` answers `false` — count it
where it is refused, or the job is accepted and never seen again; the same
reconciliation [taking work off a
queue](/docs/cookbook/taking-work-off-a-queue/) is about, one layer up. And a
`Fiber::spawn` that fails says nothing to anybody: a listener that could not
bind raises inside its own fiber, and a `main` that is polling a flag waits for
ever with an empty terminal. Write the port into the log line before you listen.

For a container, this is the in-program half only. Draining at the layer above
— out of the load balancer, wait, then stop — is what
[Containers](/docs/deployment/containers/) covers, and is what a `SIGTERM`
does today.

## A trap in a handler ends the server

The router turns a typed failure into a 500 rather than a dropped connection.
It cannot do that for a trap. A checked overflow, an index outside an array or
a division by zero does not unwind, so it never reaches the wrapper, and the
process exits with status 134 taking every in-flight connection with it.

The practical consequence is that request-shaped integers must be validated
before they are used in arithmetic, which is what [Decoding
input](/docs/cookbook/decoding-input/) is for, and that a service wants more
than one process behind it. [Traps](/docs/reference/traps/#what-this-means-for-a-server)
has the whole of it.

For the complete router, request, response, and client surface, see the [HTTP API reference](/docs/stdlib/api/net/http/).
