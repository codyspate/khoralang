"""Every server in `bench/`, back to back in one sitting.

    python bench/compare.py

`load.py` measures one server that is already running, at one connection
count. This starts each server in turn and walks a *ladder* of connection
counts, because the single most important thing about a throughput number is
whether it is a fact about the server or about the client — and one number
cannot tell you.

**Nothing here reports a rate that is still climbing as a measurement.** A
server at its ceiling answers about the same amount however many connections
are offered; a client at *its* ceiling reports more the more connections it is
given. Pointed at Go this prints 140k, 143k and 159k for 48, 96 and 160
connections — flat, so the figure is the server's. Pointed at the Rust control
it prints 630k, 1.26M and 2.13M, which is not a slower version of the same
thing: it is not a measurement of the server at all.

That distinction is why `bench/README.md`'s older figures are now marked. They
were taken at 48 connections, which is below this client's ceiling for
anything above a few hundred thousand requests a second, so every fast number
this repository has published was a measurement of the harness.

**A warm-up run is discarded**, which the older method did not do. Khora, Rust
and Go are compiled ahead of time and do not need one; Node, .NET and the JVM
do, and measuring them cold would flatter the others by an artefact. Every
server gets the same treatment.

Everything else follows the README: held-open connections, five-second runs,
the same request, the same body, one machine, nothing else running.
"""
import json
import os
import shutil
import socket
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
PEERS = os.path.join(HERE, "peers")

# Three connection counts rather than one, because the number that matters is
# not the throughput but whether the throughput *moves* when the client is
# given more capacity. See `run`.
LADDER = [48, 96, 160]
SECONDS = 5


def executable(*parts):
    path = os.path.join(*parts)
    return path if os.path.exists(path) else path + ".exe"


# (label, port, how to start it, what it is)
SERVERS = [
    (
        "Khora, floor",
        18950,
        [executable(ROOT, "bench", "floor", "src", "release")],
        "accept, read, write a fixed string. No parsing.",
    ),
    (
        "Khora, render",
        18951,
        [executable(ROOT, "bench", "render", "src", "release")],
        "the floor plus response rendering. No parsing.",
    ),
    (
        "Khora, std::net::http",
        18952,
        [executable(ROOT, "bench", "service", "src", "release")],
        "a Router with one route: accept, read, parse, route, render.",
    ),
    (
        "Khora, the same, debug",
        # The same port: it is the same program, and the runner only ever has
        # one server up at a time.
        18952,
        [executable(ROOT, "bench", "service", "src", "main")],
        "the same program under the default profile, for what --release buys.",
    ),
    (
        "Rust, thread per connection",
        18953,
        [executable(ROOT, "bench", "control_keepalive"), "18953"],
        "hand-rolled, no framework. The floor for a compiled language.",
    ),
    (
        "Go, net/http",
        18954,
        [executable(PEERS, "go_health"), "18954"],
        "the standard library's server.",
    ),
    (
        "Node, node:http",
        18955,
        ["node", os.path.join(PEERS, "node_health.mjs"), "18955"],
        "the standard library's server.",
    ),
    (
        "C#, ASP.NET Core",
        18956,
        [
            "dotnet",
            os.path.join(PEERS, "dotnet_health", "bin", "Release", "net8.0", "dotnet_health.dll"),
            "18956",
        ],
        "a minimal API on Kestrel.",
    ),
    (
        "Java, JDK HttpServer",
        18957,
        ["java", "-cp", PEERS, "JavaHealth", "18957"],
        "com.sun.net.httpserver, which is not what a Java service ships on.",
    ),
]


def listening(port, seconds=30):
    deadline = time.time() + seconds
    while time.time() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.5):
                return True
        except OSError:
            time.sleep(0.05)
    return False


def measure(port, label, workers):
    """One `load.py` run, as requests a second."""
    out = subprocess.run(
        [sys.executable, os.path.join(HERE, "load.py"), str(port), label,
         str(workers), str(SECONDS)],
        capture_output=True, text=True,
    )
    for word in out.stdout.split():
        try:
            return float(word)
        except ValueError:
            continue
    return 0.0


def run(label, port, command, note):
    """Walks the ladder, and says whether the answer is about the server.

    **A number is only the server's if it stops moving.** Pointed at Go this
    reports 138k, 139k and 151k for 48, 96 and 160 connections: flat, so the
    server is the limit and the figure means something. Pointed at the Rust
    control it reports 664k, 1.27M and 2.13M — still climbing, which is the
    shape of a *client* running out of capacity, not a server reaching one.

    The second is not a slower version of the first. It is not a measurement of
    the server at all, and reporting it as one is how a benchmark comes to
    flatter whoever wrote it.
    """
    if not (os.path.exists(command[0]) or shutil.which(command[0])):
        print(f"  {label:30s} not built, skipped")
        return None
    server = subprocess.Popen(
        command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, cwd=ROOT
    )
    rates = []
    try:
        if not listening(port):
            print(f"  {label:30s} never came up")
            return None
        measure(port, label, LADDER[0])  # warm-up, discarded
        rates = [measure(port, label, w) for w in LADDER]
    finally:
        server.terminate()
        try:
            server.wait(timeout=10)
        except subprocess.TimeoutExpired:
            server.kill()
    # The port has to be free before the next server asks for it.
    time.sleep(1.0)

    best = max(rates)
    # Settled when the top of the ladder is no more than a sixth above the
    # middle of it. A server that is still climbing by more than that has not
    # been found yet.
    settled = rates[-1] <= rates[len(rates) // 2] * 1.15
    ladder = ", ".join(f"{w}:{r:,.0f}" for w, r in zip(LADDER, rates))
    verdict = f"{best:9,.0f} req/s" if settled else f">{best:8,.0f} req/s  client-bound"
    print(f"  {label:30s} {verdict}   ({ladder})")
    return {
        "label": label,
        "best": best,
        "measured": settled,
        "ladder": dict(zip(LADDER, rates)),
        "note": note,
    }


if __name__ == "__main__":
    print(f"connections {LADDER}, {SECONDS}s each, after a discarded warm-up")
    print("a rate that keeps climbing with connections is the client's, not the server's\n")
    results = [r for r in (run(*s) for s in SERVERS) if r]
    with open(os.path.join(HERE, "compare.json"), "w", encoding="utf-8") as f:
        json.dump(results, f, indent=2)
    print(f"\nwritten to {os.path.join(HERE, 'compare.json')}")
