#!/bin/sh
# A PostgreSQL for a trial to talk to, without Docker and without root.
#
#     sh scripts/postgres-local.sh [up|down|status]
#
# **`scripts/postgres.sh` is the Docker one and stays the default.** This is
# for a machine that has no Docker daemon and no passwordless sudo, which is
# the machine the driver's SCRAM support was written on — the .debs are
# extracted into a scratch directory and the cluster runs as the calling user.
#
# It listens on **5433**, deliberately not 5432, so a database somebody already
# runs cannot be used by accident. Authentication is **scram-sha-256**, because
# a driver tested against `trust` is a driver whose authentication is untested
# and that is exactly what shipped before.
set -eu

root=${KHORA_PG_ROOT:-/tmp/pgroot}
port=${KHORA_PG_PORT:-5433}
user=${KHORA_PG_USER:-khora}
password=${KHORA_PG_PASSWORD:-khora}

bin="$root/root/usr/lib/postgresql/17/bin"
export LD_LIBRARY_PATH="$root/root/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

fetch() {
    mkdir -p "$root"
    cd "$root"
    # `apt-get download` needs no privileges: it writes .debs into the cwd.
    apt-get download postgresql-17 postgresql-client-17 postgresql-common \
        postgresql-client-common libpq5 >/dev/null 2>&1
    for package in *.deb; do dpkg-deb -x "$package" root/; done
}

case "${1:-up}" in
up)
    [ -x "$bin/initdb" ] || fetch
    if [ ! -d "$root/data" ]; then
        # The password file is read and then removed; passing it on the command
        # line would put it in this process's arguments, where any other user
        # on the machine can read it out of `ps`.
        printf '%s\n' "$password" > "$root/pw"
        chmod 600 "$root/pw"
        "$bin/initdb" -D "$root/data" -U "$user" \
            --auth-local=trust --auth-host=scram-sha-256 --pwfile="$root/pw" >/dev/null
        rm -f "$root/pw"
    fi
    "$bin/pg_ctl" -D "$root/data" -o "-p $port -k $root" -l "$root/log" start >/dev/null
    # `pg_ctl start` returns when the postmaster is up, which is not the same
    # as accepting connections. Ask until it answers rather than sleeping a
    # number somebody will have to lengthen on a slower machine.
    tries=0
    while [ "$tries" -lt 30 ]; do
        if PGPASSWORD="$password" "$bin/psql" -h 127.0.0.1 -p "$port" -U "$user" \
            -d postgres -tAc 'select 1' >/dev/null 2>&1; then
            echo "postgres: ready on $port as $user (scram-sha-256)"
            exit 0
        fi
        tries=$((tries + 1))
        sleep 1
    done
    echo "postgres-local.sh: it started but never answered; see $root/log" >&2
    exit 1
    ;;
down)
    "$bin/pg_ctl" -D "$root/data" stop >/dev/null 2>&1 || true
    echo "postgres: stopped"
    ;;
status)
    if PGPASSWORD="$password" "$bin/psql" -h 127.0.0.1 -p "$port" -U "$user" \
        -d postgres -tAc 'show password_encryption' 2>/dev/null; then
        :
    else
        echo "postgres: not answering on $port"
        exit 1
    fi
    ;;
*)
    echo "usage: postgres-local.sh [up|down|status]" >&2
    exit 1
    ;;
esac
