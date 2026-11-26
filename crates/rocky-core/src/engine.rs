use crate::{Commitment, DETERMINISTIC_COLLATIONS, DETERMINISTIC_FUNCTIONS, commitment};
use serde::{Deserialize, Serialize};

pub const BUNDLED_SQLITE_VERSION: &str = "3.53.2";

pub const REPLAY_SEMANTICS: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EngineIdentity(#[serde(with = "crate::byte_arrays::one")] pub Commitment);
impl EngineIdentity {
    pub fn current() -> Self {
        static CURRENT: std::sync::OnceLock<EngineIdentity> = std::sync::OnceLock::new();
        *CURRENT.get_or_init(|| Self::of(BUNDLED_SQLITE_VERSION, REPLAY_SEMANTICS))
    }
    pub fn of(sqlite: &str, semantics: u32) -> Self {
        let mut bytes = Vec::new();
        let mut put = |text: &str| {
            bytes.extend((text.len() as u32).to_le_bytes());
            bytes.extend(text.as_bytes());
        };
        put(sqlite);
        for names in [DETERMINISTIC_FUNCTIONS, DETERMINISTIC_COLLATIONS] {
            let mut names = names.to_vec();
            names.sort_unstable();
            put(&names.len().to_string());
            names.into_iter().for_each(&mut put);
        }
        bytes.extend(semantics.to_le_bytes());
        Self(commitment("rocky.engine.v1", &bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_stable_and_names_every_input() {
        let current = EngineIdentity::current();
        assert_eq!(
            current,
            EngineIdentity::of(BUNDLED_SQLITE_VERSION, REPLAY_SEMANTICS)
        );
        assert_ne!(current, EngineIdentity::of("3.0.0", REPLAY_SEMANTICS));
        assert_ne!(
            current,
            EngineIdentity::of(BUNDLED_SQLITE_VERSION, REPLAY_SEMANTICS + 1)
        );
    }
}
