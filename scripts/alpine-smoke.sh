#!/bin/sh
# Installs a packaged Alpine toolchain and uses it, inside a plain Alpine.
#
#     docker run --rm -v "$PWD:/src" -w /src alpine:3.24 \
#         sh scripts/alpine-smoke.sh 0.5.0 dist
#
# **What it proves, which no test in the repository can:** that the archive
# `scripts/build-alpine.sh` wrote installs with `install.sh` on a machine that
# has never seen a Rust toolchain or this checkout's `target/`, and that the
# installed `khora` then does an Alpine user's first five minutes -- `new`,
# `build` and run the program, `check`, `fmt`, `test`, `run`, and a language
# server that reports an error in a file it is given. The only packages added
# for that are the ones the installation page tells a reader to add: `clang`
# and `lld`. `--lib` is checked last, with the `build-base` the page says it
# needs, both linked into a C program and opened with `dlopen`.
#
# **Nothing runs `khora` inside the checkout.** The checkout is a project that
# pins `Cargo.toml`'s version, and the archive carries the tag's, which for a
# release candidate differs: `khora` there refuses with "this project pins
# Khora X, which is not installed". Every command below runs in a scratch
# directory, and so does `install.sh`, whose own check would hit the same pin.
# `examples/khq` is copied out for the same reason, and so that no `std/` or
# runtime archive in the tree can be found by accident.
set -eu

version=${1:?usage: sh scripts/alpine-smoke.sh <version> <dist dir>}
dist=${2:?usage: sh scripts/alpine-smoke.sh <version> <dist dir>}
src=$(pwd)
case "$dist" in
    /*) ;;
    *) dist="$src/$dist" ;;
esac

apk add --no-cache clang lld file
fail() { printf 'alpine-smoke: %s\n' "$*" >&2; exit 1; }

work=$(mktemp -d)
cd "$work"

# --- install --------------------------------------------------------------

triple=$(sh "$src/install.sh" --print-target)
case "$triple" in
    *-unknown-linux-musl) ;;
    *) fail "install.sh chose $triple on Alpine" ;;
esac
sh "$src/install.sh" --version "$version" --from "$dist" --no-modify-path
export PATH="$HOME/.khora/bin:$PATH"
said=$(khora --version)
echo "$said"
case "$said" in
    *"$triple"*) ;;
    *) fail "the installed khora is not the $triple build" ;;
esac
# Nothing from this checkout or a Rust toolchain may be what it finds.
unset KHORA_STD KHORA_RT_LIB LLVM_SYS_221_PREFIX
command -v rustc > /dev/null 2>&1 && fail "rustc is on PATH; this is meant to be a clean machine"

# --- a new program --------------------------------------------------------

khora new hello
cd hello
cat > src/main.kh <<'EOF'
module hello::main;
import std::core::{print};
pub fn main() -> Int { print("hello from alpine"); 0 }
EOF
khora check .
khora fmt .
khora build .
file build/hello
# A program built here runs in a `FROM alpine` or `FROM scratch` image, so it
# must ask for no loader and no shared library -- and it is a PIE, so ASLR
# can move it, as it can a glibc program.
case "$(file build/hello)" in
    *"static-pie linked"*) ;;
    *) fail "khora build did not produce a static PIE on Alpine" ;;
esac
said=$(env -i ./build/hello)
[ "$said" = "hello from alpine" ] || fail "the program said: $said"
said=$(khora run . | tail -n 1)
[ "$said" = "hello from alpine" ] || fail "khora run said: $said"

# --- an example's tests ---------------------------------------------------

cp -r "$src/examples/khq" "$work/khq"
# The example inherits its version and formatter settings from the
# repository's workspace, which is not here; a standalone manifest says the
# same things.
cd "$work/khq"
sed -e 's/^version.workspace = true$/version = "0.1.0"/' \
    -e '/^\[fmt\]$/,/^workspace = true$/d' khora.toml > khora.toml.new
mv khora.toml.new khora.toml
printf '\n[toolchain]\nversion = "%s"\n' "$version" >> khora.toml
khora test .

# --- the language server --------------------------------------------------

# A real exchange: open a file with a type error and wait for the server to
# publish a diagnostic about it. An `initialize` answer alone is what a stub
# that understands nothing would give.
mkdir -p "$work/lsp/src"
cd "$work/lsp"
printf '[package]\nname = "l"\nversion = "0.1.0"\n\n[toolchain]\nversion = "%s"\n' "$version" > khora.toml
text='module l::main;\npub fn main() -> Int { let x: Int = \"nope\"; 0 }\n'
printf 'module l::main;\npub fn main() -> Int { let x: Int = "nope"; 0 }\n' > src/main.kh
uri="file://$work/lsp/src/main.kh"
frame() { printf 'Content-Length: %d\r\n\r\n%s' "${#1}" "$1"; }
{
    frame '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":"file://'"$work"'/lsp","capabilities":{}}}'
    frame '{"jsonrpc":"2.0","method":"initialized","params":{}}'
    frame '{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"'"$uri"'","languageId":"khora","version":1,"text":"'"$text"'"}}}'
    # The server answers on its own time; keep its input open until it has.
    sleep 15
    frame '{"jsonrpc":"2.0","id":2,"method":"shutdown"}'
    frame '{"jsonrpc":"2.0","method":"exit"}'
} | timeout -s 9 60 khora lsp > "$work/lsp/out" 2>/dev/null || true
grep -q '"textDocument/publishDiagnostics"' "$work/lsp/out" \
    || fail "khora lsp published no diagnostics: $(head -c 600 "$work/lsp/out")"
grep -q 'expected `Int`, found `String`' "$work/lsp/out" \
    || fail "khora lsp did not report the type error: $(head -c 600 "$work/lsp/out")"
echo "lsp: the type error was reported"

# --- a shared library -----------------------------------------------------

# The installation page says `--lib` needs `build-base`, and that the library
# can be linked into a C program or opened with `dlopen`. Both, then.
apk add --no-cache build-base
mkdir -p "$work/nl/src"
cd "$work/nl"
printf '[package]\nname = "nl"\nversion = "0.1.0"\n\n[toolchain]\nversion = "%s"\n' "$version" > khora.toml
printf 'module nl::main;\npub extern fn khora_answer() -> Int { 42 }\n' > src/main.kh
khora build . --lib -o build/libnl.so
cat > linked.c <<'EOF'
#include <stdio.h>
#include <stdint.h>
int64_t khora_answer(void);
int main(void) { printf("%lld\n", (long long)khora_answer()); return 0; }
EOF
cat > opened.c <<'EOF'
#include <stdio.h>
#include <stdint.h>
#include <dlfcn.h>
int main(int argc, char **argv) {
  (void)argc;
  void *h = dlopen(argv[1], RTLD_NOW);
  if (!h) { printf("dlopen: %s\n", dlerror()); return 1; }
  int64_t (*f)(void) = (int64_t (*)(void))dlsym(h, "khora_answer");
  printf("%lld\n", (long long)f());
  return 0;
}
EOF
cc linked.c -L build -lnl -Wl,-rpath,"$work/nl/build" -o linked
said=$(./linked)
[ "$said" = 42 ] || fail "the library linked into a C program said: $said"
cc opened.c -o opened
said=$(./opened "$work/nl/build/libnl.so")
[ "$said" = 42 ] || fail "the library opened with dlopen said: $said"
echo "lib: linked and dlopen both answered 42"

echo "alpine-smoke: $triple toolchain $version works on Alpine $(cat /etc/alpine-release)"
