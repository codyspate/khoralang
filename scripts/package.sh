#!/bin/sh
# Assembles a toolchain somebody can download and use.
#
#     sh scripts/package.sh                 # into dist/
#     sh scripts/package.sh --out somewhere
#
# The layout is chosen so that nothing has to be configured after unpacking.
# `khora-db::standard_library` and `khora-codegen-llvm::toolchain` both look
# beside the running executable and one directory up, so:
#
#     khora-<version>-<triple>/
#       bin/khora            the compiler, with LLVM linked into it
#       bin/khora_rt.lib     the runtime every generated program links against
#       std/                 the standard library, as source
#       LICENSE-MIT LICENSE-APACHE README
#
# is found without `KHORA_STD` or `KHORA_RT_LIB` being set. Those variables
# remain the override for an unusual layout.
#
# **`std` ships as source, not as a compiled artifact**, and that is a
# consequence of a decision taken for other reasons. Khora monomorphizes the
# whole program, so there is nothing to precompile — `docs/design/compatibility.md`
# §"There is no Khora ABI". It costs compile time on every build and it buys an
# install with no per-target components: one `std/` works for every target this
# compiler can emit, where Rust needs a separate set of `.rlib`s per triple and
# a `rustup target add` to fetch them.
set -e

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

out="$root/dist"
version=""
while [ $# -gt 0 ]; do
    case "$1" in
        --out) out="$2"; shift 2 ;;
        --version) version="$2"; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

# **The tag decides, not the manifest.** A release candidate is published as
# `v0.2.0-rc.1` while `Cargo.toml` still says `0.2.0`: bumping every crate for
# each candidate is churn, and the artifact has to be named for the tag or
# `install.sh` — which builds the filename out of the tag it asked the API for
# — looks for a file nobody published.
#
# `KHORA_RELEASE` carries the same string into the binary, so `khora --version`
# reports what it was published as rather than what the manifest said.
if [ -z "$version" ]; then
    version=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
fi
KHORA_RELEASE="$version"
export KHORA_RELEASE
triple=$(rustc -vV | sed -n 's/^host: //p')
name="khora-$version-$triple"
stage="$out/$name"

case "$triple" in
    *windows*) exe=".exe"; archive="khora_rt.lib" ;;
    *)         exe="";     archive="libkhora_rt.a" ;;
esac

echo "packaging $name"

# **Release, and the runtime too.** A toolchain that ships a debug compiler is
# slower at everything, and one that ships a debug runtime hands that slowness
# to every program built with it.
cargo build --release -p khora-cli --features llvm
cargo build --release -p khora-rt

rm -rf "$stage"
mkdir -p "$stage/bin"

cp "target/release/khora$exe" "$stage/bin/"
cp "target/release/$archive" "$stage/bin/"
cp -r std "$stage/std"
cp LICENSE-MIT LICENSE-APACHE "$stage/"

# Written rather than copied: the repository's README is about building the
# compiler, and somebody who has just unpacked one wants the other half.
cat > "$stage/README" <<EOF
Khora $version — $triple

  bin/khora build .        compile the package in this directory
  bin/khora check .        parse and type check it
  bin/khora test .         run its \`test\` blocks

Put bin/ on your PATH. Nothing else needs configuring: the compiler finds
std/ and the runtime archive beside itself.

One thing is not in here, and cannot be: a linker. Khora compiles to a native
object and needs a C driver to link it against this platform's runtime.
  Windows  Visual Studio Build Tools, "Desktop development with C++"
  macOS    xcode-select --install
  Linux    clang or gcc from your package manager

Licensed MIT OR Apache-2.0. See LICENSE-MIT and LICENSE-APACHE.
EOF

# One archive per platform convention, so that whatever unpacks it on the other
# side is the thing that platform already has.
cd "$out"
case "$triple" in
    *windows*)
        rm -f "$name.zip"
        powershell -NoProfile -Command \
            "Compress-Archive -Path '$name' -DestinationPath '$name.zip'" > /dev/null
        bundle="$name.zip"
        ;;
    *)
        rm -f "$name.tar.gz"
        tar czf "$name.tar.gz" "$name"
        bundle="$name.tar.gz"
        ;;
esac

# A checksum beside every artifact, because `install.sh` verifies what it
# downloaded and a release without one gives it nothing to verify against.
if command -v sha256sum > /dev/null 2>&1; then
    sha256sum "$bundle" > "$bundle.sha256"
elif command -v shasum > /dev/null 2>&1; then
    shasum -a 256 "$bundle" > "$bundle.sha256"
else
    powershell -NoProfile -Command \
        "(Get-FileHash '$bundle' -Algorithm SHA256).Hash.ToLower() + '  $bundle'" \
        > "$bundle.sha256"
fi

printf '\n%s\n' "$out/$bundle"
cat "$bundle.sha256"
