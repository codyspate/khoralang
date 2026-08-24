//! Randomness: SplitMix64, and the seed it starts from.
//!
//! Deterministic given a seed, which is what makes a test that draws numbers
//! reproducible. `std::random` decides how a program gets one.

use super::*;
use crate::counters::COUNTER_ORDER;
use crate::time::{khora_monotonic_millis, khora_unix_millis};

//
// **Three pure functions and a seed, and no generator state here at all.** The
// state lives on the Khora side in a `Shared<Int>`, which is a cell behind a
// mutex — see `khora_shared_update`. That is the answer to "fibers are threads,
// so what serializes two of them drawing at once": the same lock every other
// shared cell uses, taken by the step that advances the state, rather than a
// second mechanism invented here. A `thread_local!` generator was the
// alternative and was rejected for one reason: it cannot be *pinned*. A test
// that seeds a handler and then spawns a fiber would get a different sequence
// in the child, and reproducibility is the entire reason randomness is a
// capability instead of a function.
//
// The generator is splitmix64: state advances by a fixed odd constant and the
// output is a bijective mix of it. Chosen because the advance is one addition —
// so the part that has to happen under the lock is as short as it can be — and
// because it needs no state beyond the one word a `Shared<Int>` holds.
//
// **This is not a cryptographic generator.** Anyone who sees 64 bits of output
// can invert the mix and predict every draw after it. It is the right thing for
// a shuffle, a jitter, a load-balancing choice or a test fixture, and the wrong
// thing for a session token or a key. A CSPRNG is a different capability with
// a different name, and giving it one is how a program says which it needed.

/// The constant splitmix64 walks its state by: the odd number nearest
/// 2^64 divided by the golden ratio, which is where the "golden gamma" name
/// comes from. Odd, so adding it repeatedly visits all 2^64 states before
/// repeating — the period is the full cycle by construction, not by luck.
const GOLDEN_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

/// The next state after `state`.
///
/// Half of the generator, split out from the mix so that the Khora side can put
/// exactly this — one wrapping addition — inside the cell's lock, and do the
/// mixing outside it. The constant lives here and only here; a Khora copy of it
/// would be a second place for the sequence to be defined from, and the two
/// drifting would break reproducibility silently.
#[unsafe(no_mangle)]
pub extern "C" fn khora_random_step(state: i64) -> i64 {
    (state as u64).wrapping_add(GOLDEN_GAMMA) as i64
}

/// The draw a state produces: splitmix64's finalizer.
///
/// A bijection, so distinct states give distinct draws and the sequence cannot
/// repeat before the state does. Pure, which is what lets it run outside the
/// lock.
#[unsafe(no_mangle)]
pub extern "C" fn khora_random_mix(state: i64) -> i64 {
    let mut z = state as u64;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 31)) as i64
}

/// A seed nobody can guess and no two runs share.
///
/// The entropy is the operating system's, reached through the one door Rust's
/// standard library opens onto it: `RandomState` is the hash-table seed, and it
/// exists precisely so that a process's hashing cannot be predicted from
/// outside — which means it is keyed from `getrandom` on Linux and
/// `BCryptGenRandom` on Windows. Using it as a seed source is why this crate
/// still has no dependencies, and `docs/design/ecosystem.md` prefers binding
/// what exists to vendoring a copy of it.
///
/// The process time and a counter go in as well, so that two seeds taken in one
/// process differ even if some future standard library hands out the same keys
/// twice. The counter is `Relaxed` for the reason all the others here are: it
/// publishes no other memory, and two fibers seeding at once need distinct
/// values rather than an ordering.
///
/// Not for keys — see the note above the constant. This is unguessable in the
/// sense that a shuffle wants and not in the sense that a cipher does.
#[unsafe(no_mangle)]
pub extern "C" fn khora_random_seed() -> i64 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_usize(NEXT.fetch_add(1, COUNTER_ORDER));
    hasher.write_i64(khora_unix_millis());
    hasher.write_i64(khora_monotonic_millis());
    khora_random_mix(hasher.finish() as i64)
}

/// A draw reduced to `[0, span)`, without division and without rejection.
///
/// Lemire's multiply-and-shift: take the draw as an unsigned 64-bit number,
/// multiply it by the span as a 128-bit product, and keep the high half. That
/// is the same thing as scaling the draw's position in `[0, 1)` up to
/// `[0, span)`, and it is one multiply where a modulo would be a division.
///
/// **The bias is real and bounded.** Because 2^64 is not usually a multiple of
/// the span, some outputs get one more of the 2^64 draws than others — a
/// relative bias of at most `span / 2^64`. For a span a program would plausibly
/// ask for that is smaller than one part in a hundred billion billion. The
/// version with no bias at all rejects and redraws, which means a loop, which
/// means the Khora side would have to hold the lock across an unbounded number
/// of steps. Not worth it for a bound nothing can measure.
///
/// An empty range stops the program, like an index outside an array does: there
/// is no number in `[low, low)` to return, and inventing `low` would turn a
/// caller's off-by-one into a value that looks legitimate everywhere it lands.
#[unsafe(no_mangle)]
pub extern "C" fn khora_random_scale(draw: i64, span: i64) -> i64 {
    if span <= 0 {
        fatal("a random range must not be empty: `low` has to be below `high`");
    }
    let wide = u128::from(draw as u64) * u128::from(span as u64);
    // The product's high half is below `span`, which is a positive `i64`, so
    // this narrowing cannot change the value.
    (wide >> 64) as i64
}
