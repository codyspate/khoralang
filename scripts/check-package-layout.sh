#!/bin/sh
# Does a packaged toolchain carry the WebAssembly runtime, and find it?
#
#     sh scripts/check-package-layout.sh dist/khora-<version>-<triple>.tar.gz
#     sh scripts/check-package-layout.sh dist/khora-<version>-<triple>   # unpacked
#
# **What it prevents: a release whose `--target wasm32-unknown-unknown`
# cannot link.** The host runtime beside `bin/` is refused for a wasm build,
# as built for another target, so an archive without
# `targets/wasm32-unknown-unknown/libkhora_rt.a` builds no wasm module at all,
# and nothing in the repository's own tests could see it: they find the
# runtime in the cargo target directory, where CI built it.
#
# Two checks. The archive lists the file -- read from the `.tar.gz` itself
# when given one, so what is checked is what is published. And the packaged
# `khora`, with no `KHORA_RT_LIB` and no `KHORA_STD`, builds a `--lib` for
# wasm from a scratch package and writes a module that starts `\0asm`. That
# second check needs `wasm-ld`, which the compiler looks for in the LLVM
# prefix it was built with and then on PATH; where neither has it, the build
# half fails and says so, rather than passing on the listing alone. So a
# pass here says the archive is right, not that a user's machine has the
# linker.
#
# A `.zip` (Windows) is checked by its listing only: `unzip -l` where it
# exists, and the build half runs on a `.tar.gz` or a directory.
set -eu

given=${1:?usage: check-package-layout.sh <archive or unpacked directory>}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM
member=targets/wasm32-unknown-unknown/libkhora_rt.a
failed=0

case "$given" in
    *.tar.gz)
        if tar tzf "$given" | grep -q "^[^/]*/$member\$"; then
            echo "ok    listed: $member"
        else
            echo "FAIL  $given does not contain $member"
            failed=1
        fi
        tar xzf "$given" -C "$work"
        stage=$(find "$work" -mindepth 1 -maxdepth 1 -type d | head -n 1)
        ;;
    *.zip)
        if unzip -l "$given" | grep -q "$member\$"; then
            echo "ok    listed: $member"
            exit 0
        fi
        echo "FAIL  $given does not contain $member"
        exit 1
        ;;
    *)
        stage=$given
        if [ -f "$stage/$member" ]; then
            echo "ok    present: $member"
        else
            echo "FAIL  $stage has no $member"
            failed=1
        fi
        ;;
esac

khora="$stage/bin/khora"
[ -x "$khora" ] || khora="$stage/bin/khora.exe"
if [ ! -x "$khora" ]; then
    echo "FAIL  no bin/khora in $stage"
    exit 1
fi
khora=$(cd "$(dirname "$khora")" && pwd)/$(basename "$khora")

package="$work/wasm-lib"
mkdir -p "$package/src"
# **`--version` runs from `$work`, which has no manifest.** From the checkout
# root it read that root's `khora.toml`, which pins the repository's version;
# for an rc the packaged binary is the tag's version, refused that pin, and
# printed nothing, so the probe package said `version = ""` and the check
# failed on every rc for a reason that had nothing to do with the archive.
version=$(cd "$work" && "$khora" --version 2>/dev/null | awk '{print $2}')
if [ -z "$version" ]; then
    echo "FAIL  \`$khora --version\` printed no version:"
    (cd "$work" && "$khora" --version 2>&1 | sed 's/^/        /')
    exit 1
fi
cat > "$package/khora.toml" <<EOF
[package]
name = "wasmlib"
version = "0.1.0"

[toolchain]
version = "$version"
EOF
cat > "$package/src/lib.kh" <<'EOF'
module wasmlib;

/// Units times scale.
pub extern fn price(units: Int, scale: Int) -> Int { units * scale }
EOF

if (cd "$package" && env -u KHORA_RT_LIB -u KHORA_STD "$khora" build . --lib \
        --target wasm32-unknown-unknown) > "$work/said" 2>&1; then
    module=$(find "$package/build" -type f -name 'wasmlib.*' ! -name '*.h' ! -name '*.o' | head -n 1)
    if [ -n "$module" ] && [ "$(head -c 4 "$module" | od -An -c | tr -d ' ')" = '\0asm' ]; then
        echo "ok    built a wasm module with the packaged runtime: ${module#"$package"/}"
    else
        echo "FAIL  the build passed and wrote no wasm module:"
        find "$package/build" -type f | sed 's/^/        /'
        failed=1
    fi
else
    echo "FAIL  the packaged khora could not build for wasm32-unknown-unknown:"
    sed 's/^/        /' "$work/said"
    failed=1
fi

if [ "$failed" -ne 0 ]; then
    echo "check-package-layout: the archive cannot build for wasm32-unknown-unknown" >&2
    exit 1
fi
