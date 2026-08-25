---
title: Traits
sidebar:
  order: 7
---

Traits describe behavior that types can provide and that generic code can require.

Common standard traits include equality, ordering, display, hashing, and JSON conversion. Derivation is available for traits whose implementation can be generated from the data definition.

Trait resolution is scoped. An implementation is only reachable through an operator or method when the relevant trait is in scope at the expression site. This keeps method/operator meaning tied to explicit module imports rather than to a global registry of all traits in the program.

Trait bounds belong on generic APIs that actually require the behavior. Over-constraining a public generic makes valid callers impossible for no semantic reason.

Higher-kinded traits allow abstractions over type constructors. Exact associated-item and higher-kinded syntax is documented as the public reference expands; compiler diagnostics and the implemented grammar remain authoritative during pre-release development.
