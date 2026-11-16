//! What a personal bootstrap page carries, sealed to the new node. Every
//! item is signed or opaque: the device verifies it against the root and
//! owner it derived from the passkey before it installs anything.
use crate::{escrow::Slot, groups::GroupRecord, tip_schemas};
use rocky_core::{Commitment, KeyEpoch, ResourceScope};
use rocky_credentials::{CredentialChain, RevocationSet, RootEscrow, SignedCapability};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// One page. The first carries the metadata and how many pages follow;
/// every page carries a share of the opaque root escrow of key epochs.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Page {
    pub(crate) metadata: Option<Box<Metadata>>,
    pub(crate) escrow: Vec<(Slot, RootEscrow)>,
    pub(crate) pages: u32,
}

/// The responder's signed view of the database.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Metadata {
    pub(crate) record: tip_schemas::Record,
    pub(crate) bootstrap: tip_schemas::Bootstrap,
    pub(crate) ttl: u64,
    /// The root's trusted grant to the owner, which every device binds.
    pub(crate) grant: SignedCapability,
    /// The responder's own chain: an owner device or a custodian.
    pub(crate) responder: CredentialChain,
    /// Membership, tombstones and policy, as the responder holds them.
    pub(crate) revocations: RevocationSet,
    /// Partitions the responder knows, without key epochs.
    pub(crate) groups: Vec<GroupRecord>,
    pub(crate) epoch_statements: BTreeMap<String, Vec<Commitment>>,
    /// Key epochs of the ciphertext the responder holds.
    pub(crate) held: BTreeSet<(ResourceScope, KeyEpoch)>,
    /// The partition naming root, sealed to the root and vouched for by an
    /// owner device under this genesis: no frame carries it.
    pub(crate) naming: Option<rocky_credentials::MetadataEscrow>,
}
