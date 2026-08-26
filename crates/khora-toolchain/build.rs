//! Two compile-time inputs that cargo would otherwise not know about.
//!
//! `RUNNING` reads `KHORA_RELEASE` with `option_env!`, which is resolved at
//! compile time — so without `rerun-if-env-changed` the binary keeps reporting
//! whatever it was first built with, silently, and it looks like the packaging
//! script not working.
//!
//! `KHORA_TARGET` is the triple this build is *for*, which release archives are
//! named with. `std::env::consts` cannot produce it: it has no way to tell
//! `x86_64-pc-windows-msvc` from `x86_64-pc-windows-gnu`, and those are
//! different downloads. Cargo hands it to a build script and nowhere else.
fn main() {
    println!("cargo:rerun-if-env-changed=KHORA_RELEASE");
    println!(
        "cargo:rustc-env=KHORA_TARGET={}",
        std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string())
    );
}
