---
title: JSON APIs
sidebar:
  order: 6
---

Use typed domain/request structures at the HTTP boundary and JSON traits for serialization rather than assembling JSON strings manually.

A typical boundary should:

1. enforce transport/body size limits;
2. parse JSON;
3. decode into a typed request value;
4. validate domain rules;
5. call application logic;
6. map typed failures to an HTTP response;
7. encode a typed response.

Derive `ToJson` and `FromJson` when the external representation matches the Khora data shape. Write an explicit codec when field names, compatibility rules, or validation mean the wire representation deserves an API of its own.

Do not represent exact monetary values as floating-point JSON numbers unless the external contract explicitly requires that loss of decimal semantics. Prefer an agreed decimal/string representation at the service boundary and convert into `Decimal` immediately.

Keep transport errors separate from domain failures. Malformed JSON is not the same failure as a valid request for a nonexistent account, even if both eventually become non-2xx responses.
