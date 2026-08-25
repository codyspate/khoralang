interface Env {
  ASSETS: Fetcher;
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);

    if (url.pathname === '/') {
      return Response.redirect(new URL('/docs/', url), 302);
    }

    return env.ASSETS.fetch(request);
  },
};
