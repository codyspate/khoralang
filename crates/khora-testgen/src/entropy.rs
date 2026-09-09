//! The byte source every generator in this crate is driven by.
//!
//! # Why bytes rather than a `proptest` strategy
//!
//! The same generator has to serve two callers that have nothing else in
//! common: `proptest`, which hands over a `Vec<u8>` it knows how to shrink,
//! and `cargo fuzz`, which hands over whatever libFuzzer's coverage feedback
//! decided to try next. A `Strategy` cannot be driven by a fuzzer and a
//! fuzzer's `Unstructured` cannot be shrunk by `proptest`, so the shared
//! artefact is the thing underneath both: a cursor over a slice of bytes.
//!
//! # The rule that makes shrinking work
//!
//! [`Entropy::choice`] returns `0` once the seed is exhausted, and `0` for a
//! zero byte. So **alternative 0 of every choice must be the simplest one** —
//! the leaf, the empty list, the absent clause. `proptest` shrinks a byte
//! vector by truncating it and by pulling bytes toward zero, and both of those
//! moves then walk a generated program toward the smallest program that still
//! fails. Order the alternatives wrongly and shrinking still terminates, it
//! just terminates somewhere useless.

/// A cursor over a seed, plus the fuel that stops a recursive generator.
pub struct Entropy<'a> {
    bytes: &'a [u8],
    pos: usize,
    /// Decremented by every recursive construct. At zero the generators are
    /// required to emit a leaf, which is what bounds the output size when the
    /// seed is long and the choices happen to keep recursing.
    fuel: u32,
    /// How deep the generator currently is. Separate from fuel because fuel is
    /// a budget for the whole program and this is a limit on one path — a
    /// thousand sibling calls are fine and a thousand nested ones are not.
    depth: u32,
    max_depth: u32,
}

impl<'a> Entropy<'a> {
    /// A source with the default limits. See [`Entropy::with_limits`] for what
    /// they are and why.
    pub fn new(bytes: &'a [u8]) -> Entropy<'a> {
        Entropy::with_limits(bytes, DEFAULT_FUEL, DEFAULT_MAX_DEPTH)
    }

    pub fn with_limits(bytes: &'a [u8], fuel: u32, max_depth: u32) -> Entropy<'a> {
        Entropy { bytes, pos: 0, fuel, depth: 0, max_depth }
    }

    /// The next byte, or `0` once the seed runs out.
    ///
    /// Running out is the ordinary case rather than an error: a short seed
    /// should produce a small program, not a failure, because a shrunk seed is
    /// a short seed and the shrinker has to be able to keep going.
    fn byte(&mut self) -> u8 {
        let b = self.bytes.get(self.pos).copied().unwrap_or(0);
        self.pos = self.pos.saturating_add(1);
        b
    }

    /// An index into `n` alternatives, where alternative 0 is the simplest.
    ///
    /// Modulo, not rejection sampling: a byte biased toward the low
    /// alternatives is exactly the bias wanted, and rejection would consume an
    /// unpredictable number of bytes, which makes shrinking incoherent —
    /// deleting one byte would re-align every later decision.
    pub fn choice(&mut self, n: usize) -> usize {
        if n <= 1 {
            return 0;
        }
        self.byte() as usize % n
    }

    /// True with probability `num`/256, and **false when the seed is spent**,
    /// so every optional clause is absent in the shrunk-to-nothing program.
    ///
    /// True for the *top* `num` byte values rather than the bottom ones, which
    /// is what puts `false` at zero. The probability is the same either way;
    /// the direction is what makes shrinking converge on the program with no
    /// optional clauses in it.
    pub fn chance(&mut self, num: u8) -> bool {
        u16::from(self.byte()) >= 256 - u16::from(num)
    }

    /// A count in `0..=max`, weighted small.
    ///
    /// `min(a, b)` of two independent draws rather than one uniform draw: a
    /// generated program wants mostly one-element and two-element lists with
    /// the occasional long one, and a uniform count over a handful of
    /// positions produces programs whose every list is the same length.
    pub fn count(&mut self, max: usize) -> usize {
        if max == 0 {
            return 0;
        }
        let a = self.choice(max + 1);
        let b = self.choice(max + 1);
        a.min(b)
    }

    /// Enters a recursive construct, or refuses.
    ///
    /// Returns `None` when the generator must emit a leaf instead — either the
    /// program-wide fuel is spent or this path is already as deep as it is
    /// allowed to go. A caller that ignores the `None` and recurses anyway is
    /// the bug this exists to prevent.
    pub fn descend(&mut self) -> Option<Guard<'_, 'a>> {
        if self.fuel == 0 || self.depth >= self.max_depth {
            return None;
        }
        self.fuel -= 1;
        self.depth += 1;
        Some(Guard { src: self })
    }

    /// How much of the seed has been read. Only for tests of the generator.
    pub fn consumed(&self) -> usize {
        self.pos
    }
}

/// Restores the depth on the way back out. Held by value so that forgetting to
/// leave a construct is not expressible.
pub struct Guard<'g, 'a> {
    src: &'g mut Entropy<'a>,
}

impl<'a> std::ops::Deref for Guard<'_, 'a> {
    type Target = Entropy<'a>;
    fn deref(&self) -> &Entropy<'a> {
        self.src
    }
}

impl<'a> std::ops::DerefMut for Guard<'_, 'a> {
    fn deref_mut(&mut self) -> &mut Entropy<'a> {
        self.src
    }
}

impl Drop for Guard<'_, '_> {
    fn drop(&mut self) {
        self.src.depth -= 1;
    }
}

/// Total recursive constructs one program may contain.
///
/// Chosen so a 256-byte seed — `proptest`'s default upper size here — cannot
/// produce a program that takes longer to format than it takes to generate.
/// The measured worst case at this budget is a few kilobytes of source.
pub const DEFAULT_FUEL: u32 = 96;

/// How deeply one path may nest.
///
/// **Deliberately far below where the parser overflows its stack**, which is
/// about 1,550 nested delimiters on a 2 MiB thread — see
/// `crates/khora-syntax/tests/parser_properties.rs::deep_nesting_overflows_the_stack`.
/// A stack overflow aborts the process rather than failing one case, so a
/// generator that could reach it would take the whole test run with it and
/// report nothing. Depth is capped here, and the overflow is asserted
/// separately by a test that knows it is asserting a bug.
pub const DEFAULT_MAX_DEPTH: u32 = 8;

#[cfg(test)]
mod tests {
    use super::*;

    /// An exhausted seed answers 0 to everything, which is what makes the
    /// smallest shrink the simplest program rather than an arbitrary one.
    #[test]
    fn an_empty_seed_takes_every_first_alternative() {
        let mut src = Entropy::new(&[]);
        assert_eq!(src.choice(7), 0);
        assert!(!src.chance(128));
        assert_eq!(src.count(4), 0);
    }

    #[test]
    fn depth_is_restored_on_the_way_out() {
        let mut src = Entropy::with_limits(&[1, 2, 3], 100, 2);
        {
            let mut a = src.descend().expect("depth 1");
            let mut b = a.descend().expect("depth 2");
            assert!(b.descend().is_none(), "depth 3 is past the cap");
        }
        assert!(src.descend().is_some(), "the cap is a limit on one path");
    }

    /// Fuel is spent for the whole program, so a wide tree runs out too.
    #[test]
    fn fuel_is_not_refunded() {
        let mut src = Entropy::with_limits(&[], 2, 100);
        assert!(src.descend().is_some());
        assert!(src.descend().is_some());
        assert!(src.descend().is_none());
    }
}
