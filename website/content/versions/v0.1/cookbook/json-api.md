---
title: JSON APIs
sidebar:
  order: 6
---

Use typed request and response records at the HTTP boundary. `std::json`
parses wire text into `Json`, a derived schema reads a Khora type out of it
and reports every problem, and `Encode` writes a typed value back for the
response.

## Complete example

This service accepts `POST /users` with a JSON body such as `{"name":"Ada"}`
and returns a typed user as JSON:

```khora
module main;

import std::core::{ChildFailed, Result, SharedFn, Validated};
import std::json::{parse};
import std::net::http::{HttpError, Request, Response, Router};
import std::schema::{Decode, Encode, Raw, Rejection};

derive(Decode)
pub type CreateUser = {
  name: String,
};

derive(Encode)
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

  let input = match CreateUser::schema().decode(Raw::of_json(document)) {
    Validated::Invalid(problems) =>
      return Response::json(422, problems),

    Validated::Valid(input) => input,
  };

  let user: User = {
    id: 1,
    name: input.name,
  };

  Response::json(201, user)
}

pub fn main()
  raises HttpError + ChildFailed
{
  Router::new()
    |> Router::post("/users", SharedFn::of(create_user))
    |> Router::listen(8080)!
}
```

The boundary handles two distinct failures separately. `parse` returns
`Result::Err` when the body is not JSON at all, so the handler answers `400`.
`decode` answers `Validated::Invalid` when the document is valid JSON but has
the wrong shape for `CreateUser`, so the handler answers `422` — with every
problem, because a `Rejection` encodes as an object with its `path` and its
`message`, and a client wants the list rather than the first line of it:

```json
[{"message":"name should be text, and is 7","path":"name"}]
```

## Derive when the wire shape matches the type

These declarations:

```khora
derive(Decode)
pub type CreateUser = {
  name: String,
};
```

ask the compiler to write the schema from the type. This is the right default
when the API representation intentionally matches the Khora record.

When the external API has different field names, a rule that is part of the
wire contract, or a legacy shape, implement `Decode` for that record by hand
with a `struct({ .. })` literal, and every schema that contains it picks the
impl up through the trait. [Decode untrusted input](/docs/cookbook/decoding-input/)
has the whole story.

## Parsing and decoding are separate operations

Keep this distinction visible:

```khora
let document = parse(request.body);                       // Result<Json, JsonError>
let input = CreateUser::schema().decode(Raw::of_json(document));  // Validated<CreateUser, Rejection>
```

A malformed document and a well-formed document with the wrong fields are
different client mistakes. Keeping them separate also gives an API boundary
enough information to choose different responses without string-parsing an
error message.

## Encode typed responses

`Response::json` takes anything that implements `Encode`: a derived record, a
`Json` built by hand, a list of rejections.

```khora
Response::json(201, user)
```

A record holding a `Redacted` has no `Encode`, so it cannot be sent by
accident; the build stops at the `derive` line.

For more complex response policies you can call `std::json::encode` on
`Raw::to_json(value.encode())` directly and construct the HTTP response
explicitly.

For the complete JSON surface, see the [JSON API reference](/docs/stdlib/api/json/).
For HTTP routing and request data, see [HTTP service](/docs/cookbook/http-service/).
