//! Generic coordination metadata: commitments, errors and issue-time
//! sequences shared with the coordinator. No application payload.
use crate::*;
use sha2::{Digest, Sha256};

pub type Commitment = [u8; 32];
pub type Configuration = [u8; 16];
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CoordinationError {
    Unavailable,
    /// The coordinator's durable state is missing where it must exist.
    CoordinationStateLost,
    UnknownDomain,
    Denied,
    Configuration,
    StaleEpoch,
    /// The log head moved past the position a call expected.
    StaleHead,
    InvalidCommitment,
    RequestConflict,
    Corrupt,
    /// A retried call is older than one the coordinator already answered.
    Retired,
    /// The signed request is older than the retry window: it never applies.
    Expired,
    /// A window or quota is full. Retry the same identity later.
    Backpressure,
    /// A persisted or wire format this build refuses to migrate.
    UnsupportedFormat,
}
impl std::fmt::Display for CoordinationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CoordinationError {}
/// Coordinator calls occur outside storage transactions.
pub type CoordinationFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, CoordinationError>> + Send + 'a>>;
/// Request sequences carry their issue time in whole seconds above a
/// per-second counter, so a coordinator bounds admission and retention by
/// time without remembering every identity.
pub const ISSUE_SHIFT: u32 = 24;
/// Issue time, in seconds since the Unix epoch, of a request sequence.
pub const fn issued_at(sequence: u64) -> u64 {
    sequence >> ISSUE_SHIFT
}
/// The next strictly increasing request sequence issued at `now` seconds.
pub fn next_request_sequence(previous: u64, now: u64) -> Result<AuthorSequence, ProtocolError> {
    let floor = now
        .checked_mul(1 << ISSUE_SHIFT)
        .ok_or(ProtocolError::InvalidCounter)?;
    AuthorSequence::new(
        previous
            .checked_add(1)
            .ok_or(ProtocolError::InvalidCounter)?
            .max(floor),
    )
}
pub fn commitment(prefix: &str, payload: &[u8]) -> Commitment {
    let mut hash = Sha256::new();
    hash.update((prefix.len() as u32).to_le_bytes());
    hash.update(prefix.as_bytes());
    hash.update(payload);
    hash.finalize().into()
}
pub fn genesis(scope: Scope, configuration: Configuration) -> Commitment {
    let mut b = Vec::new();
    b.extend(scope.mesh.0);
    b.extend(scope.app.0);
    b.extend(scope.domain.0);
    b.extend(configuration);
    commitment("rocky.mvp.genesis.v1", &b)
}
