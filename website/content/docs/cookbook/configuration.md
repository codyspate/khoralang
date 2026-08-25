---
title: Configuration
sidebar:
  order: 7
---

Treat configuration as input at the application boundary, not as ambient state read unpredictably from deep inside the program.

A production service should read environment variables, files, or platform bindings once; validate them into a typed configuration value; and pass or provide the capabilities built from that configuration to the rest of the application.

Fail startup early when required configuration is missing or invalid. A malformed database URL discovered on the first live request is harder to operate than the same problem reported before the service declares itself ready.

Keep secrets out of logs, tracing attributes, panic/trap messages, and generated diagnostics. Configuration types should distinguish secret values from ordinary displayable settings where practical.

For tests, construct configuration values directly instead of mutating process-global environment state unless the environment-reading boundary itself is what the test is exercising.
