# Public Documentation Plan

This is the content plan for the documentation published at `khoralang.com`.

The repository's `docs/` directory remains the home of compiler/runtime design records. Public docs live under `website/content/docs/` and are written for programmers using the language.

## Information architecture

```text
website/content/docs/
  index.md
  getting-started/
    installation.md
    first-project.md
    editor.md
  guide/
    values-and-functions.md
    modules-and-packages.md
    data-types.md
    pattern-matching.md
    collections-and-strings.md
    pipelines.md
    generics-and-traits.md
    errors-and-raises.md
    effects-and-capabilities.md
    resources-and-regions.md
    fibers-and-nurseries.md
    shared-state.md
    testing.md
  reference/
    lexical-structure.md
    grammar.md
    expressions.md
    types.md
    generics.md
    traits.md
    effects.md
    failures.md
    capabilities.md
    patterns.md
    memory-and-resources.md
    concurrency.md
    ffi.md
    traps.md
  stdlib/
    index.md
  cookbook/
    http-service.md
    json-api.md
    database-transactions.md
    bounded-concurrency.md
    cancellation-safe-resources.md
    tracing.md
    configuration.md
    testing-capabilities.md
  deployment/
    supported-targets.md
    linux.md
    containers.md
    cloudflare.md
  migration/
    from-typescript-effect.md
    from-go.md
    from-rust.md
  limitations/
    index.md
  project/
    release-readiness.md
    documentation-plan.md
    auto-documentation.md
    website-operations.md
```

The public content boundary is stable even if the frontend framework changes.

## Writing principles

### Teach the user model first

The public guide explains what a programmer writes and what behavior they can rely on. It should not make compiler implementation concepts prerequisites.

For example, typed failure begins from code shaped like:

```khora
fn load_user(id: Id) -> User raises DbError
```

and explains what callers must handle. Algebraic-effect lowering belongs in an optional design link, not in the first explanation.

Capabilities begin from what authority a function requires and how a handler supplies it. Fiber documentation begins from structured lifetime and cancellation, not coroutine stack mechanics.

### Distinguish tutorial, guide and reference

- **Getting Started** is linear and opinionated. A new developer follows it once.
- **Guide** explains how to accomplish ordinary work and builds a mental model.
- **Reference** is precise, complete and optimized for lookup rather than teaching order.
- **Cookbook** shows production patterns combining several language/library concepts.

Do not make one page try to serve all four purposes.

### Examples must be trustworthy

Examples intended to compile should be checked against the compiler version whose docs they appear in. A docs build should eventually fail when a checked example stops compiling.

If an example is illustrative pseudocode rather than valid Khora, label it explicitly. Public documentation must not train users on syntax the compiler does not accept.

### Planned behavior is not current behavior

Pages for unreleased functionality may exist under development docs, but must be marked clearly. Stable release docs describe what that compiler actually ships.

### Explain failure and cleanup paths

Khora's strongest ideas appear when things go wrong. Database, resource and concurrency pages should show cancellation, typed failure, cleanup and bounded concurrency where they materially affect correctness.

## Version model

The public documentation supports three views before the first stable release workflow is finalized:

```text
/docs/             current stable/current public docs
/docs/<version>/   immutable docs for a released compiler
/docs/next/        current development docs
```

A release tag should produce both compiler artifacts and the corresponding documentation snapshot. Links copied from a versioned reference page must not silently begin describing a different compiler after an upgrade.

## Automatic API documentation

Public `std` and package API documentation should be generated from the same source revision as the compiler/package being documented.

The intended compiler surface is:

```text
khora doc
khora doc --package
khora doc --stdlib
khora doc --format json
khora doc --check
```

The compiler owns extraction and resolution. The website consumes structured output rather than scraping Khora source text.

Each generated symbol should expose at least:

- symbol kind and fully qualified name;
- resolved declaration/signature;
- module;
- generics and trait bounds;
- effect/capability row;
- typed failure row;
- `///` Markdown documentation comment;
- source location;
- links to referenced symbols;
- checked examples where usage is not obvious.

`khora doc --check` compiles fenced examples that claim to be valid Khora. Generated API pages complement, rather than replace, the curated Guide and Cookbook.

The design rule is: **the compiler generates facts; humans write explanations.**

## Search

Search should index guide pages, language reference, generated standard-library/package symbols, and Cookbook pages. Exact symbols should rank their API page highly; conceptual searches such as “cancellation” or “raises” should prefer conceptual documentation over compiler internals.

## Website implementation

The public site uses Astro + Starlight. Canonical Markdown remains in `website/content/docs/`; `scripts/sync-docs.mjs` copies it into Starlight's framework-specific content tree for the build.

Astro renders a static site at `/docs`. A small Cloudflare Worker handles top-level routing and delegates static requests to a Workers Static Assets binding.

```text
website/content/docs/
        ↓
npm run sync:docs
        ↓
Astro + Starlight
        ↓
website/dist/
        ↓
Cloudflare Worker / Static Assets
        ↓
https://khoralang.com/docs/
```

The Worker is configured as the Custom Domain origin for `khoralang.com`. `/` redirects to `/docs/` until a separate public homepage is introduced.

## Required public pages before release

The first public release must not ship with only an API reference. Before release, the public site needs at minimum:

1. homepage/positioning;
2. installation;
3. first project;
4. language guide covering the core language model;
5. language reference;
6. standard-library reference;
7. production cookbook for HTTP, DB, tracing, cancellation and testing;
8. supported-target/deployment documentation;
9. known limitations;
10. compatibility/stability policy;
11. security reporting;
12. contribution/governance information;
13. release notes/download path.

The complete production gate is maintained in [Production Release Readiness](release-readiness.md).
