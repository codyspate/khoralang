#!/bin/sh
# Shows verify.py can fail: a correct stand-in server must pass, and each of
# nine single-rule faults must fail. Run after changing verify.py; a checker
# that has never gone red checks nothing.
HERE=$(cd "$(dirname "$0")" && pwd)
PG=${PG:-/general/khora-tmp/pg}
export PSQL=${PSQL:-$PG/root/usr/lib/postgresql/17/bin/psql}
export LD_LIBRARY_PATH=$PG/root/usr/lib/x86_64-linux-gnu
PORT=${PORT:-8089}
wrong=""
for fault in none unescaped no-server cached-date no-clamp cached-world in-batch content-type string-ids order-by; do
  python3 "$HERE/broken_server.py" "$PORT" "$fault" &
  pid=$!
  sleep 1
  python3 "$HERE/../verify.py" --port "$PORT" --label "$fault" --requests 64 --concurrency 8 > "/tmp/te-selftest-$fault.log" 2>&1
  code=$?
  kill $pid; wait $pid 2>/dev/null
  first=$(grep -m1 FAIL "/tmp/te-selftest-$fault.log")
  if [ "$fault" = none ]; then
    [ $code -eq 0 ] && echo "ok   none passes" || { echo "WRONG none failed"; cat "/tmp/te-selftest-$fault.log"; wrong="$wrong none"; }
  else
    [ $code -ne 0 ] && echo "ok   $fault fails:$first" || { echo "WRONG $fault passed"; wrong="$wrong $fault"; }
  fi
done
[ -z "$wrong" ] && echo "selftest: verify.py fails every fault and passes the correct server" || { echo "selftest WRONG:$wrong"; exit 1; }
