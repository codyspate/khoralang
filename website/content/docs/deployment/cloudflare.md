---
title: Cloudflare Workers
sidebar:
  order: 2
---

Khora's public documentation site is itself deployed to Cloudflare Workers. The language's future Cloudflare runtime target is a separate concern; this page describes the documentation website and the release expectations for a future Khora Worker target.

## Documentation website

The source of truth is `website/content/docs/`. Astro + Starlight builds the static documentation site, and Wrangler deploys the generated assets with a small Worker entrypoint.

```bash
cd website
npm install
npm run dev
npm run build
npm run deploy
```

The production Worker is configured as the origin for `khoralang.com`. Requests to `/` redirect to `/docs/`; Starlight owns the `/docs` URL space.

Cloudflare manages the custom-domain DNS/certificate when the Worker is attached as a Custom Domain.

## CI credentials

Automated deployment should use a narrowly scoped Cloudflare API token stored as a repository secret. Do not commit account IDs, API tokens, or other credentials into the website directory.

Production deployments should come from the release/deployment workflow rather than a developer laptop. Pull requests may use preview Workers or build-only checks without touching the production custom domain.

## Khora applications on Workers

A future supported Khora `wasm32-unknown-unknown`/Workers target must use Worker host capabilities rather than native Linux assumptions. Networking, persistence, and request lifecycle are host-provided; native sockets and a local filesystem are not part of that environment.

The target will only be documented as supported when the compiler, runtime/std split, bindings, packaging, and deploy example all work end to end.
