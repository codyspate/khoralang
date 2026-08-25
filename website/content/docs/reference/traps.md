---
title: Traps
sidebar:
  order: 22
---

A trap represents a programming error or violated invariant rather than a recoverable domain failure.

Examples include arithmetic overflow and bounds failures. Traps are intentionally distinct from typed `raises` failures so ordinary APIs do not force callers to pretend bugs are expected business outcomes.

For command-line programs, terminating the process may be the appropriate response. Long-running servers and edge isolates need a defined containment policy so one bad request does not necessarily destroy unrelated work.

The production release requires the runtime to document exactly which traps are process-fatal, which may be contained at a fiber/request boundary, what cleanup runs before containment, and what diagnostic/backtrace information is emitted.

Do not use traps as an input-validation mechanism. Convert untrusted input into typed validation failures before performing operations whose invariants require valid data.
