---
title: JSON APIs
sidebar:
  order: 6
---

Use typed request and response records at the HTTP boundary. `std::json` parses wire text into `Json`, `FromJson` decodes that value into a Khora type, and `ToJson` encodes a typed value for the response.

## Complete example

This service accepts `POST /users` with a JSON body such as `{"name":"Ada"}` and returns a typed user as JSON:

```khora
module main;

import std::core::{ChildFailed, Result, SharedFn};
import std::json::{DecodeError, FromJson, ToJson, decode, parse};
import std::net::http::{HttpError, Request, Response, Router};

derive(ToJson, FromJson)
pub type CreateUser = {
  name: String,
};

derive(ToJson, FromJson)
pub type User = {
  id: Int,
  name: String,
};

fn create_user(request: Request) -> Response {
  let document = match parse(request.body) {
    Result::Err(_) =>
      return Response::text(400, "body is not valid JSON"),

    Result::Ok(value) => value,
  };

  let input: CreateUser = decode(document)! catch {
    DecodeError::At(_, _, _) =>
      return Response::text(422, "JSON does not match CreateUser"),
  };

  let user: User = {
    id: 1,
    name: input.name,
  };

  Response::json(201, user.to_json())
}

pub fn main()
  raises HttpError + ChildFailed
{
  Router::new()
    |> Router::post("/users", SharedFn::of(create_user))
    |> Router::listen(8080)!
}
```

The boundary handles two distinct failures separately. `parse` returns `Result::Err` when the body is not JSON at all, so the handler answers `400`. `decode` raises `DecodeError` when the document is valid JSON but has the wrong shape for `CreateUser`, so the handler answers `422`.

## Derive when the wire shape matches the type

These declarations:

```khora
derive(ToJson, FromJson)
pub type CreateUser = {
  name: String,
};
```

ask the compiler to generate the structural JSON codec. This is the right default when the API representation intentionally matches the Khora record.

When the external API has compatibility aliases, different field names, legacy shapes, or validation rules that are part of the wire contract, implement `ToJson` or `FromJson` explicitly instead of forcing the domain type to look like the wire format.

## Parsing and decoding are separate operations

Keep this distinction visible:

```khora
let document = parse(request.body);       // Result<Json, JsonError>
let input: CreateUser = decode(json)!;    // raises DecodeError
```

A malformed document and a well-formed document with the wrong fields are different client mistakes. Keeping them separate also gives an API boundary enough information to choose different responses without string-parsing an error message.

## Encode typed responses

`ToJson` turns the response value into the standard JSON data model:

```khora
user.to_json()
```

`Json` implements `Show`, so the HTTP response helper can serialize it:

```khora
Response::json(201, user.to_json())
```

For more complex response policies you can call `std::json::encode` directly and construct the HTTP response explicitly.

For the complete JSON surface, see the [JSON API reference](/docs/stdlib/api/json/). For HTTP routing and request data, see [HTTP service](/docs/cookbook/http-service/).
