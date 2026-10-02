"""Times the four TechEmpower tests on every server, interleaved.

    sh bench/peers/techempower/run.sh        # verifies first, then this

For each round, for each server: start it on CPUs 1,3,5, and for each test
warm up and then measure 10 s with wrk on CPUs 9,10 at 64 connections. Five
rounds. Postgres is on CPU 7 by itself.

Reported per test and server: requests a second (median and range over the
rounds), p50 and p99 latency (median over rounds), the server's peak resident
memory, the server's CPU time per request, and Postgres's CPU use.

**Postgres's CPU is measured because it can be the whole answer.** It has one
CPU. When a database test holds that CPU near 100%, every server is waiting on
the same database and the ranking measures Postgres, not the language; the
table says so on that row instead of letting the number stand for the server.

Standard library only. Writes every run to results.jsonl beside the table.
"""
import argparse
import json
import os
import signal
import socket
import statistics
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
PG = os.environ.get("PG", "/general/khora-tmp/pg")
WRK = os.environ.get("WRK", "/general/khora-tmp/wrk/wrk")
BUN = os.environ.get("BUN", os.path.expanduser("~/.bun/bin/bun"))
PORT = int(os.environ.get("PORT", "8080"))

SERVER_CPUS = os.environ.get("SERVER_CPUS", "1,3,5")
LOAD_CPUS = os.environ.get("LOAD_CPUS", "9,10")
PG_CPU = os.environ.get("PG_CPUS", "7")
TICK = os.sysconf("SC_CLK_TCK")

SERVERS = {
    "khora-threads": ({"KHORA_FIBERS": "threads"}, [os.path.join(HERE, "khora/build/techempower")]),
    "khora-scheduler": ({"KHORA_FIBERS": "scheduler"}, [os.path.join(HERE, "khora/build/techempower")]),
    "go": ({}, [os.path.join(HERE, "go/techempower")]),
    "node": ({}, ["node", os.path.join(HERE, "node/server.mjs")]),
    "bun": ({}, [BUN, os.path.join(HERE, "bun/server.js")]),
}

TESTS = [
    ("json", "/json"),
    ("db", "/db"),
    ("queries20", "/queries?queries=20"),
    ("pipelined20", "/pipelined-queries?queries=20"),
    ("fortunes", "/fortunes"),
]

# The servers whose stock driver pipelines, and so have /pipelined-queries.
# The row compares Khora with Go and Bun pipelining the same way; the
# headline queries20 row stays sequential for every server. README.md says
# why Node is not in it.
PIPELINING = {"khora-threads", "khora-scheduler", "go", "bun"}

# Postgres at or above this share of its one CPU during a run is saturated.
SATURATED = 0.90


# ---------------------------------------------------------------------------
# /proc
# ---------------------------------------------------------------------------

def cpu_ticks(pid):
    """utime + stime of a whole process (every thread), in clock ticks."""
    with open("/proc/%d/stat" % pid) as f:
        # The command name can hold spaces and parentheses; fields start after
        # the last ')'.
        fields = f.read().rsplit(")", 1)[1].split()
    return int(fields[11]) + int(fields[12])


def postgres_pids():
    with open(os.path.join(PG, "data", "postmaster.pid")) as f:
        master = int(f.readline())
    pids = [master]
    for entry in os.listdir("/proc"):
        if entry.isdigit():
            try:
                with open("/proc/%s/stat" % entry) as f:
                    if int(f.read().rsplit(")", 1)[1].split()[1]) == master:
                        pids.append(int(entry))
            except (OSError, IndexError, ValueError):
                pass
    return pids


def postgres_ticks():
    """CPU of the postmaster and every backend. A backend that exits during a
    window takes its ticks with it, so this can only under-count; the pools
    here hold their connections open, so in practice none do."""
    total = 0
    for pid in postgres_pids():
        try:
            total += cpu_ticks(pid)
        except OSError:
            pass
    return total


def resident_kb(pid):
    with open("/proc/%d/status" % pid) as f:
        for line in f:
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    return 0


class PeakRss:
    """Samples VmRSS every 50 ms. VmHWM would include the warm-up and earlier
    tests on the same process, which is not this test's peak."""

    def __init__(self, pid):
        self.pid, self.peak, self.stop = pid, 0, threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        while not self.stop.is_set():
            try:
                self.peak = max(self.peak, resident_kb(self.pid))
            except OSError:
                return
            self.stop.wait(0.05)

    def finish(self):
        self.stop.set()
        self.thread.join()
        return self.peak


# ---------------------------------------------------------------------------
# Servers and load
# ---------------------------------------------------------------------------

def start(name):
    extra, argv = SERVERS[name]
    env = dict(os.environ, PORT=str(PORT), **extra)
    proc = subprocess.Popen(["taskset", "-c", SERVER_CPUS] + argv, env=env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    end = time.time() + 20
    while time.time() < end:
        try:
            socket.create_connection(("127.0.0.1", PORT), 0.5).close()
            return proc
        except OSError:
            if proc.poll() is not None:
                raise SystemExit("%s exited during start-up" % name)
            time.sleep(0.1)
    proc.kill()
    raise SystemExit("%s did not listen on %d" % (name, PORT))


def stop(proc):
    proc.send_signal(signal.SIGTERM)
    try:
        proc.wait(5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()


def wrk(path, seconds, connections):
    out = subprocess.run(
        ["taskset", "-c", LOAD_CPUS, WRK, "-t2", "-c%d" % connections, "-d%ds" % seconds,
         "--timeout", "10s", "-s", os.path.join(HERE, "report.lua"),
         "http://127.0.0.1:%d%s" % (PORT, path)],
        capture_output=True, text=True, check=True)
    for line in out.stdout.splitlines():
        if line.startswith("{"):
            return json.loads(line)
    raise SystemExit("wrk printed no summary:\n" + out.stdout + out.stderr)


def measure(name, proc, test, path, seconds, warmup, connections):
    wrk(path, warmup, connections)
    pid = proc.pid  # taskset execs the server, so this is the server's pid
    rss = PeakRss(pid)
    server0, pg0, t0 = cpu_ticks(pid), postgres_ticks(), time.time()
    result = wrk(path, seconds, connections)
    server1, pg1, t1 = cpu_ticks(pid), postgres_ticks(), time.time()
    peak = rss.finish()
    wall = t1 - t0
    requests = result["requests"]
    errors = sum(result[k] for k in ("connect_errors", "read_errors", "write_errors", "timeouts", "non_2xx"))
    return {
        "server": name, "test": test, "path": path,
        "rps": requests / (result["duration_us"] / 1e6),
        "p50_us": result["p50_us"], "p99_us": result["p99_us"], "max_us": result["max_us"],
        "errors": errors,
        "peak_rss_kb": peak,
        "server_cpu_us_per_req": (server1 - server0) / TICK * 1e6 / max(requests, 1),
        "server_cores": (server1 - server0) / TICK / wall,
        "pg_cores": (pg1 - pg0) / TICK / wall,
        "pg_cpu_us_per_req": (pg1 - pg0) / TICK * 1e6 / max(requests, 1),
    }


# ---------------------------------------------------------------------------
# Reporting
# ---------------------------------------------------------------------------

def table(runs, servers):
    lines = []
    for test, _ in TESTS:
        lines.append("")
        lines.append("## %s" % test)
        lines.append("%-16s %9s %19s %8s %8s %9s %10s %8s %8s  %s" % (
            "server", "req/s", "range", "p50 ms", "p99 ms", "peak MB", "cpu us/rq", "srv cpu", "pg cpu", "flags"))
        for name in servers:
            mine = [r for r in runs if r["server"] == name and r["test"] == test]
            if not mine:
                continue
            rps = [r["rps"] for r in mine]
            flags = []
            pg = statistics.median(r["pg_cores"] for r in mine)
            pg_cpus = len(PG_CPU.split(","))
            if test != "json" and pg >= SATURATED * pg_cpus:
                flags.append("POSTGRES-BOUND (%.0f%% of its %d CPU(s)): this row measures the database" % (pg / pg_cpus * 100, pg_cpus))
            if any(r["errors"] for r in mine):
                flags.append("ERRORS %d" % sum(r["errors"] for r in mine))
            lines.append("%-16s %9.0f %9.0f-%-9.0f %8.2f %8.2f %9.1f %10.1f %8.2f %8.2f  %s" % (
                name, statistics.median(rps), min(rps), max(rps),
                statistics.median(r["p50_us"] for r in mine) / 1000,
                statistics.median(r["p99_us"] for r in mine) / 1000,
                max(r["peak_rss_kb"] for r in mine) / 1024,
                statistics.median(r["server_cpu_us_per_req"] for r in mine),
                statistics.median(r["server_cores"] for r in mine),
                pg, "; ".join(flags)))
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rounds", type=int, default=5)
    ap.add_argument("--seconds", type=int, default=10)
    ap.add_argument("--warmup", type=int, default=5)
    ap.add_argument("--connections", type=int, default=64)
    ap.add_argument("--servers", default=",".join(SERVERS))
    ap.add_argument("--out", default=os.path.join(HERE, "results"))
    a = ap.parse_args()
    servers = a.servers.split(",")
    os.makedirs(a.out, exist_ok=True)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    jsonl = os.path.join(a.out, "results-%s.jsonl" % stamp)

    header = [
        "# TechEmpower read tests, %s" % time.strftime("%Y-%m-%d %H:%M:%S %Z"),
        "machine: %s, %d CPUs usable; load average at start %s" % (
            os.uname().machine, len(os.sched_getaffinity(0)), " ".join("%.2f" % x for x in os.getloadavg())),
        "server on CPUs %s, Postgres on %s, wrk -t2 on %s; %d connections; %d rounds x %d s after %d s warm-up; interleaved"
        % (SERVER_CPUS, PG_CPU, LOAD_CPUS, a.connections, a.rounds, a.seconds, a.warmup),
        "load generator: wrk (bench/loadgen can only send GET /health)",
    ]
    print("\n".join(header), flush=True)
    runs = []
    with open(jsonl, "w") as out:
        for round_ in range(1, a.rounds + 1):
            for name in servers:
                proc = start(name)
                try:
                    for test, path in TESTS:
                        if test == "pipelined20" and name not in PIPELINING:
                            continue
                        r = measure(name, proc, test, path, a.seconds, a.warmup, a.connections)
                        r["round"] = round_
                        runs.append(r)
                        out.write(json.dumps(r) + "\n")
                        out.flush()
                        print("round %d %-16s %-10s %9.0f req/s  p99 %.2f ms  srv %.2f cores  pg %.2f cores  err %d"
                              % (round_, name, test, r["rps"], r["p99_us"] / 1000, r["server_cores"], r["pg_cores"], r["errors"]),
                              flush=True)
                finally:
                    stop(proc)
    header.append("load average at end %s" % " ".join("%.2f" % x for x in os.getloadavg()))
    report = "\n".join(header) + "\n" + table(runs, servers) + "\n"
    with open(os.path.join(a.out, "summary-%s.txt" % stamp), "w") as f:
        f.write(report)
    print(report)
    print("raw runs: %s" % jsonl)


if __name__ == "__main__":
    main()
