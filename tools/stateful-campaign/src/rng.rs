//! Deterministic xorshift64* PRNG — no external rand crate.
//!
//! Every arithmetic operation is overflow-safe: the campaign runs under the
//! workspace-wide `arithmetic_side_effects` denial, so `wrapping_*` and
//! checked-and-defaulted forms are used throughout. A zero bound in
//! [`Rng::below`] degrades to index 0 instead of panicking; callers only pass
//! nonzero bounds.

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // SplitMix-style warmup so small seeds do not start weak.
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Self(z ^ (z >> 31))
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform-ish index in `0..bound`; `bound == 0` yields 0.
    #[inline]
    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        let reduced = self
            .next_u64()
            .checked_rem(u64::try_from(bound).unwrap_or(u64::MAX))
            .unwrap_or_default();
        usize::try_from(reduced).unwrap_or(bound.saturating_sub(1))
    }

    #[inline]
    pub fn chance(&mut self, percent: u64) -> bool {
        self.next_u64().checked_rem(100).unwrap_or(0) < percent
    }

    /// Pick a uniform-ish element. Every call site passes a non-empty literal
    /// array and `below` returns an index in `0..len`, so the fallback below
    /// is unreachable; the scoped allow carries that proof (repo precedent:
    /// provably-total indexing carries a scoped proof).
    #[allow(clippy::indexing_slicing)]
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        let index = self.below(items.len());
        &items[index.min(items.len().saturating_sub(1))]
    }

    /// Deterministic UUID-shaped u128 from the stream.
    pub fn uuid_u128(&mut self) -> u128 {
        let hi = self.next_u64();
        let lo = self.next_u64();
        (u128::from(hi) << 64) | u128::from(lo)
    }
}

#[cfg(test)]
mod tests {
    use super::Rng;

    #[test]
    fn deterministic_stream() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..64 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn below_stays_in_bounds_and_handles_zero() {
        let mut rng = Rng::new(7);
        for bound in [1usize, 2, 3, 11, 1000] {
            for _ in 0..256 {
                assert!(rng.below(bound) < bound);
            }
        }
        assert_eq!(rng.below(0), 0);
    }
}
