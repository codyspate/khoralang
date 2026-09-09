#!/bin/sh
# Khora against other languages, on one workload, under one load generator.
#
# **What is a peer and what is a floor.** `service` (Khora), Go's `net/http`
# and Bun's `Bun.serve` all parse a request and route it. The Rust control and
# Khora's `floor` do neither -- they accept, read, and write a fixed string --
# so they bound the runtime rather than competing with the others. Quoting a
# floor against a peer is the mistake `bench/README.md` exists to prevent.
#
# **Everything is cached.** Each artifact is rebuilt only when it is missing or
# older than what it is built from, so a second run is just the measurement.
# `--rebuild` forces it, `--clean` throws the workspace away.
#
# **What the numbers are.** Ratios on the machine that ran them, with the load
# generator competing for the same cores as the server. They are not throughput
# headlines, and `website/content/docs/performance/` says what a published
# number has to carry.
set -eu

cd "$(dirname "$0")/.."
root=$(pwd)
work=${KHORA_BENCH_DIR:-/general/musl-build}
conns=${KHORA_BENCH_CONNS:-64}
secs=${KHORA_BENCH_SECONDS:-5}
reps=${KHORA_BENCH_REPS:-3}

for arg in "$@"; do
    case "$arg" in
        --clean) rm -rf "$work/peers" "$work"/service-glibc "$work"/floor-glibc "$work"/loadgen; echo "  cleaned"; exit 0 ;;
        --rebuild) rm -f "$work"/peers/srv-go "$work"/peers/srv-rust "$work"/service-glibc "$work"/floor-glibc "$work"/loadgen ;;
    esac
done

mkdir -p "$work/peers"
kit="$work/kit-glibc"

if [ ! -x "$kit/bin/khora" ]; then
    echo "!! no toolchain kit at $kit -- see scripts/bench-peers.sh header" >&2
    exit 1
fi

# `newer_than a b` -- true when a is missing or older than b.
newer_than() { [ ! -f "$1" ] || [ "$2" -nt "$1" ]; }

manifest() { printf '[package]\nname = "%s"\nversion = "0.2.0"\n\n[toolchain]\nversion = "0.2.0"\n' "$1"; }

build_khora() { # $1=package name under bench/  $2=output
    if newer_than "$2" "$root/bench/$1/src/main.kh" || newer_than "$2" "$kit/bin/libkhora_rt.a"; then
        printf '  building %s\n' "$(basename "$2")"
        docker run --rm -v "$kit":/kit:ro -v "$root":/src:ro -v "$work":/out \
            -e M="$(manifest "$1")" ubuntu:24.04 sh -c '
            apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq clang >/dev/null 2>&1
            cp -r /src/bench/'"$1"' /tmp/p && cd /tmp/p && printf "%s" "$M" > khora.toml
            KHORA_PROFILE=release /kit/bin/khora build . --release -o /out/'"$(basename "$2")"' >/dev/null 2>&1'
    fi
}

if newer_than "$work/loadgen" "$root/bench/loadgen.rs"; then
    printf '  building loadgen\n'
    docker run --rm -v "$root":/src:ro -v "$work":/mb \
        -e CARGO_HOME=/mb/rustup-target/cargo -e RUSTUP_HOME=/mb/rustup-target/rustup \
        -e PATH="/mb/rustup-target/cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
        alpine:edge sh -c 'apk add -q gcc musl-dev >/dev/null 2>&1
        rustc -O --target x86_64-unknown-linux-musl -o /mb/loadgen /src/bench/loadgen.rs' >/dev/null 2>&1
fi

build_khora service "$work/service-glibc"
build_khora floor "$work/floor-glibc"

if newer_than "$work/peers/srv-rust" "$root/bench/control_keepalive.rs"; then
    printf '  building the Rust control\n'
    docker run --rm -v "$root":/src:ro -v "$work":/mb \
        -e CARGO_HOME=/mb/rustup-target/cargo -e RUSTUP_HOME=/mb/rustup-target/rustup \
        -e PATH="/mb/rustup-target/cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
        alpine:edge sh -c 'apk add -q gcc musl-dev >/dev/null 2>&1
        rustc -O --target x86_64-unknown-linux-musl -o /mb/peers/srv-rust /src/bench/control_keepalive.rs' >/dev/null 2>&1
fi

if newer_than "$work/peers/srv-rust-fair" "$work/peers/srv-rust-fair.rs" 2>/dev/null; then
    printf '  building the matched Rust control\n'
    docker run --rm -v "$work":/mb \
        -e CARGO_HOME=/mb/rustup-target/cargo -e RUSTUP_HOME=/mb/rustup-target/rustup \
        -e PATH="/mb/rustup-target/cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
        alpine:edge sh -c 'apk add -q gcc musl-dev >/dev/null 2>&1
        rustc -O --target x86_64-unknown-linux-musl -o /mb/peers/srv-rust-fair /mb/peers/srv-rust-fair.rs' >/dev/null 2>&1
fi

if [ ! -f "$work/peers/srv.go" ]; then
    cat > "$work/peers/srv.go" <<'GO'
package main

import ("net/http"; "os")

func main() {
	mux := http.NewServeMux()
	mux.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write([]byte(`{"status":"ok"}`))
	})
	http.ListenAndServe("127.0.0.1:"+os.Args[1], mux)
}
GO
fi
if [ ! -f "$work/peers/srv.ts" ]; then
    cat > "$work/peers/srv.ts" <<'TS'
Bun.serve({
  port: Number(process.argv[2]),
  hostname: "127.0.0.1",
  fetch(req) {
    const u = new URL(req.url);
    if (u.pathname === "/health")
      return new Response('{"status":"ok"}', { headers: { "Content-Type": "application/json" } });
    return new Response("not found", { status: 404 });
  },
});
TS
fi

if newer_than "$work/peers/srv-go" "$work/peers/srv.go"; then
    printf '  building the Go server\n'
    docker run --rm -v "$work/peers":/w -w /w golang:alpine sh -c \
        'go mod init peers >/dev/null 2>&1; CGO_ENABLED=0 go build -ldflags="-s -w" -o srv-go srv.go' >/dev/null 2>&1
fi

# --- run ------------------------------------------------------------------

one() { # $1=label $2=image $3=server command $4=port
    i=0
    while [ "$i" -lt "$reps" ]; do
        docker run --rm -v "$work":/mb:ro -v "$work/peers":/p:ro "$2" sh -c "
            $3 >/dev/null 2>&1 & sleep 2
            /mb/loadgen --port $4 --label x --connections $conns --seconds $secs 2>/dev/null | grep '^json'
            kill %1 2>/dev/null" 2>/dev/null | sed 's/^json //'
        i=$((i + 1))
    done | python3 -c "
import sys, json
rs=[]; f=0
for l in sys.stdin:
    try:
        d=json.loads(l); rs.append(d['rate']); f+=d['failed']
    except Exception: pass
if rs:
    rs.sort()
    print(f'{rs[len(rs)//2]:>9,.0f} req/s   failed {f}   ({\" \".join(f\"{r:.0f}\" for r in rs)})')
else:
    print('  NO DATA')"
}

printf '\n  %s connections, %ss, median of %s\n\n' "$conns" "$secs" "$reps"
printf '  parse and route\n'
printf '    %-22s ' 'Khora std::net::http'; one khora ubuntu:24.04 '/mb/service-glibc' 18952
printf '    %-22s ' 'Go net/http';          one go    ubuntu:24.04 '/p/srv-go 18952' 18952
printf '    %-22s ' 'Bun.serve';            one bun   oven/bun:latest 'bun /p/srv.ts 18952' 18952
printf '\n  floors -- accept, read, write, no parsing\n'
printf '    %-22s ' 'Khora sockets';        one floor ubuntu:24.04 '/mb/floor-glibc' 18950
printf '    %-22s ' 'Rust, matched';        one rustf ubuntu:24.04 '/p/srv-rust-fair 18952' 18952
printf '    %-22s ' 'Rust, bench control';  one rust  ubuntu:24.04 '/p/srv-rust 18952' 18952
printf '\n  `bench/control_keepalive.rs` builds its header with `format!` per request\n'
printf '  and writes head and body separately. `floor` has a constant and one\n'
printf '  write, so the matched control is the runtime comparison and the bench\n'
printf '  control is what that difference costs.\n'
echo
