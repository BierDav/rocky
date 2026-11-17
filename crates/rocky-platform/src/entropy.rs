use rand::RngCore;

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

#[cfg(feature = "simulation")]
pub mod simulation {
    use crate::time::{Duration, Instant};
    use std::cell::RefCell;

    struct Source {
        state: u64,
        started: Instant,
        passed: Duration,
        frozen: bool,
    }
    thread_local! {
        static SOURCE: RefCell<Option<Source>> = const { RefCell::new(None) };
    }
    const EPOCH: Duration = Duration::from_secs(1_767_225_600);

    #[must_use]
    pub struct Seeded(());
    impl Drop for Seeded {
        fn drop(&mut self) {
            SOURCE.with(|source| source.borrow_mut().take());
        }
    }
    pub fn seed(seed: u64) -> Seeded {
        SOURCE.with(|source| {
            *source.borrow_mut() = Some(Source {
                state: seed,
                started: Instant::now(),
                passed: Duration::ZERO,
                frozen: false,
            })
        });
        Seeded(())
    }
    pub fn installed() -> bool {
        SOURCE.with(|source| source.borrow().is_some())
    }
    pub fn pass(by: Duration) {
        SOURCE.with(|source| {
            if let Some(source) = source.borrow_mut().as_mut() {
                source.passed += by;
            }
        })
    }
    static SKIPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    pub fn skip(by: Duration) {
        SKIPPED.fetch_add(by.as_secs(), std::sync::atomic::Ordering::SeqCst);
    }
    pub(crate) fn skipped() -> Duration {
        Duration::from_secs(SKIPPED.load(std::sync::atomic::Ordering::SeqCst))
    }
    pub fn freeze() {
        SOURCE.with(|source| {
            if let Some(source) = source.borrow_mut().as_mut() {
                source.frozen = true;
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
                let mixed = split_mix64(&mut source.state);
                chunk.copy_from_slice(&mixed.to_le_bytes()[..chunk.len()]);
            }
            true
        })
    }
    fn split_mix64(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut mixed = *state;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        mixed ^ (mixed >> 31)
    }
    pub(crate) fn since_epoch() -> Option<Duration> {
        SOURCE.with(|source| {
            source.borrow().as_ref().map(|source| match source.frozen {
                true => EPOCH,
                false => EPOCH + source.passed + source.started.elapsed(),
            })
        })
    }
}
