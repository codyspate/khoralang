interface Env {
  ASSETS: Fetcher;
}

const DOCS_PREFIX = '/docs';

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);

    // The product homepage is a standalone static page. Starlight remains
    // mounted under /docs so every existing documentation URL stays stable.
    if (url.pathname === '/') {
      const assetUrl = new URL(url);
      assetUrl.pathname = '/home.html';
      return env.ASSETS.fetch(new Request(assetUrl, request));
    }

    if (url.pathname === DOCS_PREFIX) {
      return Response.redirect(new URL('/docs/', url), 301);
    }

    if (url.pathname.startsWith(`${DOCS_PREFIX}/`)) {
      const assetUrl = new URL(url);
      assetUrl.pathname = url.pathname.slice(DOCS_PREFIX.length) || '/';
      return env.ASSETS.fetch(new Request(assetUrl, request));
    }

    // Public homepage assets such as /home.css and /favicon.svg live at the
    // site root and should not be forced through the documentation prefix.
    return env.ASSETS.fetch(request);
  },
};
