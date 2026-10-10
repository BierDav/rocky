mod custody;
mod retention;
mod transfer;
use super::command::resources;
use super::*;
use crate::storage::{bindings, collect_rows, rows};
pub use custody::{CustodyAck, DurabilityPolicy};
use p2panda_core::Signature;
use rocky_core::{
    Commitment, Coordinator, ResourceScope, RowCommandApplication, Scope, SqlValue, StrongHead,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Signed inventory is inspectable without the external recovery encryption key.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryInventory {
    pub format: u32,
    pub definition: Commitment,
    pub recipient: PublicKey,
    pub scopes: Vec<ResourceScope>,
    pub schema: u32,
    pub handler: u32,
    pub key_epochs: BTreeMap<ResourceScope, u64>,
    pub frontier: Vec<Hash>,
    pub heads: Vec<StrongHead>,
    pub closure: Commitment,
    pub revision: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryPackage {
    pub inventory: RecoveryInventory,
    exporter: PublicKey,
    credentials: CredentialChain,
    ciphertext: Vec<u8>,
    signature: Signature,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct CommandRecord {
    handle: Commitment,
    input: Commitment,
    command: EncryptedCommand,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct HandleRecord {
    handle: Commitment,
    input: Commitment,
    operation: Hash,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Payload {
    bases: Vec<(ResourceScope, crate::command::history::StrongState)>,
    pruned: Vec<PrunedCommand>,
    frames: Vec<EncryptedFrame>,
    results: Vec<ConfirmedCommand>,
    preparations: Vec<(Vec<u8>, ConfirmedCommand)>,
    commands: Vec<CommandRecord>,
    handles: Vec<HandleRecord>,
    observations: Vec<super::effects::EffectObservation>,
    durability: Vec<(Scope, DurabilityPolicy)>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PrunedCommand {
    handle: Commitment,
    input: Commitment,
    node: rocky_core::NodeId,
    sequence: u64,
    domain: Scope,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryBlock {
    MissingKey(ResourceScope),
    MissingPayload { domain: Scope, sequence: u64 },
    IncompleteScope(ResourceScope),
    Fenced(Scope),
    Generations(Scope),
    HeadChanged(Scope),
}
#[derive(Clone, Debug)]
pub struct RecoveryReport {
    pub heads: Vec<StrongHead>,
    pub blocked: Vec<RecoveryBlock>,
    pub unresolved: Vec<EncryptedCommand>,
}
impl RecoveryReport {
    pub fn ready(&self) -> bool {
        self.blocked.is_empty()
    }
}
fn large_decode<T: Serialize + serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    if bytes.len() > MAX_SECURE_BYTES {
        return Err(denied("recovery package quota"));
    }
    let value: T =
        p2panda_core::cbor::decode_cbor(bytes).map_err(|_| denied("invalid recovery package"))?;
    if encode(&value)? != bytes {
        return Err(denied("non-canonical recovery package"));
    }
    Ok(value)
}
impl RecoveryPackage {
    fn signing_bytes(&self) -> Result<Vec<u8>, Error> {
        signing_bytes(
            "rocky.recovery.v2",
            &(
                &self.inventory,
                self.exporter,
                &self.credentials,
                &self.ciphertext,
            ),
        )
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let bytes = encode(self)?;
        if bytes.len() > MAX_SECURE_BYTES {
            return Err(denied("recovery package quota"));
        }
        Ok(bytes)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        large_decode(bytes)
    }
    pub fn commitment(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.recovery-package.v2",
            &self.to_bytes()?,
        ))
    }
    fn inspect(&self, publisher: PublicKey, revocations: &RevocationSet) -> Result<(), Error> {
        self.to_bytes()?;
        if self.inventory.format != 3
            || self.inventory.scopes.is_empty()
            || self.inventory.scopes.len() > 32
            || self
                .inventory
                .scopes
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || !self
                .exporter
                .verify(&self.signing_bytes()?, &self.signature)
        {
            return Err(denied("invalid recovery inventory/signature"));
        }
        let grants = self
            .credentials
            .verify(
                publisher,
                self.inventory.definition,
                self.exporter,
                revocations,
                now()?,
            )
            .map_err(credential)?;
        authorize(&grants, &self.inventory.scopes)
    }
    fn open(&self, key: &[u8; 32]) -> Result<Payload, Error> {
        let bytes = super::wire::decrypt(key, &encode(&self.inventory)?, &self.ciphertext)?;
        if rocky_core::commitment("rocky.recovery-closure.v2", &bytes) != self.inventory.closure {
            return Err(denied("recovery closure commitment mismatch"));
        }
        let payload: Payload = large_decode(&bytes)?;
        let mut ids = BTreeSet::new();
        for frame in &payload.frames {
            if !self.inventory.scopes.contains(&frame.scope()?) || !ids.insert(frame.id()?) {
                return Err(denied("recovery frame scope/duplicate"));
            }
        }
        if ids.into_iter().collect::<Vec<_>>() != self.inventory.frontier {
            return Err(denied("recovery frontier mismatch"));
        }
        let ids: BTreeSet<_> = self.inventory.frontier.iter().copied().collect();
        for frame in &payload.frames {
            let op = frame.operation()?;
            if op
                .header
                .previous
                .iter()
                .chain(op.header.backlink.iter())
                .any(|id| !ids.contains(id))
            {
                return Err(denied("recovery dependency closure missing"));
            }
        }
        let mut confirmed = BTreeMap::new();
        let mut bases = BTreeMap::<Scope, u64>::new();
        let mut base_scopes = BTreeSet::new();
        for (scope, base) in &payload.bases {
            if !self.inventory.scopes.contains(scope) || !base_scopes.insert(*scope) {
                return Err(denied("recovery strong base scope/duplicate"));
            }
            bases
                .entry(scope.domain)
                .and_modify(|sequence| *sequence = (*sequence).min(base.sequence()))
                .or_insert(base.sequence());
            if let Some((sequence, head)) = base.head
                && confirmed
                    .insert((scope.domain, sequence), head)
                    .is_some_and(|old| old != head)
            {
                return Err(denied("recovery strong base fork"));
            }
        }
        if payload.pruned.iter().any(|record| {
            !self
                .inventory
                .scopes
                .iter()
                .any(|scope| scope.domain == record.domain)
        }) {
            return Err(denied("recovery pruned handle scope"));
        }
        for result in &payload.results {
            let package = result.strong_package()?;
            let receipt = result.receipt()?;
            if confirmed
                .insert(
                    (package.request.scope, package.sequence),
                    receipt.confirmation.new_head,
                )
                .is_some_and(|old| old != receipt.confirmation.new_head)
            {
                return Err(denied("duplicate recovery strong sequence"));
            }
        }
        for result in payload
            .results
            .iter()
            .chain(payload.preparations.iter().map(|(_, result)| result))
        {
            let package = result.strong_package()?;
            if package.sequence <= bases.get(&package.request.scope).copied().unwrap_or(0) {
                continue;
            }
            let predecessor = if package.base.sequence == 0 {
                Some(rocky_core::genesis(
                    package.request.scope,
                    package.base.configuration,
                ))
            } else {
                confirmed
                    .get(&(package.request.scope, package.base.sequence))
                    .copied()
            };
            if predecessor != Some(package.base.head) {
                return Err(denied("recovery strong dependency closure missing"));
            }
        }
        let mut epochs = BTreeMap::new();
        // A scope rotated since the oldest retained artifact spans several
        // key epochs; the inventory names the latest.
        let mut check_epoch = |scope: ResourceScope, generation: u64| -> Result<(), Error> {
            if !self.inventory.scopes.contains(&scope) {
                return Err(denied("recovery scope/key epoch mismatch"));
            }
            let latest = epochs.entry(scope).or_insert(generation);
            *latest = (*latest).max(generation);
            Ok(())
        };
        for frame in &payload.frames {
            let op = frame.operation()?;
            check_epoch(op.header.extensions.scope, op.header.extensions.generation)?;
        }
        for result in payload
            .results
            .iter()
            .chain(payload.preparations.iter().map(|(_, result)| result))
        {
            let invocation: EncryptedPackage = decode(&result.strong_package()?.request.arguments)?;
            check_epoch(invocation.scope, invocation.generation)?;
            for part in &result.parts {
                check_epoch(part.scope, part.generation)?;
            }
        }
        for command in &payload.commands {
            let invocation: EncryptedPackage = decode(&command.command.request()?.arguments)?;
            check_epoch(invocation.scope, invocation.generation)?;
        }
        for (scope, base) in &payload.bases {
            if let Some(signed) = &base.checkpoint {
                check_epoch(*scope, signed.statement.key_epoch)?;
            }
            if let Some(result) = &base.latest {
                let invocation: EncryptedPackage =
                    decode(&result.strong_package()?.request.arguments)?;
                check_epoch(invocation.scope, invocation.generation)?;
                for part in &result.parts {
                    check_epoch(part.scope, part.generation)?;
                }
            }
        }
        if epochs != self.inventory.key_epochs {
            return Err(denied("recovery key epoch inventory mismatch"));
        }
        if payload
            .handles
            .iter()
            .any(|record| !self.inventory.frontier.contains(&record.operation))
        {
            return Err(denied("deduplication outside recovery closure"));
        }
        if payload.frames.len()
            + payload.results.len()
            + payload.preparations.len()
            + payload.commands.len()
            + payload.observations.len()
            > MAX_SECURE_RECORDS
        {
            return Err(denied("recovery record quota"));
        }
        Ok(payload)
    }
}
pub(super) fn authorize(
    grants: &[rocky_core::ResourceGrant],
    scopes: &[ResourceScope],
) -> Result<(), Error> {
    for scope in scopes {
        if !grants.iter().any(|grant| {
            grant.scope == *scope
                && grant.rows == rocky_core::RowSelection::All
                && (grant.rights.contains(&rocky_core::Right::Read)
                    || grant.rights.contains(&rocky_core::Right::Custody))
        }) {
            return Err(denied("recovery custody denied"));
        }
    }
    Ok(())
}
