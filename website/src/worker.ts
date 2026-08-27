interface Env {
  ASSETS: Fetcher;
}

const LEGACY_HOME_PATHS = new Set([
  '/home',
  '/home/',
  '/home.html',
  '/homepage.txt',
  '/docs/home',
  '/docs/home/',
  '/docs/home.html',
]);

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);

    if (LEGACY_HOME_PATHS.has(url.pathname)) {
      return Response.redirect(new URL('/', url), 301);
    }

    if (url.pathname === '/docs') {
      return Response.redirect(new URL('/docs/', url), 301);
    }

    // Astro now owns the real route structure: / is the product homepage and
    // Starlight content is generated under /docs/. The Worker only handles the
    // small canonical redirects above and otherwise serves the built asset as-is.
    return env.ASSETS.fetch(request);
  },
};
