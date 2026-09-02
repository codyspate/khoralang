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

## Bound the resource that is actually constrained

**The server has one capacity number, not two.** `Router::listen` accepts at
most 256 connections at once, and an accepted connection is a fiber that is
inside your handler for as long as the handler runs. There is no second,
smaller pool that handlers queue for, so 256 is both the most connections
served at once and the most handlers running at once.

That bound is not usually what limits throughput. Measured on a 16-core
desktop, the server saturates by about 16 connections: from there to 128 the
rate is flat while the median request slows in proportion to the queue behind
it. Past saturation, raising the bound lengthens the queue and lowers it sheds
load sooner; neither makes the server faster.

So bound the thing that is actually scarce. If a handler waits on a database
pool or a rate-limited API, put a smaller bound around *that* work rather than
lowering the connection limit, which would refuse connections that could have
been served. See [Bounded concurrency](/docs/cookbook/bounded-concurrency/).

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
