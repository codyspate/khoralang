# Deploying khoralang.com documentation

The documentation site is an Astro + Starlight static build deployed as Cloudflare Workers Static Assets.

```bash
cd website
npm install
npm run dev      # local docs server
npm run build    # render static site
npm run deploy   # build + wrangler deploy
```

Public Markdown source lives only in `content/docs/`. The build syncs that tree into Starlight's framework-specific `src/content/docs/` location. Do not edit the generated copy.

Production CI uses `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` GitHub secrets. The API token should be narrowly scoped to Workers deployment for the account/zone.

The intended URL layout is:

```text
https://khoralang.com/                 -> /docs/
https://khoralang.com/docs/            current docs
https://khoralang.com/docs/<version>/  immutable release docs (before first release)
https://khoralang.com/docs/next/       development docs (before first release)
```
