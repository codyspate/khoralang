"""Where the test suite spends its time.

    cargo nextest run --workspace --features llvm --no-fail-fast > run.log 2>&1
    python3 scripts/test-timing.py run.log

Prints in-binary time by crate, the slowest binaries, and the slowest
individual tests. Wall clock is much lower than the total because nextest runs
binaries in parallel; what this answers is *which work exists*, and the slowest
single test is the floor no amount of parallelism gets under.

Written for roadmap 14.28 and 14.29, both of which it then argued against. See
`crates/khora-codegen-llvm/tests/phases.rs` for the per-phase half.
"""
import re
import sys
from collections import defaultdict

ANSI = re.compile(r'\x1b\[[0-9;]*m')
# "        PASS [   1.790s] (135/1708) khora-codegen-llvm::arrays  an_array_holds..."
LINE = re.compile(r'^\s*(PASS|FAIL|SLOW)\s+\[\s*([0-9.]+)s\]\s*(?:\([^)]*\))?\s*(\S+)\s+(\S+)')

by_binary = defaultdict(float)
by_crate = defaultdict(float)
counts = defaultdict(int)
tests = []

for raw in open(sys.argv[1], encoding='utf-8', errors='replace'):
    line = ANSI.sub('', raw)
    m = LINE.match(line)
    if not m:
        continue
    verdict, secs, target, name = m.group(1), float(m.group(2)), m.group(3), m.group(4)
    if verdict == 'SLOW':
        continue
    by_binary[target] += secs
    by_crate[target.split('::')[0]] += secs
    counts[target] += 1
    tests.append((secs, target, name))

total = sum(by_binary.values())
print(f'{len(tests)} tests, {total:.0f}s of in-binary time')
print()
print('by crate')
for crate, secs in sorted(by_crate.items(), key=lambda kv: -kv[1]):
    share = 100 * secs / total if total else 0
    print(f'  {secs:8.1f}s  {share:5.1f}%  {crate}')
print()
print('slowest binaries')
for target, secs in sorted(by_binary.items(), key=lambda kv: -kv[1])[:12]:
    print(f'  {secs:8.1f}s  {counts[target]:4d} tests  {target}')
print()
print('slowest tests')
for secs, target, name in sorted(tests, reverse=True)[:12]:
    print(f'  {secs:8.2f}s  {target}  {name}')
