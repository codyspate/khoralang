#!/bin/sh
# Which build does `install.sh` choose for this machine's C library?
#
#     sh scripts/check-install-target.sh
#
# **What it prevents: an Alpine machine downloading the glibc toolchain**,
# which cannot start there -- every command fails with a loader error, and the
# installer's own `khora --version` check is the first place it shows, after
# the download. And the reverse: a glibc machine taking the musl build because
# it happens to have a musl loader installed beside glibc, which Debian's and
# Ubuntu's `musl` package does.
#
# No container and no network: each case puts a fake `ldd` and `uname` first
# on `PATH`, answering as that machine's would, and asks `install.sh
# --print-target` what it would download. The answers are copied from the real
# tools -- musl's `ldd` writes to stderr and exits 1, which is part of what is
# being checked. Then the override (`--target`, `KHORA_INSTALL_TARGET`), and a
# glibc older than the release, whose refusal must offer the musl build.
#
#     INSTALL_SH=/path/to/install.sh   check another copy
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
script=${INSTALL_SH:-$root/install.sh}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

failed=0

# expect <name> <uname -m> <ldd output, or "none"> <ldd stream: out|err> <ldd exit> <expected triple>
expect() {
    name=$1 machine=$2 says=$3 stream=$4 status=$5 want=$6
    bin="$work/$name"
    mkdir -p "$bin"
    cat > "$bin/uname" <<EOF
#!/bin/sh
case "\$1" in
    -s) echo Linux ;;
    -m) echo $machine ;;
    *) echo Linux ;;
esac
EOF
    if [ "$says" != none ]; then
        if [ "$stream" = err ]; then
            printf '#!/bin/sh\nprintf "%%s\\n" "%s" >&2\nexit %s\n' "$says" "$status" > "$bin/ldd"
        else
            printf '#!/bin/sh\nprintf "%%s\\n" "%s"\nexit %s\n' "$says" "$status" > "$bin/ldd"
        fi
    fi
    chmod +x "$bin"/*
    # Without `ldd` anywhere: a PATH that has the fake `uname` and the shell's
    # tools but none of the system's `ldd`, which lives in the same directories.
    if [ "$says" = none ]; then
        for tool in sh cat grep awk head tr cut mktemp tar dirname rm; do
            found=$(command -v "$tool") && ln -sf "$found" "$bin/$tool"
        done
        path="$bin"
    else
        path="$bin:$PATH"
    fi
    got=$(PATH="$path" sh "$script" --print-target 2>&1) || true
    if [ "$got" = "$want" ]; then
        echo "ok    $name: $got"
    else
        echo "FAIL  $name: wanted $want, got: $got"
        failed=1
    fi
}

expect alpine-x86_64  x86_64  "musl libc (x86_64)"                 err 1 x86_64-unknown-linux-musl
expect alpine-aarch64 aarch64 "musl libc (aarch64)"                err 1 aarch64-unknown-linux-musl
expect debian-x86_64  x86_64  "ldd (Debian GLIBC 2.41-12) 2.41"    out 0 x86_64-unknown-linux-gnu
expect ubuntu-aarch64 aarch64 "ldd (Ubuntu GLIBC 2.39-0ubuntu8) 2.39" out 0 aarch64-unknown-linux-gnu
expect fedora-x86_64  x86_64  "ldd (GNU libc) 2.40"                out 0 x86_64-unknown-linux-gnu

# A machine with no `ldd` falls to which loaders are installed, and those are
# this machine's own -- so the answer is whatever this machine is.
if [ -e /lib/ld-musl-x86_64.so.1 ] && ! ls /lib64/ld-linux-* /lib/ld-linux-* > /dev/null 2>&1; then
    here=musl
else
    here=gnu
fi
expect no-ldd x86_64 none out 0 "x86_64-unknown-linux-$here"

# --- a build named by the caller -------------------------------------------

# An override is taken as named, whatever the machine says, and a triple no
# release publishes is refused rather than downloaded as nothing.
# named <name> <how: flag|env> <triple> <expected output>
named() {
    name=$1 how=$2 value=$3 want=$4
    bin="$work/debian-x86_64"
    if [ "$how" = flag ]; then
        got=$(PATH="$bin:$PATH" sh "$script" --target "$value" --print-target 2>&1) || true
    else
        got=$(PATH="$bin:$PATH" KHORA_INSTALL_TARGET="$value" sh "$script" --print-target 2>&1) || true
    fi
    case "$got" in
        "$want"*)
            echo "ok    $name: $(printf '%s' "$got" | head -n 1)" ;;
        *)
            echo "FAIL  $name: wanted $want, got: $got"
            failed=1 ;;
    esac
}
named override-flag flag x86_64-unknown-linux-musl x86_64-unknown-linux-musl
named override-env  env  aarch64-unknown-linux-musl aarch64-unknown-linux-musl
named override-bad  flag x86_64-unknown-linux-uclibc "install: --target x86_64-unknown-linux-uclibc is not a build"

# --- a glibc too old for the glibc build -------------------------------------

# Debian 12's glibc is refused before the download, and the refusal offers the
# build that runs there: the static musl one, by the flag that installs it.
# `--from` an empty directory, so a case that wrongly gets past the refusal
# fails on a missing archive rather than downloading anything.
old="$work/debian12"
mkdir -p "$old" "$work/empty"
printf '#!/bin/sh\nprintf "%%s\\n" "ldd (Debian GLIBC 2.36-9+deb12u10) 2.36"\n' > "$old/ldd"
cat > "$old/uname" <<'EOF'
#!/bin/sh
case "$1" in -m) echo x86_64 ;; *) echo Linux ;; esac
EOF
chmod +x "$old"/*
refused=$(PATH="$old:$PATH" sh "$script" --version 0.5.0 --from "$work/empty" --to "$work/home" --no-modify-path 2>&1) && {
    echo "FAIL  debian12: installed, where it should have refused"
    failed=1
}
case "$refused" in
    *"--target x86_64-unknown-linux-musl"*"glibc 2.36 is older than"*)
        echo "ok    debian12: refused, and offered the musl build" ;;
    *)
        echo "FAIL  debian12: the refusal does not offer --target x86_64-unknown-linux-musl: $refused"
        failed=1 ;;
esac
# And taking the offer gets past the refusal: what stops it now is the archive
# that is not in the empty directory, not the glibc.
taken=$(PATH="$old:$PATH" sh "$script" --version 0.5.0 --from "$work/empty" --to "$work/home" --no-modify-path --target x86_64-unknown-linux-musl 2>&1) || true
case "$taken" in
    *"older than"*)
        echo "FAIL  debian12 --target musl: still refused for glibc: $taken"
        failed=1 ;;
    *"khora-v0.5.0-x86_64-unknown-linux-musl"*|*"khora-0.5.0-x86_64-unknown-linux-musl"*)
        echo "ok    debian12 --target musl: went for the musl archive" ;;
    *)
        echo "FAIL  debian12 --target musl: $taken"
        failed=1 ;;
esac

if [ "$failed" -ne 0 ]; then
    echo "check-install-target: install.sh chooses the wrong build for at least one C library" >&2
    exit 1
fi
