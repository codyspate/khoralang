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

The router already owns request fibers. If a downstream resource has a smaller capacity—for example a database pool—bound concurrency around that work rather than treating the total number of HTTP connections as the same limit. See [Bounded concurrency](/docs/cookbook/bounded-concurrency/).

For the complete router, request, response, and client surface, see the [HTTP API reference](/docs/stdlib/api/net/http/).
