// The four TechEmpower read tests on node:http and node-postgres (`pg`).
//
// One process, one Pool of 16: the same work as the Khora, Go and Bun
// servers beside it. `pg` does not prepare statements unless a query is
// given a name, and none is here -- the setting is "whatever the driver does
// by default", for every server.
import http from "node:http";
import pg from "pg";

const env = (name, fallback) => process.env[name] || fallback;

const pool = new pg.Pool({
  host: env("PGHOST", "127.0.0.1"),
  port: Number(env("PGPORT", "5432")),
  user: env("PGUSER", "benchmarkdbuser"),
  password: env("PGPASSWORD", "benchmarkdbpass"),
  database: env("PGDATABASE", "hello_world"),
  max: Number(env("POOL", "16")),
});

const WORLD = "SELECT id, randomnumber FROM world WHERE id = $1";
const FORTUNES = "SELECT id, message FROM fortune";

// node:http writes Date itself; Server it leaves to the application.
function send(res, type, body) {
  res.writeHead(200, {
    Server: "node",
    "Content-Type": type,
    "Content-Length": Buffer.byteLength(body),
  });
  res.end(body);
}

function fail(res, err) {
  res.writeHead(500, { Server: "node", "Content-Type": "text/plain" });
  res.end(String(err));
}

async function randomWorld() {
  const id = 1 + Math.floor(Math.random() * 10000);
  const { rows } = await pool.query(WORLD, [id]);
  return { id: rows[0].id, randomNumber: rows[0].randomnumber };
}

// TechEmpower's rule: missing, not an integer or below one is one; above
// 500 is 500.
function queriesOf(search) {
  const n = Number.parseInt(new URLSearchParams(search).get("queries"), 10);
  if (!Number.isInteger(n) || n < 1) return 1;
  return n > 500 ? 500 : n;
}

// Node has no HTML template engine in its standard library, and a Node team
// writing a page this small would escape by hand rather than add one. The
// entities are the ones TechEmpower's example page uses, so this page is
// byte-for-byte theirs.
const ESCAPES = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&apos;" };
const escapeHtml = (text) => text.replace(/[&<>"']/g, (c) => ESCAPES[c]);

function page(fortunes) {
  let html =
    "<!DOCTYPE html><html><head><title>Fortunes</title></head><body><table>" +
    "<tr><th>id</th><th>message</th></tr>";
  for (const f of fortunes) {
    html += `<tr><td>${f.id}</td><td>${escapeHtml(f.message)}</td></tr>`;
  }
  return html + "</table></body></html>";
}

async function handle(req, res) {
  const at = req.url.indexOf("?");
  const path = at < 0 ? req.url : req.url.slice(0, at);
  const search = at < 0 ? "" : req.url.slice(at + 1);
  try {
    if (path === "/json") {
      send(res, "application/json", JSON.stringify({ message: "Hello, World!" }));
    } else if (path === "/db") {
      send(res, "application/json", JSON.stringify(await randomWorld()));
    } else if (path === "/queries") {
      const n = queriesOf(search);
      const worlds = [];
      // One after another, as every server here does them.
      for (let i = 0; i < n; i++) worlds.push(await randomWorld());
      send(res, "application/json", JSON.stringify(worlds));
    } else if (path === "/fortunes") {
      const { rows } = await pool.query(FORTUNES);
      const fortunes = rows.map((r) => ({ id: r.id, message: r.message }));
      fortunes.push({ id: 0, message: "Additional fortune added at request time." });
      // Code-unit order, the same byte-for-byte order as the others for this
      // data: every message is BMP text, and UTF-16 and UTF-8 agree there.
      fortunes.sort((a, b) => (a.message < b.message ? -1 : a.message > b.message ? 1 : 0));
      send(res, "text/html; charset=utf-8", page(fortunes));
    } else {
      res.writeHead(404, { Server: "node", "Content-Type": "text/plain" });
      res.end("not found");
    }
  } catch (err) {
    fail(res, err);
  }
}

http.createServer(handle).listen(Number(env("PORT", "8080")), "0.0.0.0");
