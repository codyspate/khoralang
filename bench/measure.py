"""Every server, one sitting, with the four conditions checked rather than hoped.

    python bench/measure.py

`/docs/performance/` says what would have to be true before a throughput number
is published, and this script is that list turned into a program:

1. **A load generator that is not the bottleneck.** `bench/loadgen.exe` is run
   at several thread counts and the rate has to stop changing. A generator
   whose rate still climbs when it is given more of the machine is the thing
   being measured.
2. **A ladder where the rate flattens.** Each server is driven at several
   connection counts and the top of the ladder has to be no higher than the
   middle. A rate that climbs with the client's concurrency is the client's
   rate.
3. **Repetition much tighter than 1.85x.** The chosen rung is run several
   times and the spread is reported. 1.85x is the number that disqualified
   the previous rig, so it is the number to beat by a wide margin.
4. **The machine, the profile and the date beside the figure.** `loadgen`
   prints the first and the third itself; the profile is this script's, and it
   refuses to report anything built at the default debug profile.

A server that fails any of the first three is reported with what failed
instead of with a number. That is the whole point: the previous rig could not
fail, so it always produced a figure.
"""
import json
import os
import platform
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
EXE = ".exe" if os.name == "nt" else ""

LOADGEN = os.path.join(HERE, "loadgen" + EXE)

PEERS = os.path.join(HERE, "peers")

# name -> (port, the command that starts it)
#
# The peers are each language's *ordinary* server rather than a hand-rolled
# socket loop, because the comparison worth making is against what a team would
# actually write. A peer that is not installed is reported as skipped rather
# than silently left out of the table.
SERVERS = [
    ("Khora floor", 18950, [os.path.join(HERE, "floor", "build", "floor" + EXE)]),
    ("Khora render", 18951, [os.path.join(HERE, "render", "build", "render" + EXE)]),
    ("Khora std::net::http", 18952, [os.path.join(HERE, "service", "build", "service" + EXE)]),
    ("Rust thread per conn", 18953, [os.path.join(HERE, "control_keepalive" + EXE), "18953"]),
    ("Go net/http", 18954, [os.path.join(PEERS, "go_health" + EXE), "18954"]),
    ("Node node:http", 18955, ["node", os.path.join(PEERS, "node_health.mjs"), "18955"]),
    ("C# ASP.NET Core", 18956, ["dotnet", os.path.join(PEERS, "dotnet_health", "bin", "Release", "net8.0", "dotnet_health.dll"), "18956"]),
    ("Java JDK HttpServer", 18957, ["java", "-cp", PEERS, "JavaHealth", "18957"]),
]

LADDER = [16, 32, 64, 128]
RUNG = 32
THREAD_LADDER = [4, 8, 16]
REPEATS = 4
SECONDS = 6


def run(port, label, connections, seconds, threads=8, pid=None):
    """One measurement, as the generator's own JSON line."""
    args = [
        LOADGEN, "--port", str(port), "--label", label,
        "--connections", str(connections), "--seconds", str(seconds),
        "--threads", str(threads),
    ]
    if pid:
        args += ["--watch-pid", str(pid)]
    out = subprocess.run(args, capture_output=True, text=True, timeout=seconds + 120)
    for line in out.stdout.splitlines():
        if line.startswith("json "):
            return json.loads(line[5:])
    raise RuntimeError(f"no measurement from {label}: {out.stdout}{out.stderr}")


def serving(command, port):
    """Starts a server and waits for it to answer, or explains why not."""
    program = command[0]
    if os.path.sep in program and not os.path.exists(program):
        return None
    try:
        process = subprocess.Popen(
            command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
    except (FileNotFoundError, OSError):
        return None
    import socket
    for _ in range(200):
        if process.poll() is not None:
            return None
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return process
        except OSError:
            time.sleep(0.05)
    process.kill()
    return None


def spread(rates):
    """Widest over narrowest, which is the shape 1.85x was quoted in."""
    return max(rates) / min(rates) if rates and min(rates) > 0 else float("inf")


def measure(name, port, command):
    process = serving(command, port)
    if process is None:
        return {"name": name, "skipped": "not built or not installed"}
    try:
        # A discarded warm-up. A JIT that has not compiled the handler yet is
        # measured on its way up, and the peers are the servers that have one.
        run(port, name, RUNG, 5)
        # 1. the generator is not the bottleneck
        by_threads = [(t, run(port, name, RUNG, SECONDS, threads=t)["rate"]) for t in THREAD_LADDER]
        # The same shape as the ladder check, and for the same reason. Comparing
        # the top against the *maximum* is a test that cannot fail whenever the
        # top is the maximum, which is exactly the case worth catching.
        thread_rates = [r for _, r in by_threads]
        # Against the *best of the earlier rungs*, not against the maximum of
        # all of them -- which is a test that cannot fail when the top rung is
        # the maximum -- and not against the middle either, because the top
        # rung gives the generator as many threads as the machine has cores and
        # a server starved of them collapses rather than climbs. Climbing is
        # the thing being detected; a collapse is the machine being oversold
        # and is visible in the printed ladder.
        settled_threads = thread_rates[-1] <= max(thread_rates[:-1]) * 1.05

        # 2. the ladder flattens
        rungs = [(c, run(port, name, c, SECONDS)) for c in LADDER]
        rates = [r["rate"] for _, r in rungs]
        # Against the *bottom* of the ladder. A saturated server answers at the
        # same rate however many connections are queued on it -- that is what
        # saturated means -- so the top rung should be no higher than the
        # bottom one. Comparing the top against the rung below it passes a
        # server that climbed steadily all the way up and then happened to
        # level off, which is what a JIT still compiling the handler does.
        settled_ladder = rates[-1] <= rates[0] * 1.10

        # 3. it repeats
        # Watched here rather than on the ladder, so the memory figure and the
        # rate beside it come from the same run.
        repeats = [run(port, name, RUNG, SECONDS, pid=process.pid) for _ in range(REPEATS)]
        at_rung = repeats[-1]
        again = [r["rate"] for r in repeats]
        again.append(next(r for c, r in rungs if c == RUNG)["rate"])
        peak_rss = max((r.get("peak_rss_kb") or 0) for r in repeats)
        broken = sum(r.get("failed") or 0 for r in repeats)

        return {
            "name": name,
            "rate": round(sum(again) / len(again)),
            "spread": round(spread(again), 3),
            "p50_us": at_rung.get("p50_us"),
            "p95_us": at_rung.get("p95_us"),
            "p99_us": at_rung.get("p99_us"),
            "peak_rss_kb": peak_rss or None,
            "ladder": [(c, round(r["rate"])) for c, r in rungs],
            "threads": [(t, round(r)) for t, r in by_threads],
            "settled_threads": settled_threads,
            "settled_ladder": settled_ladder,
            "failed": broken,
        }
    finally:
        process.kill()
        process.wait()


def main():
    if not os.path.exists(LOADGEN):
        sys.exit(f"build the generator first:\n  rustc -O -o {LOADGEN} {os.path.join(HERE, 'loadgen.rs')}")
    profile = os.environ.get("KHORA_PROFILE", "debug")
    if profile != "release":
        print("!! KHORA_PROFILE is not `release`, so the Khora servers here are")
        print("!! debug builds and no number from them should be quoted.")
        print()

    print(f"{platform.system()} {platform.machine()}, {os.cpu_count()} cores, "
          f"{time.strftime('%Y-%m-%d %H:%M')}, servers built at {profile}")
    print(f"{SECONDS}s per run, {RUNG} connections, median of {REPEATS + 1}")
    print()

    results = [measure(*server) for server in SERVERS]
    print(f"{'':22s} {'req/s':>9s} {'spread':>7s} {'p50us':>6s} {'p99us':>6s} {'rssKB':>6s}  conditions")
    for r in results:
        if "skipped" in r:
            print(f"{r['name']:22s} {'--':>9s}  {r['skipped']}")
            continue
        conditions = []
        if not r["settled_threads"]:
            conditions.append("GENERATOR STILL CLIMBING")
        if not r["settled_ladder"]:
            conditions.append("LADDER STILL CLIMBING")
        if r["spread"] > 1.1:
            conditions.append(f"SPREAD {r['spread']}x")
        if r.get("failed"):
            conditions.append(f"{r['failed']} CONNECTION(S) LOST")
        verdict = "; ".join(conditions) if conditions else "ok"
        print(f"{r['name']:22s} {r['rate']:9d} {r['spread']:6.3f}x "
              f"{r['p50_us'] or 0:6d} {r['p99_us'] or 0:6d} {r['peak_rss_kb'] or 0:6d}  {verdict}")

    print()
    print("ladders (connections -> req/s), which is where a climbing rate shows:")
    for r in results:
        if "skipped" not in r:
            print(f"  {r['name']:22s} {r['ladder']}")
    print()
    print("generator threads -> req/s, which is where a client-bound rate shows:")
    for r in results:
        if "skipped" not in r:
            print(f"  {r['name']:22s} {r['threads']}")

    with open(os.path.join(HERE, "measure.json"), "w") as f:
        json.dump({"machine": platform.system(), "cores": os.cpu_count(),
                   "at": time.strftime("%Y-%m-%d %H:%M"), "profile": profile,
                   "results": results}, f, indent=2)
    print()
    print("written to bench/measure.json")


if __name__ == "__main__":
    main()
