//! The engine identity a tip's schema manifest commits: everything besides
//! the schema and the intents that decides what a replayed transaction does.
//! A replica whose build has another identity follows the tip read-only and
//! replays nothing until the owner opens a tip with its identity.
use crate::{Commitment, DETERMINISTIC_COLLATIONS, DETERMINISTIC_FUNCTIONS, commitment};
use serde::{Deserialize, Serialize};

/// The SQLite version `rocky-storage` bundles. Storage refuses to open a
/// connection on another SQLite, so the identity never names the wrong one.
pub const BUNDLED_SQLITE_VERSION: &str = "3.53.2";

/// Rocky's own replay semantics. Bump it in the same change as anything
/// else that can change a replay result without changing SQLite, the
/// function allowlist or the collations: a registered function such as the
/// `checked_*` arithmetic, the statement rewrite that calls them, partition
/// filters, drop rules or the canonical order.
pub const REPLAY_SEMANTICS: u32 = 1;

/// A digest of the SQLite version, the deterministic functions and
/// collations, and [`REPLAY_SEMANTICS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EngineIdentity(pub Commitment);
impl EngineIdentity {
    /// This build's identity.
    pub fn current() -> Self {
        static CURRENT: std::sync::OnceLock<EngineIdentity> = std::sync::OnceLock::new();
        *CURRENT.get_or_init(|| Self::of(BUNDLED_SQLITE_VERSION, REPLAY_SEMANTICS))
    }
    /// The identity of a build on `sqlite` with replay semantics `semantics`.
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
