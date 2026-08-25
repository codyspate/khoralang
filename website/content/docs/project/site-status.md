---
title: Documentation site status
---

The documentation site scaffold is intentionally separate from the language compiler and from repository-internal design docs.

Implemented on the documentation branch:

- Astro + Starlight site shell;
- public Markdown source under `website/content/docs/`;
- source sync step into Starlight's content tree;
- Cloudflare Workers configuration;
- Worker entrypoint for top-level routing;
- GitHub Actions build/deploy workflow;
- Getting Started, core Guide, core Reference, stdlib overview, production Cookbook, Deployment, Migration, Limitations, and project operations content.

Still release work:

- `khora doc` and generated per-symbol stdlib/package pages;
- package-manager lockfile once site dependency versions are finalized;
- complete language-reference coverage for every syntax/typing rule;
- binary installer/download pages once release artifacts exist;
- immutable version snapshots under `/docs/<version>/`;
- production Cloudflare secrets/environment setup and first deployment;
- preview deployment policy for pull requests;
- public governance/security/contribution pages once those policies are finalized.
