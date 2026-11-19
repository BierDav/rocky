use crate::*;
use sha2::{Digest, Sha256};

pub type Commitment = [u8; 32];
pub type Configuration = [u8; 16];
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CoordinationError {
    Unavailable,
    ClockSkew { coordinator_time: u64 },
    CoordinationStateLost,
    UnknownDomain,
    Denied,
    Configuration,
    StaleEpoch,
    StaleHead,
    InvalidCommitment,
    RequestConflict,
    Corrupt,
    Retired,
    Expired,
    Backpressure,
    UnsupportedFormat,
    Sealed,
}
impl CoordinationError {
    pub fn category(&self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::ClockSkew { .. } => "clock_skew",
            Self::CoordinationStateLost => "state_lost",
            Self::UnknownDomain => "unknown_domain",
            Self::Denied => "denied",
            Self::Configuration => "configuration",
            Self::StaleEpoch => "stale_epoch",
            Self::StaleHead => "stale_head",
            Self::InvalidCommitment => "invalid_commitment",
            Self::RequestConflict => "request_conflict",
            Self::Corrupt => "corrupt",
            Self::Retired => "retired",
            Self::Expired => "expired",
            Self::Backpressure => "backpressure",
            Self::UnsupportedFormat => "unsupported_format",
            Self::Sealed => "sealed",
        }
    }
}
impl std::fmt::Display for CoordinationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CoordinationError {}
pub type CoordinationFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, CoordinationError>> + Send + 'a>>;
pub struct CommitmentBuilder(Sha256);
impl CommitmentBuilder {
    pub fn new(prefix: &str) -> Self {
        let mut hash = Sha256::new();
        hash.update((prefix.len() as u32).to_le_bytes());
        hash.update(prefix.as_bytes());
        Self(hash)
    }
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
    pub fn finish(self) -> Commitment {
        self.0.finalize().into()
    }
}
pub fn commitment(prefix: &str, payload: &[u8]) -> Commitment {
    let mut hash = CommitmentBuilder::new(prefix);
    hash.update(payload);
    hash.finish()
}

pub fn genesis(scope: Scope, configuration: Configuration) -> Commitment {
    let mut b = Vec::new();
    b.extend(scope.mesh.0);
    b.extend(scope.app.0);
    b.extend(scope.domain.0);
    b.extend(configuration);
    commitment("rocky.mvp.genesis.v1", &b)
}

#[cfg(test)]
mod commitment_tests {
    use super::*;
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn incremental_commitments_keep_the_domain_and_payload_encoding() {
        let payload = b"abc\0xyz";
        let expected = "a14fc9ddace9459292e1194e88acfbeab9d76584c7438bdd702f9f62380ba618";
        let hex = |bytes: Commitment| {
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        assert_eq!(hex(commitment("rocky.hash-test.v1", payload)), expected);
        for size in 1..=payload.len() {
            let mut hash = CommitmentBuilder::new("rocky.hash-test.v1");
            for chunk in payload.chunks(size) {
                hash.update(chunk);
            }
            assert_eq!(hex(hash.finish()), expected);
        }
    }
}
