use crate::{escrow::Slot, groups::GroupRecord, tip_schemas};
use rocky_core::{Commitment, KeyEpoch, ResourceScope};
use rocky_credentials::{CredentialChain, RevocationSet, RootEscrow, SignedCapability};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Page {
    pub(crate) metadata: Option<Box<Metadata>>,
    pub(crate) escrow: Vec<(Slot, RootEscrow)>,
    pub(crate) pages: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Metadata {
    pub(crate) record: tip_schemas::Record,
    pub(crate) bootstrap: tip_schemas::Bootstrap,
    pub(crate) ttl: u64,
    pub(crate) grant: SignedCapability,
    pub(crate) responder: CredentialChain,
    pub(crate) revocations: RevocationSet,
    pub(crate) groups: Vec<GroupRecord>,
    pub(crate) epoch_statements: BTreeMap<String, Vec<Commitment>>,
    pub(crate) held: BTreeSet<(ResourceScope, KeyEpoch)>,
    pub(crate) naming: Option<rocky_credentials::MetadataEscrow>,
}
