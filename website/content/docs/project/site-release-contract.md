---
title: Documentation release contract
---

A Khora compiler release and its documentation are one versioned product. Release automation must build compiler artifacts and the corresponding documentation snapshot from the same tagged revision.

`/docs/` points to current stable documentation. `/docs/<version>/` is immutable. `/docs/next/` describes development behavior and may change as main changes.

Examples marked as compilable must be checked against the compiler represented by that documentation tree. Generated API docs must come from the same source revision. A release must not silently change old versioned pages to match a newer compiler.
