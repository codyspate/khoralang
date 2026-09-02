//! Where a build's time went, when somebody asks.
//!
//! `KHORA_TIMINGS=1` makes a build print one line per phase to stderr and a
//! total at the end. Off by default and costing an environment-variable read
//! per build when off, because a compiler that prints timings nobody asked for
//! is a compiler whose output cannot be diffed.
//!
//! **Phases, not a profiler.** These are the five boundaries a person tuning a
//! build can act on: how long it took to resolve and type the program, to
//! decide which specializations exist, to generate IR for them, to optimize
//! and encode an object, and to link it. A regression in one of those points
//! at different work than a regression in another -- monomorphization growing
//! superlinearly is a different problem from the linker being slow -- and
//! `scripts/compiler-perf.py` reads these lines to say which moved.
//!
//! The numbers are wall clock and include everything the phase waited for.
//! That is the honest choice for a compiler whose back end calls a linker: CPU
//! time would quietly stop counting the process a build spends most of its
//! time inside on a cold run.

use std::time::Instant;

/// Whether this build was asked for timings.
pub fn wanted() -> bool {
    std::env::var_os("KHORA_TIMINGS").is_some_and(|v| v != "0")
}

/// A phase that reports how long it took when it is dropped.
///
/// Dropped rather than stopped explicitly, so that an early `return` -- which
/// every one of these phases can do, because each can fail -- still reports.
/// A phase that only printed on success would go quiet exactly when a build
/// got slow enough to investigate.
pub struct Phase {
    name: &'static str,
    began: Instant,
    live: bool,
}

impl Phase {
    pub fn start(name: &'static str) -> Phase {
        Phase { name, began: Instant::now(), live: wanted() }
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        if !self.live {
            return;
        }
        eprintln!("khora-timing {:<16} {:>9.3} ms", self.name, self.began.elapsed().as_secs_f64() * 1000.0);
    }
}

/// The whole build, printed last so it reads as a total under the parts.
pub struct Whole {
    began: Instant,
    live: bool,
}

impl Whole {
    pub fn start() -> Whole {
        Whole { began: Instant::now(), live: wanted() }
    }
}

impl Drop for Whole {
    fn drop(&mut self) {
        if !self.live {
            return;
        }
        eprintln!("khora-timing {:<16} {:>9.3} ms", "total", self.began.elapsed().as_secs_f64() * 1000.0);
    }
}
