use crate::{
    Failure,
    enrollment::Settings,
    groups::{GroupPartition, GroupRecord, record_key, secret_role},
    registry::IdentityRecord,
};
use data_encoding::{BASE64URL_NOPAD, HEXLOWER};
use rocky_core::{Commitment, KeyEpoch, PartitionValue};
use rocky_core::{Right, RowSelection};
use rocky_credentials::{DeliveryReceipt, GroupSecretEnvelope, SealedDelivery, write_private};
use rocky_secure::{PrivateKey, PublicKey};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub(crate) const MAX_MAILBOXES: usize = 3;
pub(crate) const RECIPIENT_DEPOSITS: usize = 256;
pub(crate) const MAILBOX_DEPOSITS: usize = 4096;
pub(crate) const DEPOSIT_LIFETIME: u64 = 30 * 24 * 60 * 60;
pub(crate) const MAX_DEPOSIT_PAYLOAD: usize = rocky_credentials::MAX_DELIVERY_PAYLOAD;
pub(crate) const MAX_OUTBOX: usize = 16_384;
pub const MAILBOX_FULL: &str = "mailbox_full";
const CONTENTS_VERSION: u32 = 1;
const MAILBOX_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reason {
    Share,
    Rotation,
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
    pub address: String,
    pub epoch: KeyEpoch,
    pub delivery: Option<String>,
    pub targets: Vec<rocky_credentials::PeerId>,
    pub mailboxes: Vec<rocky_credentials::PeerId>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Delivered,
    Deposited,
    Pending,
}
impl Pending {
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
        let reference = rocky_credentials::PeerId::of(recipient);
        self.delivery_nodes.insert(reference, recipient);
        let recipient = reference;
        let address = record_key(&record.address);
        let held = self.deliveries.iter().find(|(_, pending)| {
            pending.recipient == recipient && pending.address == address && pending.epoch == epoch
        });
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
                address,
                epoch,
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
    pub(crate) fn queue_rotation(
        &mut self,
        vault: &crate::secrets::Vault,
        domain: &crate::domain::Domain,
        (registry, membership): (&crate::registry::Registry, &crate::membership::Membership),
    ) -> Result<(), Failure> {
        let Some(rotation) = self
            .authority
            .as_ref()
            .and_then(|authority| authority.rotation.clone())
        else {
            return Ok(());
        };
        let own = PrivateKey::from_bytes(&vault.get("node")?).public_key();
        let candidates: Vec<(&PublicKey, &IdentityRecord)> = registry
            .records
            .iter()
            .filter(|(identity, _)| {
                !rotation.identities.contains(identity)
                    && !self.revoked_identity(identity, registry)
            })
            .flat_map(|(identity, record)| {
                membership
                    .nodes(identity)
                    .map(move |linked| (&linked.node, record))
            })
            .filter(|(node, _)| **node != own && !self.tombstoned(node))
            .collect();
        if rotation.groups.is_empty() || candidates.is_empty() {
            return Ok(());
        }
        let signer_under_issuer_chain = PrivateKey::from_bytes(&vault.get("node")?);
        let mut pending = Vec::new();
        for (address, generation) in &rotation.groups {
            let Some(record) = self.groups.get(address).cloned() else {
                continue;
            };
            let secret = vault.get(&format!("rotation.partition.{address}.{generation}"))?;
            let epoch = KeyEpoch::new(*generation, &secret);
            let partition = GroupPartition::of(domain, &record)?;
            let statements = self
                .epoch_statements
                .get(&secret_role(&record.address, epoch))
                .cloned()
                .unwrap_or_default();
            for (node, entry) in &candidates {
                if !reads_group(domain, entry, &partition) {
                    continue;
                }
                let envelope = GroupSecretEnvelope::seal_recorded(
                    &signer_under_issuer_chain,
                    rocky_credentials::GroupSecrets {
                        group: record.address,
                        epoch: epoch.generation,
                        secret,
                        name: record.name,
                    },
                    **node,
                    statements.clone(),
                )?;
                let delivery = Contents::seal(
                    **node,
                    Reason::Rotation,
                    &record,
                    envelope.to_bytes()?,
                    Some(self.credentials.clone()),
                )?;
                pending.push((**node, record.clone(), epoch, delivery));
            }
        }
        for (node, record, epoch, delivery) in pending {
            self.queue(node, &record, epoch, &delivery, false)?;
        }
        Ok(())
    }
    pub(crate) fn check_deliveries(&self) -> Result<(), Failure> {
        if self.deliveries.len() > MAX_OUTBOX
            || self.deliveries.values().any(|pending| {
                !pending.epoch.valid()
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
    let profile = record.profile().map(|profile| domain.effective(profile));
    let recorded: Vec<_> = record
        .grants
        .iter()
        .map(|grant| grant.grant.clone())
        .collect();
    let recorded = domain.effective_partitions(&recorded);
    !record.revoked
        && partition.members.iter().all(|member| {
            profile
                .as_ref()
                .is_some_and(|profile| profile.reads(&member.table))
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
}
