# Fuzz targets

Three, all sharing the generators in `crates/khora-testgen` with the
`proptest` harnesses in `crates/khora-syntax/tests/parser_properties.rs` and
`crates/khora-fmt/tests/property.rs`. That sharing is the point: a generator
that only one of the two could drive would get half the attention.

| Target | Input | Invariant |
| --- | --- | --- |
| `parse` | raw bytes, read lossily as UTF-8 | `khora_syntax::parse` returns, and the tree reproduces the input |
| `token_soup` | bytes, turned into real Khora tokens in an arbitrary order | the same |
| `format_roundtrip` | bytes, turned into a valid Khora program | `format(format(x)) == format(x)`, and the tokens are the same ones |

```sh
cargo +nightly fuzz run parse
cargo +nightly fuzz run token_soup
cargo +nightly fuzz run format_roundtrip
cargo +nightly fuzz run parse fuzz/corpus/parse -- -max_total_time=300
```

`token_soup` uses `Vocabulary::Full` and will find the two known parser bugs
within seconds — `pub` with no declaration after it, and `extern` with no `fn`
after it. Both are documented on the ignored tests in
`crates/khora-syntax/tests/parser_properties.rs`. Until they are fixed, switch
that target to `Vocabulary::Sound` to look for anything else.

## Status on Windows

**Checked, not run.** `cargo check --bins` from this directory is clean, and
the targets have never been executed on a Windows machine: `cargo fuzz build`
fails at the link step with

```text
LINK : fatal error LNK1104: cannot open file 'clang_rt.asan_dynamic_runtime_thunk-x86_64.lib'
```

The file is present, in the MSVC toolchain's `lib/x64`, and cargo replaces the
`LIB` environment variable with its own MSVC detection, so pointing the linker
at it from the shell does not take. `-s none` fails earlier and differently —
`unresolved external symbol __start___sancov_pcs` — because libFuzzer's
coverage counters come from the sanitizer runtime that flag removes.

This is why the `proptest` layer exists and is the one wired into
`cargo test`. It runs everywhere, it runs on every commit, and it found every
bug listed above. Treat these targets as the deeper search to run on a Linux
box, not as the thing keeping the parser honest day to day.
