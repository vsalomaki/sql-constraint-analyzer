/// `SplitMix64`: small, fast, and fixed here, so a seed gives the same export across crate upgrades.
#[derive(Debug, Clone)]
pub(crate) struct Rng(u64);

const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

impl Rng {
    /// A generator for one `stream` (a table, the file order, …) of a run, independent of the other streams.
    pub(crate) fn new(seed: u64, stream: u64) -> Self {
        let mut mixer = Self(seed ^ stream.wrapping_mul(GOLDEN));
        Self(mixer.next_u64())
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(GOLDEN);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (the modulo bias is irrelevant for test data); `0` when `n` is `0`.
    pub(crate) fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    pub(crate) fn chance_ppm(&mut self, parts_per_million: u32) -> bool {
        self.below(1_000_000) < u64::from(parts_per_million)
    }

    pub(crate) fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        let len = u64::try_from(items.len()).ok()?;
        items.get(usize::try_from(self.below(len)).ok()?)
    }

    pub(crate) fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let Ok(bound) = u64::try_from(i + 1) else {
                return;
            };
            if let Ok(j) = usize::try_from(self.below(bound)) {
                items.swap(i, j);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_and_stream_give_the_same_sequence() {
        let a: Vec<u64> = (0..5)
            .scan(Rng::new(7, 1), |r, _| Some(r.next_u64()))
            .collect();
        let b: Vec<u64> = (0..5)
            .scan(Rng::new(7, 1), |r, _| Some(r.next_u64()))
            .collect();
        let c: Vec<u64> = (0..5)
            .scan(Rng::new(7, 2), |r, _| Some(r.next_u64()))
            .collect();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn the_first_values_are_pinned() {
        // A change here changes every generated fixture.
        let mut rng = Rng::new(1, 0);
        assert_eq!(
            [rng.next_u64(), rng.next_u64()],
            [0x5e41_ab08_7439_611e, 0xf18d_6ce9_3d6c_f1ee]
        );
    }

    #[test]
    fn below_stays_in_range() {
        let mut rng = Rng::new(3, 3);
        assert!((0..1000).all(|_| rng.below(7) < 7));
        assert_eq!(rng.below(0), 0);
    }
}
