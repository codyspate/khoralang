interface Env {
  ASSETS: Fetcher;
}

const DOCS_PREFIX = '/docs';
const HOMEPAGE_ASSET = '/homepage.txt';

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);

    // The product homepage has one public URL. These legacy/implementation
    // paths always canonicalize back to the site root.
    if (
      url.pathname === '/home' ||
      url.pathname === '/home/' ||
      url.pathname === '/home.html' ||
      url.pathname === HOMEPAGE_ASSET
    ) {
      return Response.redirect(new URL('/', url), 301);
    }

    // Keep the homepage payload as a non-HTML static asset internally. If the
    // asset itself were named *.html, Cloudflare's HTML asset handling could
    // canonicalize that internal request and create a redirect loop with the
    // public /home -> / rule above.
    if (url.pathname === '/') {
      const assetUrl = new URL(url);
      assetUrl.pathname = HOMEPAGE_ASSET;
      const homepage = await env.ASSETS.fetch(new Request(assetUrl, request));

      if (!homepage.ok) {
        return homepage;
      }

      const headers = new Headers(homepage.headers);
      headers.set('content-type', 'text/html; charset=utf-8');

      return new Response(homepage.body, {
        status: homepage.status,
        statusText: homepage.statusText,
        headers,
      });
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
