// Bun's own `Bun.serve`, answering what `bench/service` answers.
//
// `Bun.serve` rather than `node:http`, which Bun also runs: the comparison
// worth making is against what a team would actually write, and nobody
// reaches for the compatibility layer when the native one is the headline.
const port = Number(process.argv[2]);
const body = '{"status":"ok"}';
const ok = { "Content-Type": "application/json" };

Bun.serve({
  port,
  hostname: "127.0.0.1",
  fetch(request) {
    if (new URL(request.url).pathname === "/health") {
      return new Response(body, { headers: ok });
    }
    return new Response(null, { status: 404 });
  },
});
console.log("listening on " + port);
