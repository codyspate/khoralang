interface Env {
  ASSETS: Fetcher;
}

const DOCS_PREFIX = '/docs';

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);

    if (url.pathname === '/') {
      return Response.redirect(new URL('/docs/', url), 302);
    }

    if (url.pathname === DOCS_PREFIX) {
      return Response.redirect(new URL('/docs/', url), 301);
    }

    if (url.pathname.startsWith(`${DOCS_PREFIX}/`)) {
      const assetUrl = new URL(url);
      assetUrl.pathname = url.pathname.slice(DOCS_PREFIX.length) || '/';

      return env.ASSETS.fetch(new Request(assetUrl, request));
    }

    return new Response('Not Found', { status: 404 });
  },
};
