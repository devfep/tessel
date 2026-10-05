//! A small deterministic generator (`SplitMix64`), so a seed reproduces a workload on any machine.

#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..n`. `n` must be above zero; 0 returns 0.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        usize::try_from(self.next_u64() % (n as u64)).unwrap_or(0)
    }

    /// True with probability `rate` (clamped to 0..=1).
    pub fn chance(&mut self, rate: f64) -> bool {
        let unit = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        unit < rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_stream_and_different_seed_differs() {
        let (mut a, mut b, mut c) = (Rng::new(7), Rng::new(7), Rng::new(8));
        let first: Vec<u64> = (0..5).map(|_| a.next_u64()).collect();
        let again: Vec<u64> = (0..5).map(|_| b.next_u64()).collect();
        let other: Vec<u64> = (0..5).map(|_| c.next_u64()).collect();
        assert_eq!(first, again);
        assert_ne!(first, other);
    }

    #[test]
    fn below_stays_in_range_and_chance_respects_its_extremes() {
        let mut rng = Rng::new(1);
        for _ in 0..200 {
            assert!(rng.below(7) < 7);
            assert!(!rng.chance(0.0));
            assert!(rng.chance(1.0));
        }
        assert_eq!(rng.below(0), 0);
    }
}
