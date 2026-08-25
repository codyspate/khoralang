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
    compatibility.md
    governance.md
    security.md
    contributing.md
```

File names may evolve when the site implementation lands, but these content families should remain recognizable. The URL contract should be stable even if the frontend framework changes.

## Writing principles

### Teach the user model first

The public guide explains what a programmer writes and what behavior they can rely on. It should not make compiler implementation concepts prerequisites.

For example, typed failure should begin from code shaped like:

```khora
fn load_user(id: Id) -> User raises DbError
```

and explain what callers must handle. Algebraic-effect lowering belongs in an optional design link, not in the first explanation.

Capabilities should begin from what authority a function requires and how a handler supplies it. Fiber documentation should begin from structured lifetime and cancellation, not coroutine stack mechanics.

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

Khora's strongest ideas appear when things go wrong. Examples should not teach only success paths. Database, resource and concurrency pages should show cancellation, typed failure, cleanup and bounded concurrency where they materially affect correctness.

## Version model

The public documentation should support three views:

```text
/docs/             current stable release
/docs/<version>/   immutable docs for a released compiler
/docs/next/        current development docs
```

A release tag should produce both compiler artifacts and the corresponding documentation snapshot. Links copied from a versioned reference page should not silently begin describing a different compiler after an upgrade.

The site should display the current documentation version prominently and make switching versions straightforward.

## Standard-library documentation

Public `std` API documentation should be generated or validated from the same source revision as the compiler release.

At minimum each exported API should expose:

- declaration/signature;
- module;
- documentation comment;
- effect/failure/capability requirements;
- important behavioral contracts;
- examples where usage is not obvious.

Generation must preserve curated conceptual pages. API generation is a reference mechanism, not a substitute for the language guide or cookbook.

## Search

Search should index at least:

- guide pages;
- language reference;
- standard-library symbols;
- cookbook pages.

Searching for an exact stdlib symbol should rank that API page highly. Searching for a concept such as “cancellation,” “transaction,” or “raises” should surface the conceptual guide before internal implementation material.

## Cloudflare deployment

`khoralang.com` is intended to run on Cloudflare infrastructure. The site implementation should satisfy these properties regardless of framework:

- deployment is automated from CI;
- the repository is the source of truth;
- the deployed build records its Git revision;
- production deploys are associated with release tags where applicable;
- preview deployments can be produced for documentation pull requests;
- redirects/version URLs are controlled in source;
- site search and static assets do not require an undocumented external build step.

Cloudflare Workers, Pages, or a combination may implement the site. Choosing between them is a website implementation decision, not a language-design decision.

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
