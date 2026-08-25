---
title: Testing capabilities
sidebar:
  order: 8
---

Capabilities make an application's external authority replaceable at a scoped boundary.

For a unit test, provide the smallest handler that records or returns the behavior the test needs. Avoid building a second production implementation merely to create a test double.

Useful test handlers include:

- a deterministic clock;
- an in-memory database/repository;
- a tracer that records finished spans;
- a fake external service with scripted responses;
- a filesystem boundary backed by a temporary directory.

Assert on the contract that matters to the caller. For a transaction helper, record `begin`, `commit`, and `rollback` operations and verify the correct transcript for success, failure, and cancellation.

Because capability requirements are in the function type, a test can see what external authority the subject needs without first executing it and discovering hidden globals.
