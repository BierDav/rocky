//! The owner's signed settings of a database, replicated with its tip
//! graph: how every device treats the database, learned by sync rather than
//! shipped with an application.
use crate::tips::{OpensStage, TipGraph, signed_statement};
use crate::{CredentialChain, Endorsement, Error, IdentityProvider, PublicKey, RevocationSet};
use crate::{decode_bounded, denied, encode};
use rocky_core::{Commitment, CoordinatorIdentity, Scope};
use serde::{Deserialize, Serialize};

pub const DATABASE_SETTINGS_FORMAT: u32 = 1;
/// Largest certified bootstrap a coordinator bundle carries.
pub const MAX_COORDINATOR_BUNDLE: usize = 64 * 1024;
/// Longest bundle URL.
pub const MAX_BUNDLE_URL: usize = 1024;

/// One revision of a database's settings, signed by its owner. The highest
/// revision governs; each names every setting, so it replaces the last.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseSettingsStatement {
    pub format: u32,
    pub scope: Scope,
    pub revision: u64,
    pub coordination: Coordination,
}
/// Whether a coordination service ends every tip's log of the database.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Coordination {
    Uncoordinated,
    /// Seals enter a graph only once they ended their tip's log at this
    /// service; a coordinated database stays coordinated by it.
    Coordinated {
        bundle: CoordinatorBundle,
    },
}
/// A coordination service: who it is and how to reach it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoordinatorBundle {
    pub identity: CoordinatorIdentity,
    /// The service's certified bootstrap, endpoints included, as its
    /// connector reads it; verified against `identity` before use.
    #[serde(with = "serde_bytes")]
    pub certified: Vec<u8>,
    /// Where the service publishes its current bundle, when it does.
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
    /// The coordinator bundle when the database is coordinated.
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
    /// Only the owner signs. Every valid revision enters the graph, whatever
    /// arrived before it: [`TipGraph::coordinator`] resolves them alike in
    /// every order, and [`TipGraph::next_settings`] refuses a revision that
    /// would uncoordinate the database or change its coordinator before
    /// it is signed.
    fn check_graph(&self, _graph: &TipGraph, _trusted: bool) -> Result<(), Error> {
        if !self.endorsement.chain.0.is_empty() {
            return Err(denied("only the owner signs a database's settings"));
        }
        Ok(())
    }
}
