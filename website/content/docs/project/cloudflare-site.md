---
title: khoralang.com architecture
---

The public site is designed so documentation content remains independent from the frontend framework.

```text
website/content/docs/      canonical Markdown
        ↓ sync:docs
website/src/content/docs/  generated Starlight input
        ↓ astro build
website/dist/              static site
        ↓ wrangler deploy
Cloudflare Worker + Static Assets
        ↓
https://khoralang.com/docs/
```

A tiny Worker handles site-level routing and delegates static requests to Cloudflare's asset binding. This leaves room for a future khoralang.com homepage, downloads API, package search, or version redirects without replacing the documentation renderer.

Cloudflare Workers is the origin for the hostname, so a Custom Domain is preferred over a route in front of another origin.

The repository remains the source of truth. No documentation should exist only in the Cloudflare dashboard, and production redirects/routing should be committed with the site configuration.
