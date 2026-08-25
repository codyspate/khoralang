#!/bin/sh
# Brings up a PostgreSQL for the driver to be developed against, and runs the
# tests that need one.
#
# **The suite does not need this.** `crates/khora-codegen-llvm/tests/postgres.rs`
# carries a server of its own — enough of the protocol to complete a handshake
# and answer a query — and every test but one runs against it. That is what
# keeps `scripts/baseline.sh` working on a machine with no Docker, which is the
# machine this was written on.
#
# What this adds is the run that a fake cannot give: the fake was written from
# the same reading of the protocol as the driver, so the two can be wrong
# together. Only a real server settles it.
#
# Usage:  sh scripts/postgres.sh [up|down|test]
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
compose="$here/packages/postgres/docker-compose.yml"

# `docker compose` is the plugin form and `docker-compose` the standalone one.
# Both exist in the wild and neither is reliably present, so this asks.
if docker compose version >/dev/null 2>&1; then
    run_compose() { docker compose -f "$compose" "$@"; }
elif command -v docker-compose >/dev/null 2>&1; then
    run_compose() { docker-compose -f "$compose" "$@"; }
else
    echo "postgres.sh: neither \`docker compose\` nor \`docker-compose\` is available." >&2
    echo "  The rest of the suite does not need it — only the one test that talks" >&2
    echo "  to a real server, and that one skips itself." >&2
    exit 1
fi

case "${1:-up}" in
up)
    run_compose up -d
    # `up -d` returns when the container is started, not when PostgreSQL is
    # accepting connections. The healthcheck in the compose file is what knows
    # the difference, so this waits on that rather than on a sleep somebody
    # will have to lengthen on a slower machine.
    printf 'waiting for postgres'
    tries=0
    until [ "$(run_compose ps --format '{{.Health}}' 2>/dev/null | head -1)" = "healthy" ]; do
        tries=$((tries + 1))
        if [ "$tries" -gt 60 ]; then
            printf '\n'
            echo "postgres.sh: it never became healthy. Its own log:" >&2
            run_compose logs --tail 30 >&2
            exit 1
        fi
        printf '.'
        sleep 1
    done
    printf '\n  ok    postgres is up on 5433\n'
    ;;
down)
    run_compose down --volumes
    ;;
test)
    sh "$0" up
    KHORA_POSTGRES=1 cargo test -p khora-codegen-llvm --features llvm --test postgres
    ;;
*)
    echo "usage: sh scripts/postgres.sh [up|down|test]" >&2
    exit 1
    ;;
esac
