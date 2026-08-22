# LLVM toolchain setup

`khora-codegen-llvm` needs LLVM only when built with `--features llvm`. The
default build — and therefore `cargo test` and all front-end work — needs no
LLVM at all.

Pinned version: **LLVM 22.1.8**, matching `inkwell` 0.10's `llvm22-1` feature
and `llvm-sys` 221.0.1.

Getting this working on Windows takes four non-obvious steps. All of them are
already applied in this repository; this document explains what and why, so a
new machine can be set up and so nobody undoes one by accident.

## 1. Use the tarball, not the installer

`LLVM-22.1.8-win64.exe` — and the `winget install LLVM.LLVM` package that wraps
it — ships the *tools* (clang, lld) but **not** `llvm-config.exe`, the `llvm-c`
headers, or the static libraries. `llvm-sys` requires all three. It also wants
administrator rights.

Use the full distribution tarball. No elevation needed, and pinning it to a
directory keeps builds reproducible and nothing leaks onto `PATH`.

```bash
mkdir -p ~/.llvm && cd ~/.llvm && curl -sL --retry 3 -o llvm.tar.xz "https://github.com/llvm/llvm-project/releases/download/llvmorg-22.1.8/clang%2Bllvm-22.1.8-x86_64-pc-windows-msvc.tar.xz"
```

862 MB compressed, roughly 5 GB extracted.

```bash
cd ~/.llvm && tar -xJf llvm.tar.xz && mv "clang+llvm-22.1.8-x86_64-pc-windows-msvc" llvm-22.1.8
```

The rename matters: `+` in a path breaks enough build tooling to be worth
avoiding.

Verify — expect `22.1.8`, ~246 `.lib` files under `lib/`, and `include/llvm-c`:

```bash
~/.llvm/llvm-22.1.8/bin/llvm-config.exe --version
```

If `llvm-config.exe` is missing you have the installer build, not the full
distribution.

Other platforms: pick the matching `clang+llvm-22.1.8-<triple>` asset from the
[LLVM 22.1.8 release](https://github.com/llvm/llvm-project/releases/tag/llvmorg-22.1.8).

## 2. Supply a stub `xml2s.lib`

`llvm-config --system-libs` advertises `xml2s.lib`, but the distribution ships
no libxml2 at all, so the link fails with
`LNK1181: cannot open input file 'xml2s.lib'`.

The only component that needs it is `LLVMWindowsManifest` — the manifest merger
behind `lld-link` and `llvm-mt`. It genuinely references 16 `xml*` symbols, but
a code generator never touches it, and static archives link lazily: if nothing
pulls those objects in, the symbols never become live. An inert `xml2s.lib` is
therefore enough to satisfy the linker's file-existence check.

It must live in the LLVM prefix's own `lib/`, because `llvm-sys` is compiled
before anything in this workspace and searches only its own paths.

```bash
cd ~/.llvm/llvm-22.1.8 && echo "int khora_xml2_stub_placeholder = 0;" > stub.c && ./bin/clang.exe -c stub.c -o stub.obj && ./bin/llvm-lib.exe "/OUT:lib/xml2s.lib" stub.obj && rm stub.c stub.obj
```

The stub holds one unused object rather than being empty: the archive
`llvm-lib /llvmlibempty` produces is rejected by `link.exe` with
`LNK1107: invalid or corrupt file`.

If a future change does pull `LLVMWindowsManifest` into the link, it will
surface as unresolved `xml*` externals rather than as silent breakage, and the
fix is to supply a real libxml2 or stub the 16 symbols properly.

## 3. Match LLVM's C runtime

**This is the one that costs hours if you miss it.** LLVM's official Windows
libraries are built `/MT` — they carry `DEFAULTLIB:libcmt` and `libcpmt` — while
rustc's `x86_64-pc-windows-msvc` target defaults to the dynamic CRT. The two
CRTs have separate heaps, so memory allocated by one and freed by the other
corrupts state.

The symptom is badly misleading: ordinary, unrelated LLVM calls fault with
`STATUS_ACCESS_VIOLATION`. During this spike it appeared successively in
`LLVMGetHostCPUName`, `LLVMCopyStringRepOfTargetData` and `LLVMVerifyModule` —
each looking like a distinct API bug, none of them actually broken. The only
hint of the real cause is an easily-ignored
`LNK4098: defaultlib 'libcmt.lib' conflicts with use of other libs`.

`.cargo/config.toml` fixes it by linking the static CRT:

```toml
[target.x86_64-pc-windows-msvc]
rustflags = [
    "-Ctarget-feature=+crt-static",
    '-Lnative=C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x64',
]
```

The second flag is required by the first. `llvm-sys`'s build script switches on
`cfg!(target_feature = "crt-static")` and then emits the Windows system
libraries (`psapi`, `shell32`, `ole32`, …) as `static=`. Those are import
libraries in the Windows SDK, so rustc must be able to find them — hence the
search path. **Update the SDK version in that path to match your machine.**

Linking the static CRT also suits the goal in §6 of shipping `khora` as a single
static binary.

## 4. Restrict inkwell's targets

`inkwell`'s default `target-all` feature references `LLVMInitialize*` for 17
architectures, but this build contains only AArch64, ARM, X86, BPF,
WebAssembly, RISCV and NVPTX (`llvm-config --targets-built`). Leaving the
default on produces 55 unresolved externals.

`khora-codegen-llvm` therefore sets `default-features = false` and enables only
`target-x86` and `target-aarch64` — exactly what §5.1 emits for.

## Configure the prefix

`.cargo/config.toml` sets `LLVM_SYS_221_PREFIX` for this workspace with
`force = false`, so an `LLVM_SYS_221_PREFIX` already exported in your
environment takes precedence. On a different machine, export your own prefix
rather than editing the committed file.

## Verify

```bash
cargo test -p khora-codegen-llvm --features llvm
```

This emits an object file with LLVM, links it with `clang` from the same prefix,
runs the result and asserts it exits 42. If that passes, the emit-link-run path
is sound and Phase 2 codegen can rely on it.

## Notes

- The backend resolves `clang` and `lld-link` from `LLVM_SYS_221_PREFIX`, never
  from `PATH`, so the linker can never drift from the LLVM `llvm-sys` was built
  against.
- `clang` is the link driver rather than `lld-link` directly: it already knows
  how to find the MSVC CRT and system libraries, and it is the same driver that
  will handle the musl and darwin cross-targets in Phase 10.
- Code is generated for a *generic* CPU, not the host. §6.1 requires bit-for-bit
  reproducible builds, which host-specific instruction selection would break.
- `~/.llvm/llvm.tar.xz` (862 MB) can be deleted once the verify step passes.
