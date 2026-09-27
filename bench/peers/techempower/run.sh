#!/bin/sh
# Measures the four TechEmpower read tests on all five server configurations.
# Verifies every server first and refuses to time if any fails a rule.
#
#     sh bench/peers/techempower/run.sh [run.py options, e.g. --rounds 5]
#
# About 5 rounds x 5 servers x 4 tests x (10 s + 5 s warm-up) = 25 minutes,
# plus verification (~4 minutes). Run it on a quiet machine: the numbers are
# only worth the load average printed beside them.
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
PG=${PG:-/general/khora-tmp/pg}
WRK=${WRK:-/general/khora-tmp/wrk/wrk}
export PG WRK

[ -x "$WRK" ] || { echo "no wrk at $WRK (see README.md)"; exit 1; }
for f in "$HERE/khora/build/techempower" "$HERE/go/techempower" "$HERE/node/node_modules/pg"; do
  [ -e "$f" ] || { echo "missing $f: build the servers first (README.md)"; exit 1; }
done

# Postgres must be running, and on its own CPUs only: a database sharing the
# server's CPUs would be charged to whichever server was running.
PG_CPUS=${PG_CPUS:-7}
export PG_CPUS
[ -f "$PG/data/postmaster.pid" ] || { echo "Postgres is not running: PG_CPUS=$PG_CPUS sh $PG/start.sh"; exit 1; }
master=$(head -1 "$PG/data/postmaster.pid")
affinity=$(taskset -pc "$master" | sed 's/.*: //')
[ "$affinity" = "$PG_CPUS" ] || { echo "Postgres (pid $master) is on CPUs $affinity, not $PG_CPUS: restart it with PG_CPUS=$PG_CPUS sh $PG/start.sh"; exit 1; }

sh "$HERE/verify.sh"
python3 "$HERE/run.py" "$@"
