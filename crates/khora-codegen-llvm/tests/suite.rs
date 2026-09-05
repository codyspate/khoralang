//! Every integration test in this crate, in one binary.
//!
//! Each file in `tests/` used to be its own executable, and each of those
//! linked the front end and all of LLVM again: sixty-eight static links, 5.5 GB
//! of near-identical binaries, for one crate's tests. They are modules of one
//! binary now, which is why `autotests = false` is in `Cargo.toml`.
//!
//! Filtering still selects a file, because a file is still a module:
//!
//!     cargo test -p khora-codegen-llvm --features llvm --test suite -- arithmetic::
//!
//! **A new test file is not discovered.** Add it below; nothing else looks.

mod harness;

mod arithmetic;
mod arrays;
mod backtick;
mod benching;
mod channels;
mod chars;
mod combinators;
mod compile;
mod config;
mod db;
mod debugging;
mod decimal;
mod derive;
mod effects;
mod env;
mod errors;
mod exporting;
mod fibers;
mod files;
mod fixed;
mod flow;
mod foreign;
mod fs;
mod hashmap;
mod http_client;
mod http_layers;
mod http;
mod interpolation;
mod json;
mod load;
mod logging;
mod modules;
mod mutation;
mod net_cancel;
mod newtypes;
mod packages;
mod phases;
mod portability;
mod postgres;
mod process_cancel;
mod process;
mod profiles;
mod random;
mod records;
mod redaction;
mod reference;
mod regions;
mod reproducible;
mod resilience;
mod reuse;
mod schedules;
mod schema;
mod shared;
mod sleeping;
mod sockets;
mod spike;
mod strings;
mod targets;
mod testing;
mod text;
mod time;
mod tls_cancel;
mod tls;
mod trace;
mod tracing;
mod traps_in_a_server;
mod tuples;
mod vector;
