//! Encryption epoch identity and deterministic writer order.
use crate::{Commitment, commitment};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// A generation and the domain-separated hash of its scope secret.
/// Greater identities win: newer generations, then the lower secret hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyEpoch {
    pub generation: u64,
    pub secret_hash: Commitment,
}
impl KeyEpoch {
    pub fn new(generation: u64, secret: &[u8; 32]) -> Self {
        Self {
            generation,
            secret_hash: commitment("rocky.key-epoch.v1", secret),
        }
    }
    pub fn valid(self) -> bool {
        self.generation > 0 && self.generation <= i64::MAX as u64
    }
}
impl Ord for KeyEpoch {
    fn cmp(&self, other: &Self) -> Ordering {
        self.generation
            .cmp(&other.generation)
            .then_with(|| other.secret_hash.cmp(&self.secret_hash))
    }
}
impl PartialOrd for KeyEpoch {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn permutations(values: &mut [KeyEpoch], at: usize, winner: KeyEpoch) {
        if at == values.len() {
            assert_eq!(values.iter().copied().reduce(KeyEpoch::max), Some(winner));
            let mut held = values[0];
            for epoch in &values[1..] {
                held = held.max(*epoch);
            }
            assert_eq!(held, winner);
            return;
        }
        for index in at..values.len() {
            values.swap(at, index);
            permutations(values, at + 1, winner);
            values.swap(at, index);
        }
    }
    #[test]
    fn up_to_four_siblings_choose_the_lowest_hash_in_every_arrival_order() {
        for seed in 0..16u8 {
            let epochs: Vec<_> = (0..4)
                .map(|index| KeyEpoch::new(2, &[seed * 4 + index; 32]))
                .collect();
            for count in 1..=4 {
                let winner = *epochs[..count]
                    .iter()
                    .min_by_key(|epoch| epoch.secret_hash)
                    .unwrap();
                permutations(&mut epochs[..count].to_vec(), 0, winner);
                assert!(KeyEpoch::new(3, &[255; 32]) > winner);
            }
        }
    }
}
