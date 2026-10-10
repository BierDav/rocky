//! The node as owner of envelope delivery: what it hands a peer, installs,
//! holds as a mailbox and drops on a receipt or a revocation. See
//! `docs/decisions/0054-envelope-delivery.md`.
use super::*;
use crate::{
    delivery::{
        Contents, MAX_DEPOSIT_PAYLOAD, MAX_MAILBOXES, Mailbox, Reason, digest_key,
        fault::{self, Stage},
    },
    groups::GroupPartition,
};
use rocky_core::Right;
use rocky_credentials::{DeliveryReceipt, GroupSecretEnvelope, SealedDelivery};
use rocky_secure::network::{Delivered, Deliveries, DeliveryFuture, Held, Outgoing};
use std::collections::BTreeSet;

/// The node's delivery state as a session sees it; `serving` on the side
/// that accepted the session.
pub(crate) struct Port<'a> {
    pub node: &'a Node,
    pub serving: bool,
}

fn secure(error: Failure) -> Error {
    Error::Application(error)
}

impl Deliveries for Port<'_> {
    fn outgoing<'a>(
        &'a self,
        peer: PublicKey,
        chain: &'a CredentialChain,
    ) -> DeliveryFuture<'a, Outgoing> {
        Box::pin(async move { self.node.outgoing(peer, chain).await.map_err(secure) })
    }
    fn receive(
        &self,
        peer: PublicKey,
        deliveries: Vec<SealedDelivery>,
    ) -> DeliveryFuture<'_, Vec<DeliveryReceipt>> {
        Box::pin(async move { self.node.receive(peer, deliveries).await.map_err(secure) })
    }
    fn deposit<'a>(
        &'a self,
        peer: PublicKey,
        _chain: &'a CredentialChain,
        deposits: Vec<SealedDelivery>,
    ) -> DeliveryFuture<'a, Vec<Option<String>>> {
        Box::pin(async move { self.node.deposit(peer, deposits).await.map_err(secure) })
    }
    fn deposited(
        &self,
        peer: PublicKey,
        stored: Vec<rocky_core::Commitment>,
    ) -> DeliveryFuture<'_, ()> {
        Box::pin(async move { self.node.deposited(peer, stored).await.map_err(secure) })
    }
    fn held(&self, peer: PublicKey, budget: usize) -> DeliveryFuture<'_, Held> {
        Box::pin(async move { self.node.held(peer, budget).await.map_err(secure) })
    }
    fn acknowledged(
        &self,
        peer: PublicKey,
        receipts: Vec<DeliveryReceipt>,
    ) -> DeliveryFuture<'_, ()> {
        Box::pin(async move {
            self.node
                .acknowledged(peer, receipts, self.serving)
                .await
                .map_err(secure)
        })
    }
    fn forget(
        &self,
        peer: PublicKey,
        digests: Vec<rocky_core::Commitment>,
    ) -> DeliveryFuture<'_, ()> {
        Box::pin(async move { self.node.forget(peer, digests).await.map_err(secure) })
    }
}

impl Node {
    fn own_node(&self) -> Result<PublicKey, Failure> {
        Ok(PrivateKey::from_bytes(&self.vault.get("node")?).public_key())
    }
    /// Any verified custody grant makes a peer a mailbox, independent of
    /// the partition being delivered.
    fn mailbox_chain(&self, settings: &Settings, node: PublicKey, chain: &CredentialChain) -> bool {
        let Ok(time) = now() else {
            return false;
        };
        chain
            .verify_selectors(
                settings.publisher,
                self.domain.manifest().hash(),
                node,
                &settings.revocations,
                time,
            )
            .is_ok_and(|(grants, partitions)| {
                grants
                    .iter()
                    .any(|grant| grant.rights.contains(&Right::Custody))
                    || partitions
                        .iter()
                        .any(|grant| grant.rights.contains(&Right::Custody))
            })
    }
    /// Runs `change` on the mailbox after removing what expired or what a
    /// revoked recipient was sent; saves when anything changed. The replica's
    /// set counts too: it holds what a session's handshake just brought.
    async fn with_mailbox<T>(
        &self,
        revocations: &RevocationSet,
        change: impl FnOnce(&mut Mailbox) -> Result<(T, bool), Failure>,
    ) -> Result<T, Failure> {
        let learned = self.replica.revocations().await;
        let mut slot = self.mailbox.lock().await;
        if slot.is_none() {
            *slot = Some(Mailbox::open(&self.path.with_file_name("mailbox.json"))?);
        }
        let mailbox = slot.as_mut().ok_or("mailbox not open")?;
        let swept = mailbox.sweep(now()?, |recipient| {
            revocations.revokes(recipient) || learned.revokes(recipient)
        });
        let (value, changed) = change(mailbox)?;
        if swept || changed {
            mailbox.save()?;
        }
        Ok(value)
    }
    /// Drops what this device would deliver to a revoked recipient, and a
    /// mailbox's deposits for one.
    pub(super) async fn sweep_deliveries(&self) -> Result<(), Failure> {
        let revocations = {
            let mut settings = self.settings.lock().await;
            if settings.drops_revoked_deliveries() {
                let mut draft = settings.clone();
                draft.drop_revoked_deliveries();
                draft.save(&self.path)?;
                *settings = draft;
            }
            settings.revocations.clone()
        };
        self.with_mailbox(&revocations, |_| Ok(((), false))).await
    }
    /// One delivery round with the upstream peer, on a session the
    /// exchange goes on with.
    pub(super) async fn deliver(
        &self,
        upstream: &Upstream<'_>,
    ) -> Result<rocky_secure::network::NativeSession, Failure> {
        let mut session = sync::session(&self.replica, upstream).await?;
        let report = session
            .deliver(&Port {
                node: self,
                serving: false,
            })
            .await?;
        self.record_delivery(upstream.peer, report);
        Ok(session)
    }
    pub(super) fn record_delivery(&self, peer: PublicKey, report: Delivered) {
        if report != Delivered::default() {
            self.activity.record(
                "envelopes_delivered",
                json!({
                    "peer": peer.to_hex(),
                    "handed": report.handed,
                    "deposited": report.deposited,
                    "refused": report.refused,
                    "received": report.received,
                    "receipts": report.receipts,
                }),
            );
        }
    }
    async fn outgoing(
        &self,
        peer: PublicKey,
        chain: &CredentialChain,
    ) -> Result<Outgoing, Failure> {
        let mut settings = self.settings.lock().await;
        let mut outgoing = Outgoing::default();
        if settings.revocations.revokes(&peer) {
            return Ok(outgoing);
        }
        let mailbox = self.mailbox_chain(&settings, peer, chain);
        let mut draft = settings.clone();
        let mut reserved = false;
        for pending in draft.deliveries.values_mut() {
            let Some(delivery) = pending.sealed()? else {
                continue;
            };
            if pending.recipient == rocky_credentials::PeerId::of(peer) {
                outgoing.direct.push(delivery);
            } else if mailbox
                && (pending
                    .targets
                    .contains(&rocky_credentials::PeerId::of(peer))
                    || pending.targets.len() < MAX_MAILBOXES)
                && !pending
                    .mailboxes
                    .contains(&rocky_credentials::PeerId::of(peer))
                && delivery.payload_len() <= MAX_DEPOSIT_PAYLOAD
                && !settings.revocations.revokes_reference(&pending.recipient)
            {
                if !pending
                    .targets
                    .contains(&rocky_credentials::PeerId::of(peer))
                {
                    pending.targets.push(rocky_credentials::PeerId::of(peer));
                    reserved = true;
                }
                outgoing.deposits.push(delivery);
            }
        }
        if reserved {
            draft.save(&self.path)?;
            *settings = draft;
        }
        Ok(outgoing)
    }
    async fn receive(
        &self,
        peer: PublicKey,
        deliveries: Vec<SealedDelivery>,
    ) -> Result<Vec<DeliveryReceipt>, Failure> {
        let node = PrivateKey::from_bytes(&self.vault.get("node")?);
        let mut receipts = Vec::new();
        for delivery in deliveries {
            if delivery.recipient() != node.public_key() {
                continue;
            }
            // A refused envelope gets no receipt: its sender keeps it.
            match self.accept_delivery(&node, &delivery).await {
                Ok(()) => receipts.push(DeliveryReceipt::sign(&node, delivery.digest()?)?),
                Err(error) => {
                    self.activity.record(
                        "envelope_refused",
                        json!({"peer": peer.to_hex(), "error": error.to_string()}),
                    );
                }
            }
        }
        if !receipts.is_empty() {
            fault::check(node.public_key(), Stage::Installed)?;
        }
        Ok(receipts)
    }
    /// Opens one delivery and installs its envelope as `install` did; a
    /// shared partition this device does not hold yet starts from a peer's
    /// checkpoint.
    async fn accept_delivery(
        &self,
        node: &PrivateKey,
        delivery: &SealedDelivery,
    ) -> Result<(), Failure> {
        let contents = Contents::open(delivery, node)?;
        let values = contents
            .values
            .iter()
            .map(|value| value.value())
            .collect::<Result<Vec<_>, _>>()?;
        let partition =
            GroupPartition::resolve(&self.domain, Some(&contents.group), None, values.clone())?;
        let envelope = GroupSecretEnvelope::from_bytes(&contents.envelope)?;
        if envelope.group() != partition.record.address {
            return Err("the delivery names another partition than its envelope".into());
        }
        let holding: BTreeMap<ResourceScope, rocky_secure::Holding> =
            self.replica.subscriptions().await?.into_iter().collect();
        let held = partition
            .members
            .iter()
            .all(|member| holding.get(&member.scope) == Some(&rocky_secure::Holding::Held));
        let first_epoch = self
            .settings
            .lock()
            .await
            .groups
            .get(&crate::groups::record_key(&partition.record.address))
            .is_none_or(|record| record.epochs.is_empty());
        let bootstrap = !held && (contents.reason == Reason::Share || first_epoch);
        let installed = self
            .install_envelope(
                partition.clone(),
                &envelope,
                contents.sender.as_ref(),
                bootstrap,
            )
            .await?;
        let named = json!({
            "group": contents.group,
            "values": values.into_iter().map(Cell::from).collect::<Vec<_>>(),
        });
        if installed {
            self.activity
                .record("partition_key_installed", named.clone());
        }
        if bootstrap {
            self.hold(&partition, &BTreeSet::new(), true).await?;
            self.activity.record("partition_bootstrap", named);
        }
        self.forget_readers().await;
        self.wake_sync();
        Ok(())
    }
    async fn deposit(
        &self,
        peer: PublicKey,
        deposits: Vec<SealedDelivery>,
    ) -> Result<Vec<Option<String>>, Failure> {
        let settings = self.settings.lock().await.clone();
        let own = self.own_node()?;
        if !self.mailbox_chain(&settings, own, &settings.credentials) {
            return Ok(deposits
                .iter()
                .map(|_| Some("not_a_mailbox: this peer holds no deposits".to_owned()))
                .collect());
        }
        let learned = self.replica.revocations().await;
        let time = now()?;
        let refusals = self
            .with_mailbox(&settings.revocations, |mailbox| {
                let before = mailbox.len();
                let mut refusals = Vec::new();
                for deposit in &deposits {
                    let recipient = deposit.recipient();
                    refusals.push(if recipient == own || recipient == peer {
                        Some("deposit_misaddressed: hand an envelope to its recipient".into())
                    } else if settings.revocations.revokes(&recipient)
                        || learned.revokes(&recipient)
                    {
                        Some("recipient_revoked: the recipient is revoked".into())
                    } else {
                        mailbox.deposit(peer, deposit, time)?
                    });
                }
                let changed = mailbox.len() != before;
                Ok((refusals, changed))
            })
            .await?;
        fault::check(own, Stage::Stored)?;
        Ok(refusals)
    }
    async fn deposited(
        &self,
        peer: PublicKey,
        stored: Vec<rocky_core::Commitment>,
    ) -> Result<(), Failure> {
        if stored.is_empty() {
            return Ok(());
        }
        fault::check(self.own_node()?, Stage::Deposited)?;
        let mut settings = self.settings.lock().await;
        let mut draft = settings.clone();
        for digest in stored {
            if let Some(pending) = draft.deliveries.get_mut(&digest_key(&digest))
                && pending.delivery.is_some()
                && !pending
                    .mailboxes
                    .contains(&rocky_credentials::PeerId::of(peer))
                && pending.mailboxes.len() < MAX_MAILBOXES
            {
                pending.mailboxes.push(rocky_credentials::PeerId::of(peer));
            }
        }
        draft.save(&self.path)?;
        *settings = draft;
        Ok(())
    }
    /// Its own envelopes for `peer` first, then deposits for it, in one
    /// page: a refused envelope of its own starves no deposit.
    async fn held(&self, peer: PublicKey, budget: usize) -> Result<Held, Failure> {
        let mut held = Held::default();
        let mut bytes = 0;
        let revocations = {
            let settings = self.settings.lock().await;
            if settings.revocations.revokes(&peer) {
                return Ok(held);
            }
            for pending in settings
                .deliveries
                .values()
                .filter(|pending| pending.recipient == rocky_credentials::PeerId::of(peer))
            {
                let Some(delivery) = pending.sealed()? else {
                    continue;
                };
                if held.deliveries.len() >= 256
                    || (!held.deliveries.is_empty() && bytes + delivery.payload_len() > budget)
                {
                    held.more = true;
                    return Ok(held);
                }
                bytes += delivery.payload_len();
                held.deliveries.push(delivery);
            }
            settings.revocations.clone()
        };
        self.with_mailbox(&revocations, |mailbox| {
            let (deposits, more) = mailbox.held_for(peer, budget.saturating_sub(bytes))?;
            held.more = more;
            let room = 256 - held.deliveries.len();
            held.more |= deposits.len() > room;
            for deposit in deposits.into_iter().take(room) {
                bytes += deposit.payload_len();
                held.deliveries.push(deposit);
            }
            for receipt in mailbox.receipts_for(peer)? {
                let size = receipt.to_bytes()?.len();
                if bytes + size > budget {
                    held.more = true;
                    break;
                }
                bytes += size;
                held.receipts.push(receipt);
            }
            held.more |= held.receipts.len() == 256;
            Ok((held, false))
        })
        .await
    }
    /// The dialer reports its round as a whole; the serving side reports
    /// what the receipts delivered.
    async fn acknowledged(
        &self,
        peer: PublicKey,
        receipts: Vec<DeliveryReceipt>,
        serving: bool,
    ) -> Result<(), Failure> {
        if receipts.is_empty() {
            return Ok(());
        }
        let own = self.own_node()?;
        let revocations = {
            let mut settings = self.settings.lock().await;
            let mut draft = settings.clone();
            let mut delivered = 0;
            for receipt in &receipts {
                if let Some(pending) = draft.deliveries.get_mut(&digest_key(&receipt.digest()))
                    && pending.delivery.is_some()
                    && rocky_credentials::PeerId::of(receipt.recipient()) == pending.recipient
                    && receipt
                        .verify(receipt.recipient(), receipt.digest())
                        .is_ok()
                {
                    pending.delivery = None;
                    pending.mailboxes.clear();
                    pending.targets.clear();
                    delivered += 1;
                }
            }
            if delivered > 0 {
                fault::check(own, Stage::Receipts)?;
                draft.save(&self.path)?;
                *settings = draft;
                if serving {
                    self.record_delivery(
                        peer,
                        Delivered {
                            handed: delivered,
                            ..Delivered::default()
                        },
                    );
                }
            }
            settings.revocations.clone()
        };
        // A mailbox keeps the receipt of a deposit its recipient took.
        self.with_mailbox(&revocations, |mailbox| {
            let mut changed = false;
            for receipt in receipts
                .iter()
                .filter(|receipt| receipt.recipient() == peer)
            {
                changed |= mailbox.acknowledge(receipt)?;
            }
            if changed {
                fault::check(own, Stage::Replaced)?;
            }
            Ok(((), changed))
        })
        .await
    }
    async fn forget(
        &self,
        peer: PublicKey,
        digests: Vec<rocky_core::Commitment>,
    ) -> Result<(), Failure> {
        if digests.is_empty() {
            return Ok(());
        }
        let own = self.own_node()?;
        let revocations = self.settings.lock().await.revocations.clone();
        self.with_mailbox(&revocations, |mailbox| {
            let changed = mailbox.forget(peer, &digests);
            if changed {
                fault::check(own, Stage::Forgotten)?;
            }
            Ok(((), changed))
        })
        .await
    }
}
