//! Randomness for keys, nonces and identifiers: the operating system's.
//! With the `simulation` feature a thread may install a seeded source and a
//! simulated wall clock (see [`simulation`]); every other thread, and every
//! build without the feature, uses the operating system's.
use rand::RngCore;

/// Fills `bytes` from the operating system, or this thread's seeded source.
pub fn fill(bytes: &mut [u8]) {
    #[cfg(feature = "simulation")]
    if simulation::fill(bytes) {
        return;
    }
    rand::rngs::OsRng.fill_bytes(bytes);
}
pub fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    fill(&mut bytes);
    bytes
}
pub fn random_u64() -> u64 {
    u64::from_le_bytes(random())
}

/// A deterministic simulation's seam, for tests only: on the installing
/// thread, randomness comes from a seed, the wall clock is a fixed epoch
/// plus the elapsed time of the (paused) task clock and what [`pass`]
/// added, and tasks run as in a
/// browser Worker: `spawn_blocking` in place, `dedicated` as a local task.
/// Nothing is cryptographically random while it is installed.
#[cfg(feature = "simulation")]
pub mod simulation {
    use crate::time::{Duration, Instant};
    use std::cell::RefCell;

    struct Source {
        state: u64,
        started: Instant,
        /// Wall time the simulation let pass besides the task clock.
        passed: Duration,
    }
    thread_local! {
        static SOURCE: RefCell<Option<Source>> = const { RefCell::new(None) };
    }
    /// The simulated wall clock's start: 2026-01-01T00:00:00Z.
    const EPOCH: Duration = Duration::from_secs(1_767_225_600);

    /// Installed until dropped, on this thread only.
    #[must_use]
    pub struct Seeded(());
    impl Drop for Seeded {
        fn drop(&mut self) {
            SOURCE.with(|source| source.borrow_mut().take());
        }
    }
    /// Seeds this thread's randomness and starts its wall clock.
    pub fn seed(seed: u64) -> Seeded {
        SOURCE.with(|source| {
            *source.borrow_mut() = Some(Source {
                state: seed,
                started: Instant::now(),
                passed: Duration::ZERO,
            })
        });
        Seeded(())
    }
    /// Whether this thread runs a seeded simulation.
    pub fn installed() -> bool {
        SOURCE.with(|source| source.borrow().is_some())
    }
    /// Moves this thread's wall clock on by `by`, as between two user
    /// actions, without firing the task clock's timers.
    pub fn pass(by: Duration) {
        SOURCE.with(|source| {
            if let Some(source) = source.borrow_mut().as_mut() {
                source.passed += by;
            }
        })
    }
    pub(super) fn fill(bytes: &mut [u8]) -> bool {
        SOURCE.with(|source| {
            let mut source = source.borrow_mut();
            let Some(source) = source.as_mut() else {
                return false;
            };
            for chunk in bytes.chunks_mut(8) {
                // SplitMix64: reproducible, not secret.
                source.state = source.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
                let mut mixed = source.state;
                mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                mixed ^= mixed >> 31;
                chunk.copy_from_slice(&mixed.to_le_bytes()[..chunk.len()]);
            }
            true
        })
    }
    pub(crate) fn since_epoch() -> Option<Duration> {
        SOURCE.with(|source| {
            source
                .borrow()
                .as_ref()
                .map(|source| EPOCH + source.passed + source.started.elapsed())
        })
    }
}
