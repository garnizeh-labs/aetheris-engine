use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

/// A deterministic random number generator wrapper.
///
/// VS-07 §3.4: Uses `ChaCha8Rng` for cross-platform determinism as per `DETERMINISM_DESIGN` D2.
pub struct DeterministicRng {
    inner: ChaCha8Rng,
}

impl DeterministicRng {
    /// Creates a new deterministic RNG seeded with the given value.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            inner: ChaCha8Rng::seed_from_u64(seed),
        }
    }

    /// Access the underlying RNG.
    pub fn inner_mut(&mut self) -> &mut ChaCha8Rng {
        &mut self.inner
    }
}

impl Default for DeterministicRng {
    fn default() -> Self {
        Self::new(0xCAFE_BABE)
    }
}
