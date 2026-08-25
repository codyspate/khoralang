---
title: Website operations
---

The public documentation site is sourced from `website/content/docs/` and published at `https://khoralang.com/docs/`.

## Local development

```bash
cd website
npm install
npm run dev
```

The build runs `scripts/sync-docs.mjs` first. That copies the framework-independent Markdown source into Starlight's generated working tree under `src/content/docs/`. Do not edit the generated copy.

## Production build

```bash
npm run build
```

Astro renders a static site into `website/dist/`. Wrangler uploads that directory as Workers Static Assets.

## Production deployment

```bash
npm run deploy
```

The Worker is the origin for `khoralang.com`. Its small entrypoint handles top-level redirects and delegates documentation/static requests to the `ASSETS` binding.

The repository workflow builds documentation changes on pull requests and deploys `main` through the protected `production-docs` GitHub environment.

Required GitHub secrets:

- `CLOUDFLARE_API_TOKEN` — a narrowly scoped token allowed to deploy this Worker;
- `CLOUDFLARE_ACCOUNT_ID` — the Cloudflare account containing the `khoralang.com` zone.

Do not use a Global API Key.

## Domain

Wrangler declares `khoralang.com` as a Cloudflare Workers Custom Domain. Cloudflare should own the zone and the hostname must not already be occupied by a conflicting CNAME. Custom Domains let Cloudflare provision the DNS record and certificate for the Worker origin.

## Versioned docs

The initial site serves development documentation from `/docs/`. Before the first public compiler release, the release workflow must snapshot documentation so `/docs/<version>/` remains immutable while `/docs/next/` tracks development and `/docs/` points at the current stable release.

## Generated stdlib docs

When `khora doc` lands, the site build gains a generation step before Astro runs:

```text
khora doc --stdlib --format json
        ↓
website generated API data
        ↓
Starlight pages/components
```

Generated files are build artifacts. Curated Markdown under `website/content/docs/` remains hand-authored source.
