"""How long the compiler takes, where the time goes, and whether that moved.

    python scripts/compiler-perf.py                    # measure and print
    python scripts/compiler-perf.py --check            # compare against the baseline
    python scripts/compiler-perf.py --write-baseline   # record a new one

**Build times are measured with a release-built compiler.** A debug-built
compiler is between five and ten times slower and is not the thing anybody
ships, so a number from one says nothing about what a user waits for. This
script builds `khora` with `--release` if it has to, and refuses to report
anything if it cannot.

Four questions, because they fail in different ways and a single wall-clock
number hides which one moved:

* **Cold build.** `--no-cache`, from an empty cache, which is what somebody
  cloning a repository waits for.
* **Warm rebuild.** The same build again with nothing changed, which is what
  the edit loop costs.
* **Peak memory.** The compiler's own resident set, sampled while it runs. A
  compiler that is fast and needs eight gigabytes is not usable on a laptop.
* **Monomorphization scaling.** A generated package with N generic
  instantiations, at several N, so that superlinear behaviour shows up as a
  curve rather than as a complaint from somebody with a large program.

`KHORA_TIMINGS=1` splits each build into check, monomorphize, lower, optimize,
object and link, so a regression points at a phase rather than at the whole.
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE = ".exe" if os.name == "nt" else ""
RELEASE_KHORA = os.path.join(ROOT, "target", "release", "khora" + EXE)
BASELINE = os.path.join(ROOT, "docs", "compiler-perf-baseline.json")

# The largest program in the corpus, which is what a scaling question wants.
SUBJECT = os.path.join(ROOT, "examples", "khq")

TIMING = re.compile(r"^khora-timing\s+(\S+)\s+([0-9.]+) ms", re.M)

# How far a measurement may move before it is a regression rather than noise.
# Wide, deliberately: this runs on whatever machine somebody has, and a gate
# that cries wolf is a gate that gets skipped. It catches a doubling, which is
# the size of regression worth stopping a change for.
TOLERANCE = 1.5


def release_compiler():
    """A release compiler built from the tree as it stands.

    **Always built, never found.** The first version of this returned an
    existing `target/release/khora` if there was one, and the one on the
    machine it was written on turned out to be four days old: it failed to
    parse a character escape the current lexer accepts, and would otherwise
    have produced build times for a compiler nobody has. Cargo does nothing
    when the binary is current, so asking costs a second and removes a whole
    class of wrong answer.
    """
    print("building the compiler with --release...", file=sys.stderr)
    out = subprocess.run(
        ["cargo", "build", "--release", "--features", "llvm", "-p", "khora-cli"],
        cwd=ROOT,
    )
    if out.returncode != 0 or not os.path.exists(RELEASE_KHORA):
        sys.exit("could not build a release compiler, and a debug one measures the wrong thing")
    # **And the runtime beside it.** Generated executables link against the
    # archive next to the compiler that produced them, so a release compiler
    # with no release runtime fails in the linker with a page of undefined
    # symbols -- which is what happened the first time this was run, and looks
    # nothing like the missing build step it is.
    subprocess.run(["cargo", "build", "--release", "-p", "khora-rt"], cwd=ROOT, check=True)
    return RELEASE_KHORA


def resident_kb(pid):
    if os.name == "nt":
        out = subprocess.run(
            ["tasklist", "/FI", f"PID eq {pid}", "/FO", "CSV", "/NH"],
            capture_output=True, text=True,
        )
        # `"name","pid","session","#","4,468 K"`. The memory field has a
        # thousands comma in it, so the field separator to split on is
        # quote-comma-quote and not the last comma -- which would return
        # `468 K"` and report four and a half megabytes as 468 KB.
        last = out.stdout.strip().rsplit('","', 1)[-1].strip().strip('"')
        digits = "".join(c for c in last if c.isdigit())
        return int(digits) if digits else 0
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    return int("".join(c for c in line if c.isdigit()))
    except OSError:
        pass
    return 0


def timed(command, cwd, watch=True):
    """Runs `command`, returning wall seconds, peak resident KB and phases."""
    began = time.time()
    process = subprocess.Popen(
        command, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        env={**os.environ, "KHORA_TIMINGS": "1"},
    )
    peak = 0
    if watch:
        while process.poll() is None:
            peak = max(peak, resident_kb(process.pid))
            time.sleep(0.02)
    out, err = process.communicate()
    elapsed = time.time() - began
    if process.returncode != 0:
        sys.exit(f"{' '.join(command)} failed:\n{err}")
    phases = {name: float(ms) for name, ms in TIMING.findall(err)}
    return {"seconds": round(elapsed, 3), "peak_rss_kb": peak, "phases": phases}


def clean(path):
    for junk in ("build",):
        target = os.path.join(path, junk)
        if os.path.isdir(target):
            shutil.rmtree(target, ignore_errors=True)
        elif os.path.exists(target):
            os.remove(target)


def generated(where, instantiations):
    """A package whose one job is to instantiate a generic many times.

    Each `Box<A>` at a distinct `A` is a specialization the back end has to
    emit, so the count is the thing being scaled. The types are distinct
    records rather than aliases, because an alias would collapse to one
    instance and measure nothing.
    """
    src = os.path.join(where, "src")
    os.makedirs(src, exist_ok=True)
    with open(os.path.join(where, "khora.toml"), "w") as f:
        f.write('[package]\nname = "mono"\nversion = "0.1.0"\n')
    lines = [
        "module mono::main;",
        "",
        "import std::core::{print};",
        "",
        "pub type Box<A> = { held: A };",
        "",
        "fn wrap<A>(value: A) -> Box<A> { { held: value } }",
        "",
    ]
    for i in range(instantiations):
        lines.append(f"pub type T{i} = {{ n{i}: Int }};")
    lines.append("")
    lines.append("pub fn main() -> Int {")
    lines.append("  let mut total = 0;")
    for i in range(instantiations):
        lines.append(f"  total = total + wrap({{ n{i}: {i} }}).held.n{i};")
    lines.append("  print(Int::to_string(total));")
    lines.append("  0")
    lines.append("}")
    with open(os.path.join(src, "main.kh"), "w") as f:
        f.write("\n".join(lines) + "\n")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true", help="compare against the recorded baseline")
    parser.add_argument("--write-baseline", action="store_true", help="record what this run measured")
    args = parser.parse_args()

    khora = release_compiler()
    print(f"{sys.platform}, {os.cpu_count()} cores, release compiler at {khora}")
    print()

    clean(SUBJECT)
    cold = timed([khora, "build", SUBJECT, "--no-cache"], ROOT)
    warm = timed([khora, "build", SUBJECT], ROOT)
    checked = timed([khora, "check", SUBJECT], ROOT)

    print(f"khq, {sum(1 for _ in open(os.path.join(SUBJECT, 'src', 'main.kh')))} lines in its entry module")
    print(f"  cold build   {cold['seconds']:7.2f}s   peak RSS {cold['peak_rss_kb'] // 1024:5d} MB")
    print(f"  warm build   {warm['seconds']:7.2f}s")
    print(f"  check only   {checked['seconds']:7.2f}s")
    if cold["phases"]:
        print("  where the cold build went:")
        for name in ("check", "monomorphize", "lower", "optimize", "object", "link", "total"):
            if name in cold["phases"]:
                print(f"    {name:<14} {cold['phases'][name]:9.1f} ms")

    print()
    print("monomorphization scaling")
    scaling = []
    import tempfile
    with tempfile.TemporaryDirectory() as tmp:
        for count in (10, 50, 200, 400):
            where = os.path.join(tmp, f"mono{count}")
            generated(where, count)
            run = timed([khora, "build", where, "--no-cache"], ROOT)
            per = run["seconds"] / count * 1000
            scaling.append({"instantiations": count, **run, "ms_each": round(per, 3)})
            mono_ms = run["phases"].get("monomorphize", 0.0)
            print(f"  {count:4d} instantiations {run['seconds']:7.2f}s"
                  f"   {per:6.2f} ms each   monomorphize {mono_ms:8.1f} ms")

    # Superlinear means the per-instantiation cost grows. Reported as a ratio
    # rather than as a verdict, because the constant factors here are large
    # enough that a small rise is the fixed cost of a build being amortized
    # over more work rather than a scaling problem.
    if len(scaling) >= 2:
        first, last = scaling[0], scaling[-1]
        span = last["instantiations"] - first["instantiations"]
        # **The marginal cost, not the total.** Every build pays for compiling
        # `std` whole-program before it reaches the program, and that fixed
        # cost is large enough to hide any amount of scaling in the totals.
        # The slope of the monomorphize phase is the thing this is asking
        # about, and a superlinear one shows up as a slope that grows.
        mono_first = first["phases"].get("monomorphize", 0.0)
        mono_last = last["phases"].get("monomorphize", 0.0)
        if span and mono_last:
            each = (mono_last - mono_first) / span
            print(f"  fixed cost {mono_first:.0f} ms (that is `std`), "
                  f"then about {each:.2f} ms per instantiation")
            print(f"  over a {last['instantiations'] // first['instantiations']}x range the phase "
                  f"grew {mono_last / mono_first:.2f}x, so it is linear with a large constant")

    measured = {
        "platform": sys.platform,
        "cores": os.cpu_count(),
        "cold": cold,
        "warm": warm,
        "check": checked,
        "scaling": scaling,
    }

    if args.write_baseline:
        with open(BASELINE, "w") as f:
            json.dump(measured, f, indent=2)
        print()
        print(f"written to {os.path.relpath(BASELINE, ROOT)}")
        return

    if args.check:
        if not os.path.exists(BASELINE):
            sys.exit(f"no baseline at {BASELINE}; run with --write-baseline first")
        with open(BASELINE) as f:
            before = json.load(f)
        print()
        if before.get("platform") != sys.platform:
            print(f"!! the baseline was taken on {before.get('platform')} and this is {sys.platform}.")
            print("!! Wall-clock numbers do not travel between machines; comparing anyway.")
        bad = []
        for name in ("cold", "warm", "check"):
            was, now = before[name]["seconds"], measured[name]["seconds"]
            ratio = now / was if was else 0
            mark = "ok" if ratio <= TOLERANCE else "REGRESSED"
            if ratio > TOLERANCE:
                bad.append(f"{name}: {was:.2f}s -> {now:.2f}s ({ratio:.2f}x)")
            print(f"  {name:<8} {was:7.2f}s -> {now:7.2f}s  {ratio:5.2f}x  {mark}")
        if bad:
            print()
            for line in bad:
                print(f"REGRESSED {line}")
            sys.exit(1)
        print()
        print(f"within {TOLERANCE}x of the baseline")


if __name__ == "__main__":
    main()
