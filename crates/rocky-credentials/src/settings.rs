use crate::tips::{OpensStage, TipGraph, signed_statement};
use crate::{CredentialChain, Endorsement, Error, IdentityProvider, PublicKey, RevocationSet};
use crate::{decode_bounded, denied, encode};
use rocky_core::{Commitment, CoordinatorIdentity, Scope};
use serde::{Deserialize, Serialize};

pub const DATABASE_SETTINGS_FORMAT: u32 = 1;
pub const MAX_COORDINATOR_BUNDLE: usize = 64 * 1024;
pub const MAX_BUNDLE_URL: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseSettingsStatement {
    pub format: u32,
    pub scope: Scope,
    pub revision: u64,
    pub coordination: Coordination,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Coordination {
    Uncoordinated,
    Coordinated { bundle: CoordinatorBundle },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoordinatorBundle {
    pub identity: CoordinatorIdentity,
    #[serde(with = "serde_bytes")]
    pub certified: Vec<u8>,
    pub url: Option<String>,
}
impl DatabaseSettingsStatement {
    pub fn validate(&self) -> Result<(), Error> {
        if self.format != DATABASE_SETTINGS_FORMAT {
            return Err(denied("unsupported database settings format"));
        }
        if self.revision == 0 {
            return Err(denied("database settings revisions start at 1"));
        }
        if let Coordination::Coordinated { bundle } = &self.coordination
            && (bundle.certified.is_empty()
                || bundle.certified.len() > MAX_COORDINATOR_BUNDLE
                || bundle
                    .url
                    .as_ref()
                    .is_some_and(|url| url.is_empty() || url.len() > MAX_BUNDLE_URL))
        {
            return Err(denied("coordinator bundle out of range"));
        }
        Ok(())
    }
    pub fn coordinator(&self) -> Option<&CoordinatorBundle> {
        match &self.coordination {
            Coordination::Coordinated { bundle } => Some(bundle),
            Coordination::Uncoordinated => None,
        }
    }
}
impl OpensStage for DatabaseSettingsStatement {}
signed_statement!(
    SignedDatabaseSettings,
    DatabaseSettingsStatement,
    DATABASE_SETTINGS_FORMAT,
    "rocky.database-settings.v1",
    "rocky.database-settings.id.v1"
);
impl SignedDatabaseSettings {
    fn check_graph(&self, _graph: &TipGraph, _trusted: bool) -> Result<(), Error> {
        if !self.endorsement.chain.0.is_empty() {
            return Err(denied("only the owner signs a database's settings"));
        }
        Ok(())
    }
}
