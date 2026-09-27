"""TechEmpower's verification rules, applied to one running server.

    python3 verify.py --port 8080 --label khora-threads

A port of what TechEmpower's own verifier checks for the four tests run here
(toolset/test_types/{json,db,query,fortune} and verifications.py in
TechEmpower/FrameworkBenchmarks), with the database side done through
pg_stat_statements the way theirs is. Standard library only.

Where this is *stricter* than TechEmpower's it says so beside the check:
their verifier downgrades some findings to warnings, and a warning here is a
failure, because a server that differs from the others in any of these ways
is not doing the same work.

Exits 0 when every check passes, 1 otherwise.
"""
import argparse
import http.client
import json
import os
import re
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone
from html.parser import HTMLParser

# ---------------------------------------------------------------------------
# The Fortunes page, and TechEmpower's normalising parser, verbatim in effect
# ---------------------------------------------------------------------------

VALID_FORTUNE = """<!doctype html><html>
<head><title>Fortunes</title></head>
<body><table>
<tr><th>id</th><th>message</th></tr>
<tr><td>11</td><td>&lt;script&gt;alert(&quot;This should not be displayed in a browser alert box.&quot;);&lt;/script&gt;</td></tr>
<tr><td>4</td><td>A bad random number generator: 1, 1, 1, 1, 1, 4.33e+67, 1, 1, 1</td></tr>
<tr><td>5</td><td>A computer program does what you tell it to do, not what you want it to do.</td></tr>
<tr><td>2</td><td>A computer scientist is someone who fixes things that aren&apos;t broken.</td></tr>
<tr><td>8</td><td>A list is only as strong as its weakest link. \u2014 Donald Knuth</td></tr>
<tr><td>0</td><td>Additional fortune added at request time.</td></tr>
<tr><td>3</td><td>After enough decimal places, nobody gives a damn.</td></tr>
<tr><td>7</td><td>Any program that runs right is obsolete.</td></tr>
<tr><td>10</td><td>Computers make very fast, very accurate mistakes.</td></tr>
<tr><td>6</td><td>Emacs is a nice operating system, but I prefer UNIX. \u2014 Tom Christaensen</td></tr>
<tr><td>9</td><td>Feature: A bug with seniority.</td></tr>
<tr><td>1</td><td>fortune: No such file or directory</td></tr>
<tr><td>12</td><td>\u30d5\u30ec\u30fc\u30e0\u30ef\u30fc\u30af\u306e\u30d9\u30f3\u30c1\u30de\u30fc\u30af</td></tr>
</table></body></html>"""

# The page as TechEmpower's example response writes it, with no whitespace:
# what a server that escapes with named entities produces byte for byte.
EXAMPLE_FORTUNE = (
    "<!DOCTYPE html><html><head><title>Fortunes</title></head><body><table>"
    + "".join(
        line
        for line in VALID_FORTUNE.split("\n")[3:-1]
    )
    + "</table></body></html>"
)


class FortuneHTMLParser(HTMLParser):
    """TechEmpower's FortuneHTMLParser, same normalisation rules.

    It accepts any correct escaping (`&#34;` for `&quot;`, `&#39;` for
    `&apos;`, an unescaped `'` or `"` in text) and rejects anything that
    changes what a browser would show, which is the definition of
    "identical to the expected page" their verifier uses.
    """

    IGNORED_TAGS = ("<meta>", "</meta>", "<link>", "</link>", "<script>", "</script>",
                    "<thead>", "</thead>", "<tbody>", "</tbody>")

    def __init__(self):
        HTMLParser.__init__(self, convert_charrefs=False)
        self.ignore_content = False
        self.body = []

    def handle_decl(self, decl):
        self.append("<!{d}>".format(d=decl.lower()))

    def handle_charref(self, name):
        val = name.lower()
        table = {
            ("34", "034", "x22"): "&quot;",
            ("39", "039", "x27"): "&apos;",
            ("43", "043", "x2b"): "+",
            ("62", "062", "x3e"): "&gt;",
            ("60", "060", "x3c"): "&lt;",
            ("47", "047", "x2f"): "/",
            ("40", "040", "x28"): "(",
            ("41", "041", "x29"): ")",
        }
        for keys, out in table.items():
            if val in keys:
                self.append(out)

    def handle_entityref(self, name):
        self.append("\u2014" if name == "mdash" else "&{n};".format(n=name))

    def handle_starttag(self, tag, attrs):
        self.append("<{t}>".format(t=tag))
        if tag.lower() in ("table", "html"):
            self.append("\n")

    def handle_data(self, data):
        if data.strip() != "":
            data = data.replace("'", "&apos;").replace('"', "&quot;").replace(">", "&gt;")
            self.append(data)

    def handle_endtag(self, tag):
        self.append("</{t}>".format(t=tag))
        if tag.lower() in ("tr", "head"):
            self.append("\n")

    def append(self, item):
        self.ignore_content = item == "<script>" or (self.ignore_content and item != "</script>")
        if not (self.ignore_content or item in self.IGNORED_TAGS):
            self.body.append(item)


# ---------------------------------------------------------------------------
# Plumbing
# ---------------------------------------------------------------------------

FAILURES = []
NOTES = []


def fail(url, what):
    FAILURES.append((url, what))
    print("  FAIL %s: %s" % (url, what))


def note(url, what):
    NOTES.append((url, what))
    print("  note %s: %s" % (url, what))


def fetch(port, path):
    """One GET, as TechEmpower's verifier sends it. Returns (status, headers, body)."""
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=30)
    try:
        c.request("GET", path, headers={
            "Accept": "application/json,text/html;q=0.9,application/xhtml+xml;q=0.9,application/xml;q=0.8,*/*;q=0.7",
        })
        r = c.getresponse()
        body = r.read()
        # Header names are case-insensitive; keep the first of each.
        headers = {}
        for k, v in r.getheaders():
            headers.setdefault(k.lower(), v)
        return r.status, headers, body
    finally:
        c.close()


CONTENT_TYPES = {
    "json": r"^application/json(; ?charset=(UTF|utf)-8)?$",
    "html": r"^text/html; ?charset=(UTF|utf)-8$",
}


def check_headers(port, path, status, headers, should_be):
    """verifications.verify_status + verify_headers."""
    if status != 200:
        fail(path, "status %s, expected 200" % status)
    for name in ("server", "date", "content-type"):
        if name not in headers:
            fail(path, "required header missing: %s" % name)
    if "content-length" not in headers and "transfer-encoding" not in headers:
        fail(path, "neither Content-Length nor Transfer-Encoding")
    if "content-encoding" in headers:
        fail(path, "compressed response (%s); gzip is not permitted" % headers["content-encoding"])
    date = headers.get("date")
    if date is not None:
        try:
            parsed = datetime.strptime(date, "%a, %d %b %Y %H:%M:%S %Z").replace(tzinfo=timezone.utc)
            # Stricter than TechEmpower (theirs warns on the format only):
            # "the rendered date must be accurate".
            skew = abs((datetime.now(timezone.utc) - parsed).total_seconds())
            if skew > 2.5:
                fail(path, "Date %r is %.1f s from now" % (date, skew))
        except ValueError:
            fail(path, "Date %r is not an IMF-fixdate (stricter: theirs warns)" % date)
    # "Make sure that the date object isn't cached": a request three seconds
    # later must carry a different Date.
    time.sleep(3)
    _, second, _ = fetch(port, path)
    if date is not None and second.get("date") == date:
        fail(path, "Date %r did not change over 3 s: cached" % date)
    content_type = headers.get("content-type")
    if content_type is not None and not re.match(CONTENT_TYPES[should_be], content_type):
        fail(path, "Content-Type %r does not match %s" % (content_type, CONTENT_TYPES[should_be]))


def is_int(value):
    # bool is an int in Python; JSON true is not a number.
    return isinstance(value, int) and not isinstance(value, bool)


def check_world(path, obj):
    """verify_randomnumber_object, strict on key spelling, type and range."""
    if not isinstance(obj, dict):
        fail(path, "expected a JSON object, got %r" % (str(obj)[:40],))
        return
    if set(obj.keys()) != {"id", "randomNumber"}:
        # Stricter: theirs lowercases the keys and warns on extras. General
        # requirement 4 says case matters, and an extra key is extra bytes.
        fail(path, "keys %s, expected exactly id and randomNumber" % sorted(obj.keys()))
        return
    for key in ("id", "randomNumber"):
        if not is_int(obj[key]):
            fail(path, "%s is %r, not a JSON integer (stricter: theirs accepts a numeric string)" % (key, obj[key]))
            return
    if not 1 <= obj["id"] <= 10000:
        fail(path, "id %d outside 1..10000 (stricter: theirs warns)" % obj["id"])
    if not 1 <= obj["randomNumber"] <= 10000:
        fail(path, "randomNumber %d outside 1..10000 (stricter: theirs warns above)" % obj["randomNumber"])


# ---------------------------------------------------------------------------
# The database side: pg_stat_statements, as TechEmpower counts it
# ---------------------------------------------------------------------------

def psql(sql):
    out = subprocess.run(
        [os.environ["PSQL"], "-h", "127.0.0.1", "-p", os.environ.get("PGPORT", "5432"),
         "-U", "benchmarkdbuser", "-d", "hello_world", "-At", "-c", sql],
        capture_output=True, text=True, env=dict(os.environ, PGPASSWORD="benchmarkdbpass"), check=True)
    return out.stdout.strip()


def counted(table):
    """(queries, rows) that mention `table`, TechEmpower's regex."""
    calls = psql("SELECT coalesce(SUM(calls),0) FROM pg_stat_statements WHERE query ~* '[[:<:]]%s[[:>:]]'" % table)
    rows = psql("SELECT coalesce(SUM(rows),0) FROM pg_stat_statements WHERE query ~* '[[:<:]]%s[[:>:]]' AND query ~* 'select'" % table)
    return int(calls), int(rows)


def hammer(port, path, requests, concurrency):
    """`requests` GETs over `concurrency` keep-alive connections. Returns failures."""
    failures = [0]
    lock = threading.Lock()
    per = [requests // concurrency + (1 if i < requests % concurrency else 0) for i in range(concurrency)]

    def worker(n):
        c = http.client.HTTPConnection("127.0.0.1", port, timeout=60)
        for _ in range(n):
            try:
                c.request("GET", path)
                r = c.getresponse()
                r.read()
                if r.status != 200:
                    raise RuntimeError(r.status)
            except Exception:
                with lock:
                    failures[0] += 1
                c.close()
                c = http.client.HTTPConnection("127.0.0.1", port, timeout=60)
        c.close()

    threads = [threading.Thread(target=worker, args=(n,)) for n in per]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    return failures[0]


def check_query_count(port, path, table, requests, concurrency, per_request, rows_per_request):
    """verify_queries_count: every request reached the database, no margin.

    A cache anywhere shows up here as too few queries; a batched IN (...) as
    too few queries with the right number of rows.
    """
    psql("SELECT pg_stat_statements_reset()")
    failed = hammer(port, path, requests, concurrency)
    queries, rows = counted(table)
    want_q = requests * per_request
    want_r = requests * rows_per_request
    if failed:
        fail(path, "%d of %d requests failed under %d-way load" % (failed, requests, concurrency))
    if queries < want_q:
        fail(path, "only %d queries on %s for %d requests (expected %d): something cached or batched" % (queries, table, requests, want_q))
    elif queries > want_q * 1.05:
        fail(path, "%d queries on %s for %d requests (expected %d): excessively high" % (queries, table, requests, want_q))
    if rows < want_r:
        fail(path, "only %d rows read from %s (expected %d)" % (rows, table, want_r))
    print("  %s: %d requests at %d-way, %d queries, %d rows (expected %d / %d), %d failed"
          % (path, requests, concurrency, queries, rows, want_q, want_r, failed))


# ---------------------------------------------------------------------------
# The four tests
# ---------------------------------------------------------------------------

def verify_json(port):
    path = "/json"
    status, headers, body = fetch(port, path)
    try:
        obj = json.loads(body)
    except ValueError as e:
        fail(path, "invalid JSON: %s" % e)
        return
    if obj != {"message": "Hello, World!"}:
        fail(path, "body %r, expected {\"message\":\"Hello, World!\"} (stricter: theirs is case-insensitive)" % obj)
    if len(body) > 32:
        fail(path, "%d bytes; expected about 28" % len(body))
    check_headers(port, path, status, headers, "json")


def verify_db(port, requests, concurrency):
    path = "/db"
    status, headers, body = fetch(port, path)
    try:
        obj = json.loads(body)
    except ValueError as e:
        fail(path, "invalid JSON: %s" % e)
        return
    if isinstance(obj, list):
        fail(path, "a JSON array; expected an object (stricter: theirs warns)")
        return
    check_world(path, obj)
    check_headers(port, path, status, headers, "json")
    check_query_count(port, path, "world", requests, concurrency, 1, 1)


def verify_queries(port, requests, concurrency):
    # TechEmpower's cases: 2, 0, foo, 501 and empty.
    for q, expected in (("2", 2), ("0", 1), ("foo", 1), ("501", 500), ("", 1), ("20", 20)):
        path = "/queries?queries=" + q
        status, headers, body = fetch(port, path)
        try:
            arr = json.loads(body)
        except ValueError as e:
            fail(path, "invalid JSON: %s" % e)
            continue
        if not isinstance(arr, list):
            fail(path, "top-level JSON is not an array")
            continue
        if len(arr) != expected:
            fail(path, "%d rows, expected %d" % (len(arr), expected))
        for obj in arr:
            check_world(path, obj)
        check_headers(port, path, status, headers, "json")
    # And with no parameter at all.
    status, headers, body = fetch(port, "/queries")
    try:
        if len(json.loads(body)) != 1:
            fail("/queries", "no parameter should be one row")
    except (ValueError, TypeError) as e:
        fail("/queries", "invalid JSON: %s" % e)
    check_query_count(port, "/queries?queries=20", "world", requests, concurrency, 20, 20)


def verify_fortunes(port, requests, concurrency):
    path = "/fortunes"
    status, headers, body = fetch(port, path)
    text = body.decode("utf-8")
    parser = FortuneHTMLParser()
    parser.feed(text)
    got = "".join(parser.body)
    if got != VALID_FORTUNE:
        import difflib
        diff = "\n".join(difflib.unified_diff(VALID_FORTUNE.split("\n"), got.split("\n"), "valid", "response", n=0, lineterm=""))
        fail(path, "page differs from TechEmpower's expected page:\n" + diff)
    elif body != EXAMPLE_FORTUNE.encode("utf-8"):
        # Equivalent by TechEmpower's rules but not the same bytes: another
        # correct escaping (`&#34;` for `&quot;`). Go's html/template and
        # Bun.escapeHTML choose their own entities, and the spec names those
        # libraries, so this is reported rather than failed.
        note(path, "equivalent to the expected page under TechEmpower's parser, but not byte-identical: "
             "%d bytes against the example's %d (a different correct escaping)"
             % (len(body), len(EXAMPLE_FORTUNE.encode("utf-8"))))
    else:
        print("  /fortunes: byte-identical to TechEmpower's example page")
    check_headers(port, path, status, headers, "html")
    check_query_count(port, path, "fortune", requests, concurrency, 1, 12)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--label", default="server")
    ap.add_argument("--requests", type=int, default=512)
    ap.add_argument("--concurrency", type=int, default=64)
    a = ap.parse_args()
    print("verifying %s on port %d" % (a.label, a.port))
    verify_json(a.port)
    verify_db(a.port, a.requests, a.concurrency)
    verify_queries(a.port, a.requests // 4, a.concurrency)
    verify_fortunes(a.port, a.requests, a.concurrency)
    if FAILURES:
        print("%s: FAILED %d check(s)" % (a.label, len(FAILURES)))
        sys.exit(1)
    print("%s: PASS (%d note(s))" % (a.label, len(NOTES)))


if __name__ == "__main__":
    main()
