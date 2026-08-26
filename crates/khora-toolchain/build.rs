//! Tells cargo that `KHORA_RELEASE` is an input.
//!
//! `RUNNING` reads it with `option_env!`, which is resolved at compile time —
//! so without this, changing the variable does not rebuild anything and the
//! binary keeps reporting whatever it was first built with. That failure is
//! silent and it looks like the packaging script not working.
fn main() {
    println!("cargo:rerun-if-env-changed=KHORA_RELEASE");
}
