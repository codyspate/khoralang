---
title: Documentation framework choice
---

The public documentation site uses Astro + Starlight.

Starlight is a documentation-focused layer over Astro that provides navigation, search-ready content structure, accessible documentation layouts, Markdown/MDX support, and room for custom components without turning documentation into a custom web application.

Cloudflare Workers serves the static build through Workers Static Assets. Workers Sites is not used because Cloudflare has deprecated that path for new projects.

The architectural boundary is intentionally above the framework: canonical public Markdown remains under `website/content/docs/`. A build-time sync feeds Starlight's `src/content/docs/` collection. If the site framework changes later, public content does not move and public URL design does not need to change.

The Worker remains small and site-level. It handles routing/redirect concerns and delegates static documents/assets to Cloudflare. Future homepage, download, package-index, or documentation-version routing can be added without coupling those features to the Khora compiler.
