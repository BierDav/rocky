use crate::{Database, Failure};
pub use rocky_credentials::TipAuthority;
pub use rocky_secure::HeldSeal;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

type Reply<T> = oneshot::Sender<Result<T, Failure>>;

pub(crate) enum Probe {
    Status(Reply<TipStatus>),
    Inspect(Reply<TipInspection>),
    Seal(Reply<TipStatus>),
    Compact(usize, Reply<TipCompaction>),
    Compaction(Reply<TipCompaction>),
    DiscoveryTopic(Reply<[u8; 32]>),
}
impl Probe {
    pub(crate) fn fail(self, error: Failure) {
        match self {
            Self::Status(reply) | Self::Seal(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Inspect(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Compact(_, reply) | Self::Compaction(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::DiscoveryTopic(reply) => {
                let _ = reply.send(Err(error));
            }
        }
    }
}

impl crate::Configuration {
    pub fn fixture_person(self) -> Self {
        if self.identity.is_some() {
            return self;
        }
        let secret = rocky_core::commitment(
            "rocky.sdk.fixture-person",
            self.directory.to_string_lossy().as_bytes(),
        );
        self.identity(std::sync::Arc::new(
            rocky_credentials::key_bytes::identity_key(secret),
        ))
    }
}

impl Database {
    pub async fn tip_status(&self) -> Result<TipStatus, crate::Error> {
        self.send(|reply| crate::request::Request::Tips(Probe::Status(reply)))
            .await
    }
    pub async fn inspect_tips(&self) -> Result<TipInspection, crate::Error> {
        self.send(|reply| crate::request::Request::Tips(Probe::Inspect(reply)))
            .await
    }
    pub async fn seal(&self) -> Result<TipStatus, crate::Error> {
        self.send(|reply| crate::request::Request::Tips(Probe::Seal(reply)))
            .await
    }
    pub async fn compact(&self, keep: usize) -> Result<TipCompaction, crate::Error> {
        self.send(|reply| crate::request::Request::Tips(Probe::Compact(keep, reply)))
            .await
    }
    pub async fn compaction(&self) -> Result<TipCompaction, crate::Error> {
        self.send(|reply| crate::request::Request::Tips(Probe::Compaction(reply)))
            .await
    }
    pub async fn discovery_topic(&self) -> Result<[u8; 32], crate::Error> {
        self.send(|reply| crate::request::Request::Tips(Probe::DiscoveryTopic(reply)))
            .await
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipStatus {
    pub bookmark: Option<String>,
    pub tip: String,
    pub operations_since_seal: u64,
    pub evidence_releasable: Option<TipEvidence>,
    pub devices: Vec<TipDevice>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipEvidence {
    pub operations: u64,
    pub bytes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipDevice {
    pub node: String,
    pub tip: Option<String>,
    pub frontiers: Vec<TipScopeFrontier>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipScopeFrontier {
    pub scope: rocky_core::ResourceScope,
    pub logs: Vec<TipFrontierLog>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipFrontierLog {
    pub author: String,
    pub held: u64,
    pub applied: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipAuthorMark {
    pub author: rocky_core::WriterId,
    pub sequence: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipSeal {
    pub id: String,
    pub tip: String,
    pub frontier: Vec<TipAuthorMark>,
    pub action: TipSealAction,
    pub schema: Option<String>,
    pub canonical: bool,
    pub authority: TipAuthority,
    pub revoked: bool,
    pub checkpoints: Vec<TipCheckpoint>,
    pub held: Option<HeldSeal>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TipSealAction {
    Continue,
    Close,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipCheckpoint {
    pub id: String,
    pub scope: rocky_core::ResourceScope,
    pub checkpoint: String,
    pub signer: String,
    pub authority: TipAuthority,
    pub rows: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipOpening {
    pub id: String,
    pub seal: String,
    pub schema: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookmarkMove {
    pub id: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub authority: TipAuthority,
    pub revoked: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LosingMove {
    pub name: String,
    #[serde(rename = "move")]
    pub movement: String,
    pub tip: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub name: String,
    pub tip: Option<String>,
    pub moves: Vec<BookmarkMove>,
    pub losers: Vec<LosingMove>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipNode {
    pub node: String,
    pub followed_tip: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipInspection {
    pub scope: rocky_core::Scope,
    pub genesis: String,
    pub schema: String,
    pub seals: Vec<TipSeal>,
    pub openings: Vec<TipOpening>,
    pub bookmarks: Vec<Bookmark>,
    pub nodes: Vec<TipNode>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipCompaction {
    pub finalized: Option<String>,
    pub compacted: Option<String>,
    pub uncompacted: Vec<String>,
    pub waiting: Option<String>,
    pub rebootstrap: Option<String>,
    pub pruned: u64,
    pub blocked: Vec<TipPruneBlock>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipPruneBlock {
    pub scope: rocky_core::ResourceScope,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn tip_enums_serialize_as_the_strings_inspect_tips_reported() -> Result<(), serde_json::Error> {
        let text = |value: serde_json::Value| value.as_str().map(str::to_owned);
        for (action, name) in [
            (TipSealAction::Continue, "continue"),
            (TipSealAction::Close, "close"),
        ] {
            assert_eq!(text(serde_json::to_value(action)?).as_deref(), Some(name));
            assert_eq!(
                serde_json::from_value::<TipSealAction>(name.into())?,
                action
            );
        }
        for (authority, name) in [
            (TipAuthority::Owner, "owner"),
            (TipAuthority::Trusted, "trusted"),
            (TipAuthority::Issuer, "issuer"),
        ] {
            assert_eq!(
                text(serde_json::to_value(authority)?).as_deref(),
                Some(name)
            );
            assert_eq!(
                serde_json::from_value::<TipAuthority>(name.into())?,
                authority
            );
        }
        for (held, name) in [
            (HeldSeal::Checkpoint, "checkpoint"),
            (HeldSeal::Live, "live"),
        ] {
            assert_eq!(text(serde_json::to_value(held)?).as_deref(), Some(name));
            assert_eq!(serde_json::from_value::<HeldSeal>(name.into())?, held);
        }
        Ok(())
    }
}
