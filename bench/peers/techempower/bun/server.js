// The four TechEmpower read tests on Bun.serve and Bun's built-in Postgres
// client, Bun.sql.
//
// One process, one pool of 16. Bun.sql prepares statements by default, and
// that default is kept: the setting is "whatever the driver does by
// default", for every server.
import { SQL } from "bun";

const env = (name, fallback) => process.env[name] || fallback;

const sql = new SQL({
  hostname: env("PGHOST", "127.0.0.1"),
  port: Number(env("PGPORT", "5432")),
  username: env("PGUSER", "benchmarkdbuser"),
  password: env("PGPASSWORD", "benchmarkdbpass"),
  database: env("PGDATABASE", "hello_world"),
  max: Number(env("POOL", "16")),
});

// Bun.serve writes Date itself; Server it leaves to the application.
const headers = (type) => ({ Server: "bun", "Content-Type": type });

async function randomWorld() {
  const id = 1 + Math.floor(Math.random() * 10000);
  const rows = await sql`SELECT id, randomnumber FROM world WHERE id = ${id}`;
  return { id: rows[0].id, randomNumber: rows[0].randomnumber };
}

// TechEmpower's rule: missing, not an integer or below one is one; above
// 500 is 500.
function queriesOf(url) {
  const n = Number.parseInt(url.searchParams.get("queries"), 10);
  if (!Number.isInteger(n) || n < 1) return 1;
  return n > 500 ? 500 : n;
}

// Bun has an HTML escaper built in, which is what a Bun team would use.
function page(fortunes) {
  let html =
    "<!DOCTYPE html><html><head><title>Fortunes</title></head><body><table>" +
    "<tr><th>id</th><th>message</th></tr>";
  for (const f of fortunes) {
    html += `<tr><td>${f.id}</td><td>${Bun.escapeHTML(f.message)}</td></tr>`;
  }
  return html + "</table></body></html>";
}

Bun.serve({
  port: Number(env("PORT", "8080")),
  hostname: "0.0.0.0",
  async fetch(req) {
    const url = new URL(req.url);
    try {
      switch (url.pathname) {
        case "/json":
          return new Response(JSON.stringify({ message: "Hello, World!" }), {
            headers: headers("application/json"),
          });
        case "/db":
          return new Response(JSON.stringify(await randomWorld()), {
            headers: headers("application/json"),
          });
        case "/queries": {
          const n = queriesOf(url);
          const worlds = [];
          // One after another, as every server here does them.
          for (let i = 0; i < n; i++) worlds.push(await randomWorld());
          return new Response(JSON.stringify(worlds), { headers: headers("application/json") });
        }
        case "/pipelined-queries": {
          // Every lookup issued on one reserved connection before any is
          // awaited: Bun.sql writes them in one go, each its own statement
          // with its own Sync, which is what TechEmpower's rule 7 asks of
          // pipelining. One connection, as Khora's lease and pgx's batch.
          const n = queriesOf(url);
          const conn = await sql.reserve();
          try {
            const answers = await Promise.all(
              Array.from({ length: n }, () => {
                const id = 1 + Math.floor(Math.random() * 10000);
                return conn`SELECT id, randomnumber FROM world WHERE id = ${id}`;
              }),
            );
            const worlds = answers.map((rows) => ({ id: rows[0].id, randomNumber: rows[0].randomnumber }));
            return new Response(JSON.stringify(worlds), { headers: headers("application/json") });
          } finally {
            conn.release();
          }
        }
        case "/fortunes": {
          const rows = await sql`SELECT id, message FROM fortune`;
          const fortunes = rows.map((r) => ({ id: r.id, message: r.message }));
          fortunes.push({ id: 0, message: "Additional fortune added at request time." });
          fortunes.sort((a, b) => (a.message < b.message ? -1 : a.message > b.message ? 1 : 0));
          return new Response(page(fortunes), { headers: headers("text/html; charset=utf-8") });
        }
        default:
          return new Response("not found", { status: 404, headers: headers("text/plain") });
      }
    } catch (err) {
      return new Response(String(err), { status: 500, headers: headers("text/plain") });
    }
  },
});
