use super::Protocol;
use std::time::Duration;

/// Faults injected on one link; the default injects none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkFaults {
    /// Delay added to each write.
    pub latency: Duration,
    /// Random extra delay up to this much.
    pub jitter: Duration,
    /// Bytes per second, or `None` for unlimited.
    pub bandwidth: Option<usize>,
    /// Chance per mille that a write stalls.
    pub stalls: u16,
    /// How long a stall lasts.
    pub stall: Duration,
    /// Chance per mille that a stream is cut.
    pub cuts: u16,
    /// Bytes within which a cut stream fails.
    pub cut_within: usize,
    /// Chance per mille that a dial is refused.
    pub refusals: u16,
}
impl LinkFaults {
    pub(super) fn delays(&self) -> bool {
        !self.latency.is_zero()
            || !self.jitter.is_zero()
            || self.bandwidth.is_some()
            || self.stalls > 0
    }
}

/// A deterministic random sequence for fault injection.
#[derive(Clone, Debug)]
pub struct Draws(u64);
impl Draws {
    /// A sequence from `seed`.
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
    pub(super) fn stream(
        seed: u64,
        from: &str,
        to: &str,
        protocol: Protocol,
        ordinal: u64,
    ) -> Self {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let protocol = format!("{protocol:?}");
        for part in [from.as_bytes(), to.as_bytes(), protocol.as_bytes()] {
            for byte in part.iter().chain([&0xff]) {
                hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
            }
        }
        Self(seed ^ hash ^ ordinal.wrapping_mul(0x9e37_79b9_7f4a_7c15))
    }
    /// The next number.
    pub fn draw(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    /// The next number below `bound`, or 0 for a bound of 0.
    pub fn below(&mut self, bound: u64) -> u64 {
        match bound {
            0 => 0,
            bound => self.draw() % bound,
        }
    }
    /// True with a chance of `per_mille` in a thousand.
    pub fn chance(&mut self, per_mille: u16) -> bool {
        self.below(1000) < u64::from(per_mille)
    }
    pub(super) fn fork(&mut self) -> Self {
        Self(self.draw())
    }
    pub(super) fn delay(&mut self, faults: &LinkFaults, bytes: usize) -> (Duration, bool) {
        let jitter = u64::try_from(faults.jitter.as_micros()).unwrap_or(u64::MAX);
        let mut delay =
            faults.latency + Duration::from_micros(self.below(jitter.saturating_add(1)));
        if let Some(rate) = faults.bandwidth {
            delay +=
                Duration::from_micros((bytes as u64).saturating_mul(1000) / rate.max(1) as u64);
        }
        let stalled = self.chance(faults.stalls);
        if stalled {
            delay += faults.stall;
        }
        (delay, stalled)
    }
}
