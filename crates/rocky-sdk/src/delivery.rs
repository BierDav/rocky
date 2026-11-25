use crate::{
    Failure,
    enrollment::Settings,
    groups::{GroupPartition, GroupRecord, record_key, secret_role},
    registry::{IdentityRecord, Registry},
};
use data_encoding::{BASE64URL_NOPAD, HEXLOWER};
use rocky_core::{Commitment, KeyEpoch, PartitionValue};
use rocky_core::{Right, RowSelection};
use rocky_credentials::{DeliveryReceipt, GroupSecretEnvelope, SealedDelivery, write_private};
use rocky_secure::{PrivateKey, PublicKey};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

pub(crate) const MAX_MAILBOXES: usize = 3;
pub(crate) const RECIPIENT_DEPOSITS: usize = 256;
pub(crate) const MAILBOX_DEPOSITS: usize = 4096;
pub(crate) const DEPOSIT_LIFETIME: u64 = 30 * 24 * 60 * 60;
pub(crate) const MAX_DEPOSIT_PAYLOAD: usize = rocky_credentials::MAX_DELIVERY_PAYLOAD;
pub(crate) const MAX_OUTBOX: usize = 16_384;
pub const MAILBOX_FULL: &str = "mailbox_full";
const CONTENTS_VERSION: u32 = 2;
const MAILBOX_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reason {
    Share,
    Rotation,
    Rights,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Contents {
    version: u32,
    pub reason: Reason,
    pub sender: Option<rocky_credentials::CredentialChain>,
    pub group: String,
    pub values: Vec<PartitionValue>,
    #[serde(with = "serde_bytes")]
    pub envelope: Vec<u8>,
}
impl Contents {
    pub fn seal(
        recipient: PublicKey,
        reason: Reason,
        record: &GroupRecord,
        envelope: Vec<u8>,
        sender: Option<rocky_credentials::CredentialChain>,
    ) -> Result<SealedDelivery, Failure> {
        if envelope.len() > 4096 {
            return Err("group envelope exceeds the 4 KiB delivery bound".into());
        }
        let contents = Self {
            version: CONTENTS_VERSION,
            reason,
            sender,
            group: record.group.clone(),
            values: record.values.clone(),
            envelope,
        };
        let payload = rocky_credentials::encode(&contents)?;
        if payload.len() > MAX_DEPOSIT_PAYLOAD {
            return Err("delivery wrapping exceeds its bound".into());
        }
        Ok(SealedDelivery::seal(recipient, &payload)?)
    }
    pub fn rights(recipient: PublicKey) -> Result<SealedDelivery, Failure> {
        let contents = Self {
            version: CONTENTS_VERSION,
            reason: Reason::Rights,
            sender: None,
            group: String::new(),
            values: Vec::new(),
            envelope: Vec::new(),
        };
        Ok(SealedDelivery::seal(
            recipient,
            &rocky_credentials::encode(&contents)?,
        )?)
    }
    pub fn open(delivery: &SealedDelivery, node: &PrivateKey) -> Result<Self, Failure> {
        let contents: Self = rocky_credentials::decode_bounded(
            &delivery.open(node)?,
            rocky_credentials::MAX_DELIVERY_PAYLOAD,
        )?;
        if contents.version != CONTENTS_VERSION || contents.envelope.len() > 4096 {
            return Err("unsupported delivery contents version".into());
        }
        Ok(contents)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    pub recipient: rocky_credentials::PeerId,
    pub carries: Carries,
    pub delivery: Option<String>,
    pub targets: Vec<rocky_credentials::PeerId>,
    pub mailboxes: Vec<rocky_credentials::PeerId>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Carries {
    Secret { address: String, epoch: KeyEpoch },
    Rights,
}
/// How far a key delivery to a device got.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// The device has the key.
    Delivered,
    /// The key waits in a mailbox for the device.
    Deposited,
    /// The key has not left this device.
    Pending,
}
impl Pending {
    #[cfg(any(test, feature = "test-support"))]
    pub fn state(&self) -> State {
        match (&self.delivery, self.mailboxes.is_empty()) {
            (None, _) => State::Delivered,
            (Some(_), false) => State::Deposited,
            (Some(_), true) => State::Pending,
        }
    }
    pub fn sealed(&self) -> Result<Option<SealedDelivery>, Failure> {
        self.delivery.as_deref().map(decode).transpose()
    }
}
fn encode(delivery: &SealedDelivery) -> Result<String, Failure> {
    Ok(BASE64URL_NOPAD.encode(&delivery.to_bytes()?))
}
fn decode(text: &str) -> Result<SealedDelivery, Failure> {
    Ok(SealedDelivery::from_bytes(
        &BASE64URL_NOPAD.decode(text.as_bytes())?,
    )?)
}
pub(crate) fn digest_key(digest: &Commitment) -> String {
    HEXLOWER.encode(digest)
}

impl Settings {
    pub(crate) fn queue(
        &mut self,
        recipient: PublicKey,
        record: &GroupRecord,
        epoch: KeyEpoch,
        delivery: &SealedDelivery,
        again: bool,
    ) -> Result<(), Failure> {
        let carries = Carries::Secret {
            address: record_key(&record.address),
            epoch,
        };
        self.enqueue(recipient, carries, delivery, again)
    }
    pub(crate) fn queue_rights_notices(
        &mut self,
        vault: &crate::secrets::Vault,
        nodes: &[PublicKey],
    ) -> Result<(), Failure> {
        let own = Self::node_key(vault)?.public_key();
        for node in nodes.iter().filter(|node| **node != own) {
            self.enqueue(*node, Carries::Rights, &Contents::rights(*node)?, true)?;
        }
        Ok(())
    }
    pub(crate) fn rights_lag(
        &self,
        registry: &Registry,
        domain: &crate::domain::Domain,
        chain: &rocky_credentials::CredentialChain,
    ) -> Result<bool, Failure> {
        let Some(identity) = chain.principal() else {
            return Ok(false);
        };
        if self.revoked_identity(&identity, registry) {
            return Ok(false);
        }
        if registry.admins.contains_key(&identity) && !self.revocations.revokes_trusted(&identity) {
            let holds_admin_grant = chain
                .capabilities()
                .trusted_identity(
                    self.publisher,
                    domain.scope,
                    &self.revocations,
                    crate::enrollment::now()?,
                )
                .is_ok_and(|trusted| trusted == identity);
            return Ok(!holds_admin_grant);
        }
        let recorded = registry.get(&identity).and_then(IdentityRecord::access);
        let held = chain.held().and_then(|held| domain.access_of(held));
        Ok(matches!((recorded, held), (Some(recorded), Some(held)) if *recorded != held))
    }
    fn enqueue(
        &mut self,
        recipient: PublicKey,
        carries: Carries,
        delivery: &SealedDelivery,
        again: bool,
    ) -> Result<(), Failure> {
        let reference = rocky_credentials::PeerId::of(recipient);
        self.delivery_nodes.insert(reference, recipient);
        let recipient = reference;
        let held = self
            .deliveries
            .iter()
            .find(|(_, pending)| pending.recipient == recipient && pending.carries == carries);
        match held {
            Some((_, pending)) if pending.delivery.is_some() || !again => return Ok(()),
            Some((digest, _)) => {
                let digest = digest.clone();
                self.deliveries.remove(&digest);
            }
            None => {}
        }
        if self.deliveries.len() >= MAX_OUTBOX {
            let evicted_delivered_digest = self
                .deliveries
                .iter()
                .find(|(_, pending)| pending.delivery.is_none())
                .map(|(digest, _)| digest.clone())
                .ok_or("delivery outbox full: 16,384 envelopes are pending")?;
            self.deliveries.remove(&evicted_delivered_digest);
        }
        self.deliveries.insert(
            digest_key(&delivery.digest()?),
            Pending {
                recipient,
                carries,
                delivery: Some(encode(delivery)?),
                targets: Vec::new(),
                mailboxes: Vec::new(),
            },
        );
        Ok(())
    }
    pub(crate) fn drops_revoked_deliveries(&self) -> bool {
        self.deliveries
            .values()
            .any(|pending| self.revocations.revokes_reference(&pending.recipient))
    }
    pub(crate) fn drop_revoked_deliveries(&mut self) {
        let revocations = &self.revocations;
        self.deliveries
            .retain(|_, pending| !revocations.revokes_reference(&pending.recipient));
    }
    fn queued(&self, recipient: PublicKey, record: &GroupRecord, epoch: KeyEpoch) -> bool {
        let recipient = rocky_credentials::PeerId::of(recipient);
        let carries = Carries::Secret {
            address: record_key(&record.address),
            epoch,
        };
        self.deliveries
            .values()
            .any(|pending| pending.recipient == recipient && pending.carries == carries)
    }
    pub(crate) fn queue_group(
        &mut self,
        signer: &PrivateKey,
        (record, epoch, secret): (&GroupRecord, KeyEpoch, [u8; 32]),
        recipients: &BTreeSet<PublicKey>,
        reason: Reason,
        again: bool,
    ) -> Result<usize, Failure> {
        let statements = self
            .epoch_statements
            .get(&secret_role(&record.address, epoch))
            .cloned()
            .unwrap_or_default();
        let sender = Some(self.credentials.clone());
        let mut queued = 0;
        for recipient in recipients {
            if !again && self.queued(*recipient, record, epoch) {
                continue;
            }
            let envelope = GroupSecretEnvelope::seal_recorded(
                signer,
                rocky_credentials::GroupSecrets {
                    group: record.address,
                    epoch: epoch.generation,
                    secret,
                    name: record.name,
                },
                *recipient,
                statements.clone(),
            )?;
            let delivery = Contents::seal(
                *recipient,
                reason,
                record,
                envelope.to_bytes()?,
                sender.clone(),
            )?;
            self.queue(*recipient, record, epoch, &delivery, again)?;
            queued += 1;
        }
        Ok(queued)
    }
    pub(crate) fn audience(
        &self,
        own: PublicKey,
        domain: &crate::domain::Domain,
        partition: &GroupPartition,
        (registry, membership): (&crate::registry::Registry, &crate::membership::Membership),
        excluded: &[PublicKey],
    ) -> BTreeSet<PublicKey> {
        registry
            .records
            .iter()
            .filter(|(identity, record)| {
                !excluded.contains(identity)
                    && !self.revoked_identity(identity, registry)
                    && reads_group(domain, record, partition)
            })
            .flat_map(|(identity, _)| {
                let seen = self
                    .authority
                    .iter()
                    .flat_map(|issuing| &issuing.seen)
                    .filter(move |(_, seen)| seen.identity == *identity)
                    .map(|(node, _)| *node);
                membership
                    .nodes(identity)
                    .map(|linked| linked.node)
                    .chain(seen)
            })
            .filter(|node| *node != own && !self.tombstoned(node))
            .collect()
    }
    pub(crate) fn queue_rotation(
        &mut self,
        vault: &crate::secrets::Vault,
        domain: &crate::domain::Domain,
        known: (&crate::registry::Registry, &crate::membership::Membership),
    ) -> Result<(), Failure> {
        let Some(rotation) = self
            .authority
            .as_ref()
            .and_then(|authority| authority.rotation.clone())
        else {
            return Ok(());
        };
        let signer = PrivateKey::from_bytes(&vault.get("node")?);
        let own = signer.public_key();
        let mut pending = Vec::new();
        for (address, target) in &rotation.groups {
            let Some(record) = self.groups.get(address).cloned() else {
                continue;
            };
            let secret = vault.get(&format!("rotation.partition.{address}.{target}"))?;
            let epoch = KeyEpoch::new(target.generation(), &secret);
            let partition = GroupPartition::of(domain, &record)?;
            let recipients = self.audience(own, domain, &partition, known, &rotation.identities);
            pending.push((record, epoch, secret, recipients));
        }
        for (record, epoch, secret, recipients) in pending {
            self.queue_group(
                &signer,
                (&record, epoch, secret),
                &recipients,
                Reason::Rotation,
                false,
            )?;
        }
        Ok(())
    }
    pub(crate) fn check_deliveries(&self) -> Result<(), Failure> {
        if self.deliveries.len() > MAX_OUTBOX
            || self.deliveries.values().any(|pending| {
                matches!(&pending.carries, Carries::Secret { epoch, .. } if !epoch.valid())
                    || pending.mailboxes.len() > MAX_MAILBOXES
                    || pending.targets.len() > MAX_MAILBOXES
            })
        {
            return Err("invalid pending envelopes".into());
        }
        Ok(())
    }
}

fn reads_group(
    domain: &crate::domain::Domain,
    record: &IdentityRecord,
    partition: &GroupPartition,
) -> bool {
    let whole = |grant: &rocky_core::ResourceGrant| {
        grant.rows == RowSelection::All && grant.rights.contains(&Right::Read)
    };
    let recorded = record.partition_grants(domain);
    !record.revoked
        && partition.members.iter().all(|member| {
            record
                .access()
                .is_some_and(|access| domain.reads_table(access, &member.table))
                || recorded.iter().any(|grant| {
                    grant
                        .resolve(member.scope, &member.tuple)
                        .ok()
                        .flatten()
                        .is_some_and(|grant| whole(&grant))
                })
        })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Deposit {
    pub depositor: PublicKey,
    pub recipient: PublicKey,
    pub arrived: u64,
    pub delivery: Option<String>,
    pub receipt: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MailboxFile {
    version: u32,
    deposits: BTreeMap<String, Deposit>,
}

pub(crate) struct Mailbox {
    path: PathBuf,
    deposits: BTreeMap<String, Deposit>,
}
impl Mailbox {
    pub fn open(path: &Path) -> Result<Self, Failure> {
        let deposits = if rocky_platform::fs::exists(path) {
            let file: MailboxFile = serde_json::from_slice(&rocky_platform::fs::read(path)?)?;
            if file.version != MAILBOX_VERSION {
                return Err("unsupported mailbox version; reinstall the domain".into());
            }
            if file.deposits.len() > MAILBOX_DEPOSITS
                || file
                    .deposits
                    .values()
                    .any(|deposit| deposit.delivery.is_some() == deposit.receipt.is_some())
            {
                return Err("invalid mailbox".into());
            }
            file.deposits
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            path: path.to_owned(),
            deposits,
        })
    }
    pub fn save(&self) -> Result<(), Failure> {
        write_private(
            &self.path,
            &serde_json::to_vec(&MailboxFile {
                version: MAILBOX_VERSION,
                deposits: self.deposits.clone(),
            })?,
        )?;
        Ok(())
    }
    pub fn len(&self) -> usize {
        self.deposits.len()
    }
    pub fn sweep(&mut self, time: u64, revoked: impl Fn(&PublicKey) -> bool) -> bool {
        let before = self.deposits.len();
        self.deposits.retain(|_, deposit| {
            deposit.arrived.saturating_add(DEPOSIT_LIFETIME) > time && !revoked(&deposit.recipient)
        });
        before != self.deposits.len()
    }
    pub fn deposit(
        &mut self,
        depositor: PublicKey,
        delivery: &SealedDelivery,
        time: u64,
    ) -> Result<Option<String>, Failure> {
        let digest = digest_key(&delivery.digest()?);
        if let Some(held) = self.deposits.get(&digest) {
            return Ok((held.depositor != depositor)
                .then(|| "deposit_owner_mismatch: a deposit belongs to its sender".into()));
        }
        if delivery.payload_len() > MAX_DEPOSIT_PAYLOAD {
            return Ok(Some(format!(
                "deposit_too_large: a deposit seals at most {MAX_DEPOSIT_PAYLOAD} bytes"
            )));
        }
        let recipient = delivery.recipient();
        let held = self
            .deposits
            .values()
            .filter(|deposit| deposit.recipient == recipient)
            .count();
        if held >= RECIPIENT_DEPOSITS || self.deposits.len() >= MAILBOX_DEPOSITS {
            return Ok(Some(format!(
                "{MAILBOX_FULL}: a mailbox holds {RECIPIENT_DEPOSITS} deposits per recipient and {MAILBOX_DEPOSITS} in all; retry later"
            )));
        }
        self.deposits.insert(
            digest,
            Deposit {
                depositor,
                recipient,
                arrived: time,
                delivery: Some(encode(delivery)?),
                receipt: None,
            },
        );
        Ok(None)
    }
    pub fn held_for(
        &self,
        recipient: PublicKey,
        budget: usize,
    ) -> Result<(Vec<SealedDelivery>, bool), Failure> {
        let mut held = Vec::new();
        let mut bytes = 0;
        for deposit in self.deposits.values() {
            let (true, Some(text)) = (deposit.recipient == recipient, &deposit.delivery) else {
                continue;
            };
            let delivery = decode(text)?;
            if held.len() >= 256
                || (bytes + delivery.payload_len() > budget && (!held.is_empty() || budget == 0))
            {
                return Ok((held, true));
            }
            bytes += delivery.payload_len();
            held.push(delivery);
        }
        Ok((held, false))
    }
    pub fn receipts_for(&self, depositor: PublicKey) -> Result<Vec<DeliveryReceipt>, Failure> {
        self.deposits
            .values()
            .filter(|deposit| deposit.depositor == depositor)
            .filter_map(|deposit| deposit.receipt.as_deref())
            .take(256)
            .map(|text| {
                Ok(DeliveryReceipt::from_bytes(
                    &BASE64URL_NOPAD.decode(text.as_bytes())?,
                )?)
            })
            .collect()
    }
    pub fn acknowledge(&mut self, receipt: &DeliveryReceipt) -> Result<bool, Failure> {
        let Some(deposit) = self.deposits.get_mut(&digest_key(&receipt.digest())) else {
            return Ok(false);
        };
        if deposit.delivery.is_none() {
            return Ok(false);
        }
        receipt.verify(deposit.recipient, receipt.digest())?;
        deposit.delivery = None;
        deposit.receipt = Some(BASE64URL_NOPAD.encode(&receipt.to_bytes()?));
        Ok(true)
    }
    pub fn forget(&mut self, depositor: PublicKey, digests: &[Commitment]) -> bool {
        let before = self.deposits.len();
        for digest in digests {
            let key = digest_key(digest);
            if self
                .deposits
                .get(&key)
                .is_some_and(|deposit| deposit.depositor == depositor && deposit.receipt.is_some())
            {
                self.deposits.remove(&key);
            }
        }
        before != self.deposits.len()
    }
}

pub(crate) mod fault {
    use crate::Failure;
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Stage {
        Handing,
        Renewing,
        Prepared,
        Installed,
        Receipts,
        Stored,
        Deposited,
        Replaced,
        Forgotten,
    }
    #[cfg(test)]
    pub static STOPS: std::sync::Mutex<Vec<(rocky_secure::PublicKey, Stage)>> =
        std::sync::Mutex::new(Vec::new());
    #[cfg(test)]
    pub fn check(node: rocky_secure::PublicKey, stage: Stage) -> Result<(), Failure> {
        let mut stops = STOPS.lock().map_err(|_| "poisoned fault")?;
        match stops.iter().position(|stop| *stop == (node, stage)) {
            Some(index) => {
                stops.remove(index);
                Err(format!("injected stop at {stage:?}").into())
            }
            None => Ok(()),
        }
    }
    #[cfg(not(test))]
    pub fn check(_: rocky_secure::PublicKey, _: Stage) -> Result<(), Failure> {
        Ok(())
    }
    #[cfg(test)]
    pub struct Pause {
        pub node: rocky_secure::PublicKey,
        pub stage: Stage,
        pub reached: tokio::sync::oneshot::Sender<()>,
        pub release: tokio::sync::oneshot::Receiver<()>,
    }
    #[cfg(test)]
    pub static PAUSES: std::sync::Mutex<Vec<Pause>> = std::sync::Mutex::new(Vec::new());
    #[cfg(test)]
    pub async fn pause(node: rocky_secure::PublicKey, stage: Stage) {
        let taken = {
            let mut pauses = PAUSES
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            pauses
                .iter()
                .position(|pause| pause.node == node && pause.stage == stage)
                .map(|index| pauses.remove(index))
        };
        if let Some(pause) = taken {
            let _ = pause.reached.send(());
            let _ = pause.release.await;
        }
    }
    #[cfg(not(test))]
    pub async fn pause(_: rocky_secure::PublicKey, _: Stage) {}
}
