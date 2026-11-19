use rocky_core::{KeyEpoch, MAX_COUNTER};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RotationEpoch(u64);

impl RotationEpoch {
    pub(crate) fn after(newest_held: Option<KeyEpoch>) -> Option<Self> {
        let held = newest_held.map_or(0, |epoch| epoch.generation);
        held.checked_add(1)
            .filter(|generation| *generation <= MAX_COUNTER)
            .map(Self)
    }
    pub(crate) fn generation(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for RotationEpoch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn held(generation: u64) -> KeyEpoch {
        KeyEpoch::new(generation, &[7; 32])
    }

    #[test]
    fn a_rotation_targets_the_generation_after_the_newest_held_epoch() {
        assert_eq!(
            RotationEpoch::after(None).map(RotationEpoch::generation),
            Some(1)
        );
        assert_eq!(
            RotationEpoch::after(Some(held(1))).map(RotationEpoch::generation),
            Some(2)
        );
        let newest = [held(3), held(9), held(4)].into_iter().max();
        assert_eq!(
            RotationEpoch::after(newest).map(RotationEpoch::generation),
            Some(10)
        );
    }

    #[test]
    fn a_rotation_never_targets_a_generation_beyond_the_signed_counter_range() {
        assert_eq!(
            RotationEpoch::after(Some(held(MAX_COUNTER - 1))).map(RotationEpoch::generation),
            Some(MAX_COUNTER)
        );
        assert_eq!(RotationEpoch::after(Some(held(MAX_COUNTER))), None);
        assert_eq!(RotationEpoch::after(Some(held(u64::MAX))), None);
    }

    #[test]
    fn a_rotation_epoch_keeps_the_bare_generation_encoding() -> Result<(), crate::Failure> {
        let epoch = RotationEpoch::after(Some(held(6))).ok_or("epoch")?;
        assert_eq!(serde_json::to_string(&epoch)?, "7");
        assert_eq!(epoch.to_string(), "7");
        let saved = r#"{"items":2,"notes":9223372036854775807}"#;
        let targets: BTreeMap<String, RotationEpoch> = serde_json::from_str(saved)?;
        assert_eq!(targets["notes"].generation(), MAX_COUNTER);
        assert_eq!(serde_json::to_string(&targets)?, saved);
        Ok(())
    }

    #[test]
    fn only_after_bounds_a_rotation_epoch_and_a_saved_one_loads_unchecked()
    -> Result<(), crate::Failure> {
        for saved in [0, u64::MAX] {
            let loaded: RotationEpoch = serde_json::from_str(&saved.to_string())?;
            assert_eq!(
                loaded.generation(),
                saved,
                "loading accepts every generation the saved format held before the type existed"
            );
        }
        Ok(())
    }
}
