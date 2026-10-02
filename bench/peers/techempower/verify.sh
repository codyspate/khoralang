#!/bin/sh
# Checks all five server configurations against TechEmpower's rules, one at a
# time, on the same port. Exits non-zero if any fails.
#
#     sh bench/peers/techempower/verify.sh            # all five
#     sh bench/peers/techempower/verify.sh go node    # some
#
# Needs the benchmark Postgres running (see README.md) and each server built.
# Nothing here times anything: a server that fails a rule has no number.
HERE=$(cd "$(dirname "$0")" && pwd)
PORT=${PORT:-8080}
PG=${PG:-/general/khora-tmp/pg}
BUN=${BUN:-$HOME/.bun/bin/bun}
export PSQL=${PSQL:-$PG/root/usr/lib/postgresql/17/bin/psql}
export LD_LIBRARY_PATH=$PG/root/usr/lib/x86_64-linux-gnu
export PORT PGPORT=${PGPORT:-5432}

# name -> the command that starts it, in the foreground
start() {
  case "$1" in
    khora-threads)   KHORA_FIBERS=threads   exec "$HERE/khora/build/techempower" ;;
    khora-scheduler) KHORA_FIBERS=scheduler exec "$HERE/khora/build/techempower" ;;
    go)              exec "$HERE/go/techempower" ;;
    node)            exec node "$HERE/node/server.mjs" ;;
    bun)             exec "$BUN" "$HERE/bun/server.js" ;;
    *) echo "unknown server $1" >&2; exit 2 ;;
  esac
}

ready() {
  python3 -c "
import socket, sys, time
end = time.time() + 20
while time.time() < end:
    try:
        socket.create_connection(('127.0.0.1', $PORT), 0.5).close(); sys.exit(0)
    except OSError:
        time.sleep(0.1)
sys.exit(1)"
}

# name -> whether it serves /pipelined-queries. Node's `pg` sends one
# statement at a time per connection, so it has no pipelined route;
# README.md says why.
pipelined() {
  case "$1" in
    khora-threads|khora-scheduler|go|bun) echo --pipelined ;;
    *) echo "" ;;
  esac
}

servers=${*:-khora-threads khora-scheduler go node bun}
failed=""
for name in $servers; do
  echo "=== $name"
  ( start "$name" ) > "/tmp/te-verify-$name.log" 2>&1 &
  pid=$!
  if ! ready; then
    echo "$name: did not start listening on $PORT"; cat "/tmp/te-verify-$name.log"
    failed="$failed $name"; kill -9 $pid 2>/dev/null; continue
  fi
  # Connections warmed before the query counts, so a pool opening lazily is
  # not counted against it.
  python3 "$HERE/verify.py" --port "$PORT" --label "$name" $(pipelined "$name") || failed="$failed $name"
  kill $pid 2>/dev/null; sleep 1; kill -9 $pid 2>/dev/null
  wait $pid 2>/dev/null
done

if [ -n "$failed" ]; then
  echo "FAILED:$failed"
  exit 1
fi
echo "all passed: $servers"
