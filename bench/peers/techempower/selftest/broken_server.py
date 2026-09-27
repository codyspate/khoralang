"""A deliberately wrong TechEmpower server, to show verify.py can fail.

    python3 broken_server.py PORT FAULT

FAULT is one of: none, unescaped, no-server, cached-date, no-clamp,
cached-world, in-batch, content-type, string-ids, order-by. `none` is a
correct server and must pass; each other one breaks exactly one rule, and
verify.py must fail on it. Standard library only (psql for the queries).
"""
import http.server
import json
import os
import random
import subprocess
import sys
from email.utils import formatdate
from urllib.parse import parse_qs, urlparse

PORT, FAULT = int(sys.argv[1]), sys.argv[2]
PSQL = os.environ["PSQL"]


def q(sql):
    out = subprocess.run([PSQL, "-h", "127.0.0.1", "-U", "benchmarkdbuser", "-d", "hello_world", "-At", "-F", "\t", "-c", sql],
                         capture_output=True, text=True, env=dict(os.environ, PGPASSWORD="benchmarkdbpass"), check=True)
    return [line.split("\t") for line in out.stdout.splitlines()]


CACHED_DATE = formatdate(usegmt=True)
CACHED_WORLD = None


def world():
    global CACHED_WORLD
    if FAULT == "cached-world" and CACHED_WORLD:
        return CACHED_WORLD
    row = q("SELECT id, randomnumber FROM world WHERE id = %d" % random.randint(1, 10000))[0]
    CACHED_WORLD = {"id": int(row[0]), "randomNumber": int(row[1])}
    if FAULT == "string-ids":
        return {"id": row[0], "randomNumber": row[1]}
    return CACHED_WORLD


def escape(s):
    if FAULT == "unescaped":
        return s
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;").replace("'", "&#39;")


class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def send(self, body, ctype):
        body = body.encode("utf-8")
        self.send_response_only(200)
        if FAULT != "no-server":
            self.send_header("Server", "broken")
        self.send_header("Date", CACHED_DATE if FAULT == "cached-date" else formatdate(usegmt=True))
        self.send_header("Content-Type", "text/plain" if FAULT == "content-type" else ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        url = urlparse(self.path)
        if url.path == "/json":
            self.send(json.dumps({"message": "Hello, World!"}, separators=(",", ":")), "application/json")
        elif url.path == "/db":
            self.send(json.dumps(world(), separators=(",", ":")), "application/json")
        elif url.path == "/queries":
            raw = parse_qs(url.query).get("queries", [""])[0]
            try:
                n = int(raw)
            except ValueError:
                n = 1
            if FAULT != "no-clamp":
                n = max(1, min(500, n))
            if FAULT == "in-batch":
                ids = ",".join(str(random.randint(1, 10000)) for _ in range(n))
                worlds = [{"id": int(a), "randomNumber": int(b)} for a, b in q("SELECT id, randomnumber FROM world WHERE id IN (%s)" % ids)]
                while len(worlds) < n:
                    worlds.append(worlds[0])
            else:
                worlds = [world() for _ in range(max(n, 0))]
            self.send(json.dumps(worlds, separators=(",", ":")), "application/json")
        elif url.path == "/fortunes":
            rows = [(int(a), b) for a, b in q("SELECT id, message FROM fortune" + (" ORDER BY id" if FAULT == "order-by" else ""))]
            rows.append((0, "Additional fortune added at request time."))
            if FAULT != "order-by":
                rows.sort(key=lambda r: r[1].encode("utf-8"))
            page = ("<!DOCTYPE html><html><head><title>Fortunes</title></head><body><table><tr><th>id</th><th>message</th></tr>"
                    + "".join("<tr><td>%d</td><td>%s</td></tr>" % (i, escape(m)) for i, m in rows)
                    + "</table></body></html>")
            self.send(page, "text/html; charset=utf-8")
        else:
            self.send_error(404)


http.server.ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
