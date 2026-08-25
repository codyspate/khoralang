#!/bin/sh
# A real PostgreSQL for the driver, inside WSL.
#
# **Why not Docker.** `packages/postgres/docker-compose.yml` is the answer where
# a container engine exists, and is what CI should use. This machine has
# `docker-compose` with no engine behind it and a Podman whose socket will not
# connect, and the point of a real server is to find where the driver disagrees
# with PostgreSQL rather than with my reading of the protocol — so the shortest
# route to one wins. WSL2 forwards a listening port to Windows' localhost, so a
# server here is reachable from a Khora program there.
#
# Configured for `password` authentication, not `scram-sha-256`. The driver
# cannot answer SCRAM yet and says so by name; `docs/roadmap.md` 13.12 tracks
# the hash surface that would remove the limitation. Cleartext to a loopback
# server on a development machine is a different thing from cleartext over a
# network, and this is only ever the first.
#
# Usage:  sh scripts/postgres-wsl.sh [up|down|status]
set -eu

PORT=5433
say() { printf '\n=== %s\n' "$*"; }

if ! command -v wsl >/dev/null 2>&1; then
    echo "postgres-wsl.sh: no wsl on this machine" >&2
    exit 1
fi

case "${1:-up}" in
up)
    say 'postgresql, if it is not installed yet'
    wsl -u root -e bash -lc '
        set -eu
        if ! command -v pg_ctlcluster >/dev/null 2>&1; then
            apt-get update -qq >/dev/null 2>&1
            DEBIAN_FRONTEND=noninteractive apt-get install -y -qq postgresql >/dev/null 2>&1
        fi
        pg_config --version 2>/dev/null || psql --version
    '

    say 'configured for password authentication on '"$PORT"
    # `password` rather than `scram-sha-256`, and listening on every address
    # so that Windows can reach it through WSL2's forwarding. Written with
    # `tee` rather than an editor because this runs unattended.
    wsl -u root -e bash -lc "
        set -eu
        version=\$(ls /etc/postgresql | sort -n | tail -1)
        conf=/etc/postgresql/\$version/main
        sed -i \"s/^#\\?port = .*/port = $PORT/\" \$conf/postgresql.conf
        sed -i \"s/^#\\?listen_addresses = .*/listen_addresses = '*'/\" \$conf/postgresql.conf
        sed -i \"s/^#\\?password_encryption = .*/password_encryption = md5/\" \$conf/postgresql.conf
        # The driver speaks cleartext, so every host rule has to ask for it.
        sed -i 's/scram-sha-256/password/g' \$conf/pg_hba.conf
        grep -q 'khora-driver' \$conf/pg_hba.conf || cat >> \$conf/pg_hba.conf <<'EOF'
# khora-driver: cleartext, because the driver cannot answer SCRAM yet.
host    all             all             0.0.0.0/0               password
host    all             all             ::/0                    password
EOF
        pg_ctlcluster \$version main restart || pg_ctlcluster \$version main start
    "

    say 'a khora user and database'
    wsl -u postgres -e bash -lc "
        set -eu
        psql -p $PORT -tAc \"select 1 from pg_roles where rolname='khora'\" | grep -q 1 \
          || psql -p $PORT -c \"create role khora login password 'khora'\"
        psql -p $PORT -tAc \"select 1 from pg_database where datname='khora'\" | grep -q 1 \
          || psql -p $PORT -c \"create database khora owner khora\"
        psql -p $PORT -tAc 'select version()' | head -1
    "

    printf '\n  ok    postgres is up on %s\n' "$PORT"
    ;;
down)
    wsl -u root -e bash -lc '
        version=$(ls /etc/postgresql | sort -n | tail -1)
        pg_ctlcluster $version main stop || true
    '
    printf '  ok    stopped\n'
    ;;
status)
    wsl -u root -e bash -lc "pg_isready -p $PORT" || true
    ;;
*)
    echo "usage: sh scripts/postgres-wsl.sh [up|down|status]" >&2
    exit 1
    ;;
esac
