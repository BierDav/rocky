//! One bounded, ordered transfer of committed scopes with a terminal receipt.
use super::*;
use std::collections::BTreeSet;
mod ordering;

pub(crate) const MAX_BYTES: usize = 256 * 1024;
pub(crate) const RECEIPT_BYTES: usize = 32 * 1024;
const DECODED_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_PARTS: usize = 256;

#[derive(Serialize, Deserialize)]
pub(super) enum PublicationMessage {
    Start {
        upper: Vec<ScopeFrontier>,
        parts: usize,
        items: usize,
        digest: Commitment,
    },
    Migration {
        chain: Commitment,
        descriptor: Box<rocky_credentials::SignedMigrationPublication>,
    },
    Page {
        frames: Vec<EncryptedFrame>,
        voids: Vec<SignedVoid>,
    },
    Finish {
        parts: usize,
        items: usize,
        digest: Commitment,
    },
    Receipt(PublicationReceipt),
}
impl PublicationMessage {
    pub(super) fn part_name(&self) -> &'static str {
        match self {
            Self::Start { .. } => "start",
            Self::Migration { .. } => "migration",
            Self::Page { .. } => "page",
            Self::Finish { .. } => "finish",
            Self::Receipt(_) => "receipt",
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PublicationReceipt {
    /// Durable prefixes, clamped to the exported upper positions.
    pub frontiers: Vec<ScopeFrontier>,
    /// Authorized scopes whose final state still needs ordinary repair.
    pub repair: Vec<ResourceScope>,
    /// Unknown dependencies require the ordinary full control/holdings planner.
    pub repair_all: bool,
    pub reasons: Vec<PublicationRepairReason>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PublicationRepairReason {
    MissingHistory,
    PendingDependencies,
    MissingKey,
    MissingChain,
    Rejected,
    UnknownRequirements,
}
/// A failed preflight sends no publication data; callers retain scoped repair.
pub enum PublicationOutcome {
    Published(PublicationReceipt),
    Repair {
        scopes: Vec<ResourceScope>,
        reason: String,
    },
}

pub(super) fn sized_encoding(message: &impl Serialize) -> Result<Vec<u8>, Error> {
    static SIGNATURE: std::sync::LazyLock<Signature> =
        std::sync::LazyLock::new(|| PrivateKey::from_bytes(&[1; 32]).sign(b"publication sizing"));
    encode(&SignedMessage {
        sequence: u64::MAX,
        bytes: encode(message)?,
        signature: *SIGNATURE,
    })
}
fn signed_size(message: &Message) -> Result<usize, Error> {
    Ok(sized_encoding(message)?.len())
}
fn digest(previous: Commitment, message: &Message) -> Result<Commitment, Error> {
    Ok(rocky_core::commitment(
        "rocky.live-publication.v1",
        &encode(&(previous, encode(message)?))?,
    ))
}
fn finish(parts: usize, items: usize, digest: Commitment) -> Message {
    Message::Publication(PublicationMessage::Finish {
        parts,
        items,
        digest,
    })
}

struct Plan {
    upper: Vec<ScopeFrontier>,
    messages: Vec<Message>,
    bytes: usize,
    ids: Vec<Hash>,
}
impl Plan {
    async fn build<A: RowApplication>(
        replica: &EncryptedReplica<A>,
        peer: PublicKey,
        credentials: &CredentialChain,
        cursors: &[ScopeFrontier],
    ) -> Result<Self, Error> {
        let export = replica
            .publication_pages(peer, credentials, cursors)
            .await?;
        let upper = export.upper;
        let mut escrow = std::collections::BTreeMap::<_, Vec<_>>::new();
        for record in export.escrow {
            escrow.entry(record.slot()).or_default().push(record);
        }
        let mut messages = Vec::new();
        let mut chains = BTreeSet::new();
        let mut migration_ids = BTreeSet::new();
        let mut slots = BTreeSet::new();
        let mut items = 0;
        let mut ids = Vec::new();
        for mut page in ordering::pages(export.pages, &export.members)? {
            if page.frames.is_empty() && page.voids.is_empty() {
                continue;
            }
            let mut carried = Vec::new();
            let mut migrations = Vec::new();
            for frame in &mut page.frames {
                ids.push(frame.id()?);
                let chain = frame
                    .take_chain()
                    .ok_or_else(|| denied("publication missing chain"))?;
                if chains.insert(chain.digest()) {
                    carried.push(chain);
                }
                if let Some(migration) = &mut frame.migration {
                    let mut descriptor = migration
                        .descriptor
                        .take()
                        .ok_or_else(|| denied("publication missing migration descriptor"))?;
                    if migration_ids.insert(migration.publication) {
                        let chain = crate::wire::Chain::new(std::mem::take(
                            &mut descriptor.endorsement.chain,
                        ))?;
                        let digest = chain.digest();
                        if chains.insert(digest) {
                            carried.push(chain);
                        }
                        migrations.push(Message::Publication(PublicationMessage::Migration {
                            chain: digest,
                            descriptor,
                        }));
                    }
                }
            }
            let mut fresh = BTreeSet::new();
            for frame in &page.frames {
                let slot = (frame.scope()?, frame.generation()?);
                if slots.insert(slot) {
                    fresh.insert(slot);
                }
            }
            let records = fresh
                .into_iter()
                .flat_map(|slot| escrow.remove(&slot).unwrap_or_default())
                .collect::<Vec<_>>();
            for record in &records {
                let chain = crate::wire::Chain::new(record.chain().clone())?;
                if chains.insert(chain.digest()) {
                    carried.push(chain);
                }
            }
            if !carried.is_empty() {
                messages.push(Message::Chains(ChainMessage::Carry { chains: carried }));
            }
            messages.extend(migrations);
            if !records.is_empty() {
                let records = records
                    .iter()
                    .map(rocky_credentials::VouchedEscrow::carried)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(credential)?;
                messages.push(Message::Escrow(EscrowMessage::Carry { records }));
            }
            items += page.frames.len() + page.voids.len();
            messages.push(Message::Publication(PublicationMessage::Page {
                frames: page.frames,
                voids: page.voids,
            }));
        }
        if messages.len() > MAX_PARTS {
            return Err(denied("publication parts quota"));
        }
        let mut hash = [0; 32];
        for message in &messages {
            hash = digest(hash, message)?;
        }
        let parts = messages.len();
        messages.insert(
            0,
            Message::Publication(PublicationMessage::Start {
                upper: upper.clone(),
                parts,
                items,
                digest: hash,
            }),
        );
        messages.push(finish(parts, items, hash));
        let mut bytes = 0usize;
        let mut metadata = 0;
        let mut table = chains::ChainTable::default();
        for message in &messages {
            if let Message::Chains(ChainMessage::Carry { chains }) = message {
                for chain in chains {
                    table.received.insert(chain.digest(), chain.clone());
                }
            }
            let size = signed_size(message)?;
            if size > MAX_SECURE_FRAME {
                return Err(denied("publication item quota"));
            }
            bytes = bytes
                .checked_add(size)
                .ok_or_else(|| denied("publication byte quota"))?;
            charge(&sized_encoding(message)?, &mut metadata)?;
            charge(&encode(message)?, &mut metadata)?;
            charge_metadata(message, &mut metadata)?;
            charge_references(message, &table, &mut metadata)?;
            if let Message::Publication(PublicationMessage::Page { frames, .. }) = message {
                replica
                    .charge_publication_frames(frames, &mut metadata)
                    .await?;
            }
        }
        // Include the worst possible repair set and every exported log in response preflight.
        let mut frontiers = upper.clone();
        for frontier in &mut frontiers {
            for log in &mut frontier.logs {
                log.applied = log.held;
            }
        }
        let largest_scope = ResourceScope {
            domain: rocky_core::Scope {
                mesh: rocky_core::MeshId([255; 16]),
                app: rocky_core::AppId([255; 16]),
                domain: rocky_core::DomainId([255; 16]),
            },
            table: [255; 32],
            encryption: [255; 32],
        };
        let receipt = Message::Publication(PublicationMessage::Receipt(PublicationReceipt {
            repair: vec![largest_scope; link::SIGNAL_SCOPES],
            frontiers,
            repair_all: true,
            reasons: vec![PublicationRepairReason::UnknownRequirements; 6],
        }));
        if bytes > MAX_BYTES || signed_size(&receipt)? > RECEIPT_BYTES {
            return Err(denied("publication byte/receipt quota"));
        }
        Ok(Self {
            upper,
            messages,
            bytes,
            ids,
        })
    }
}

impl link::channels::Handle {
    pub async fn publish_committed<A: RowApplication>(
        &self,
        replica: &EncryptedReplica<A>,
        cursors: &[ScopeFrontier],
    ) -> Result<PublicationOutcome, Error> {
        let (peer, credentials, proved) = self.publication_peer();
        if cursors.iter().any(|c| !proved.contains(&c.scope)) {
            return Err(denied("unproved publication scope"));
        }
        let plan = match Plan::build(replica, peer, credentials, cursors).await {
            Ok(plan) => plan,
            Err(error @ (Error::Rejected(_) | Error::Quota { .. })) => {
                tracing::debug!(%error, "live publication requires scoped repair");
                return Ok(PublicationOutcome::Repair {
                    scopes: cursors.iter().map(|c| c.scope).collect(),
                    reason: error.to_string(),
                });
            }
            Err(error) => return Err(error),
        };
        if plan.upper.is_empty() {
            return Ok(PublicationOutcome::Published(PublicationReceipt::default()));
        }
        replica.offer_publication(&plan.ids).await?;
        let mut session = self.open_publication(plan.bytes).await?;
        let deadline = session
            .publication_deadline()
            .ok_or_else(|| denied("publication channel"))?;
        let result = rocky_platform::time::timeout_at(deadline, async {
            // Authorization may have changed during preflight or channel reservation.
            for fence in &plan.upper {
                replica
                    .authorize_scope_peer(peer, credentials, fence.scope)
                    .await?;
            }
            for message in plan.messages {
                session.send(message).await?;
            }
            let Message::Publication(PublicationMessage::Receipt(receipt)) =
                session.receive().await?
            else {
                return Err(denied("unexpected publication receipt"));
            };
            if receipt.frontiers.len() != plan.upper.len()
                || receipt.repair.len() > link::SIGNAL_SCOPES
                || receipt.reasons.len() > 6
            {
                return Err(denied("publication receipt quota"));
            }
            if let SessionIo::Channel(io) = &mut session.stream {
                io.finish_publication()?;
            }
            for (received, fence) in receipt.frontiers.iter().zip(&plan.upper) {
                received.validate()?;
                if received.scope != fence.scope
                    || received.logs.len() != fence.logs.len()
                    || received
                        .logs
                        .iter()
                        .zip(&fence.logs)
                        .any(|(a, b)| a.author != b.author || a.held > b.held)
                {
                    return Err(denied("publication receipt outside export"));
                }
            }
            for scope in &receipt.repair {
                replica
                    .authorize_scope_peer(peer, credentials, *scope)
                    .await?;
            }
            for frontier in &receipt.frontiers {
                replica
                    .acknowledge_frontier(peer, credentials, frontier)
                    .await?;
            }
            session.close().await?;
            Ok(PublicationOutcome::Published(receipt))
        })
        .await;
        result.map_err(|_| link::timed_out("publication lifetime timeout"))?
    }
}

impl NativeSession {
    pub fn is_publication(&self) -> bool {
        self.publication_deadline().is_some()
    }
    fn publication_deadline(&self) -> Option<rocky_platform::time::Instant> {
        match &self.stream {
            SessionIo::Channel(io) => io.publication.map(|(_, deadline)| deadline),
            _ => None,
        }
    }
    pub async fn receive_publication<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
    ) -> Result<PublicationReceipt, Error> {
        let deadline = self
            .publication_deadline()
            .ok_or_else(|| denied("publication channel required"))?;
        let SessionIo::Channel(io) = &self.stream else {
            return Err(denied("publication channel required"));
        };
        tracing::debug!(parent: &self.span, queue_us = io.opened.elapsed().as_micros().min(u128::from(u64::MAX)) as u64, "publication_intake");
        let progress = io.progress();
        #[cfg(any(test, feature = "qualification"))]
        io.record_intake();
        let result = rocky_platform::time::timeout_at(deadline, async {
            tokio::select! {
                result = self.intake_publication(replica) => result,
                result = progress.run() => { result?; Err(denied("publication progress ended")) }
            }
        })
        .await;
        match result {
            Ok(result) => result,
            Err(_) => {
                if let SessionIo::Channel(io) = &mut self.stream {
                    io.retire();
                }
                Err(link::timed_out("publication lifetime timeout"))
            }
        }
    }
    async fn intake_publication<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
    ) -> Result<PublicationReceipt, Error> {
        let Message::Publication(PublicationMessage::Start {
            upper,
            parts,
            items,
            digest: expected,
        }) = self.receive().await?
        else {
            return Err(denied("publication start required"));
        };
        crate::replica::roles::validate_frontiers(&upper)?;
        if parts > MAX_PARTS || items > MAX_PARTS * rows::MAX_SECURE_PAGE {
            return Err(denied("publication declaration quota"));
        }
        for frontier in &upper {
            frontier.validate()?;
            if !self.offered.contains(&frontier.scope) {
                return Err(denied("unproved publication scope"));
            }
            self.require_holding(replica, frontier.scope).await?;
            replica
                .authorize_scope_peer(self.peer, &self.credentials, frontier.scope)
                .await?;
        }
        let mut hash = [0; 32];
        let mut count = 0;
        let mut received = Vec::new();
        let mut identities = BTreeSet::new();
        let mut positions = BTreeSet::new();
        let mut repair = BTreeSet::new();
        let mut repair_all = false;
        let mut reasons = BTreeSet::new();
        let mut chain_ids = BTreeSet::new();
        let mut escrow_ids = BTreeSet::new();
        let mut migration_ids = BTreeSet::new();
        for _ in 0..parts {
            let message = self.receive().await?;
            hash = digest(hash, &message)?;
            match message {
                Message::Publication(PublicationMessage::Migration {
                    chain,
                    mut descriptor,
                }) => {
                    if !descriptor.endorsement.chain.0.is_empty()
                        || !migration_ids.insert(descriptor.statement.publication)
                    {
                        return Err(denied("publication duplicate/inline migration metadata"));
                    }
                    let scope = upper
                        .iter()
                        .find(|f| f.scope.domain == descriptor.statement.scope)
                        .ok_or_else(|| denied("migration outside publication domain"))?
                        .scope;
                    replica
                        .authorize_scope_peer(self.peer, &self.credentials, scope)
                        .await?;
                    descriptor.endorsement.chain = self
                        .chains
                        .received
                        .get(&chain)
                        .ok_or_else(|| denied("incomplete migration chain"))?
                        .credentials()
                        .clone();
                    replica.admit_migration_publication(*descriptor).await?;
                }
                Message::Chains(ChainMessage::Carry { chains }) => {
                    if chains.is_empty() || chains.iter().any(|c| !chain_ids.insert(c.digest())) {
                        return Err(denied("publication duplicate chains"));
                    }
                    self.learn_chains(ChainMessage::Carry { chains });
                }
                Message::Escrow(EscrowMessage::Carry { records }) => {
                    if records.is_empty() {
                        return Err(denied("publication empty escrow"));
                    }
                    for record in &records {
                        let id = rocky_core::commitment("rocky.live-escrow.v1", &encode(record)?);
                        if !escrow_ids.insert(id) {
                            return Err(denied("publication duplicate escrow"));
                        }
                    }
                    self.learn_escrow(EscrowMessage::Carry { records })?;
                }
                Message::Publication(PublicationMessage::Page { frames, voids }) => {
                    let mut scopes = BTreeSet::new();
                    for frame in &frames {
                        scopes.insert(frame.scope()?);
                    }
                    for record in &voids {
                        scopes.insert(record.scope()?);
                    }
                    for scope in &scopes {
                        if !upper.iter().any(|f| f.scope == *scope) {
                            return Err(denied("publication scope outside declaration"));
                        }
                        self.require_holding(replica, *scope).await?;
                        replica
                            .authorize_scope_peer(self.peer, &self.credentials, *scope)
                            .await?;
                    }
                    let weight = frames
                        .iter()
                        .fold(0usize, |n, f| n.saturating_add(f.weight()));
                    let weight = voids
                        .iter()
                        .fold(weight, |n, v| n.saturating_add(v.weight()));
                    if weight > MAX_PAGE_BYTES {
                        return Err(denied("publication range quota"));
                    }
                    let n = frames.len() + voids.len();
                    if n == 0 || n > rows::MAX_SECURE_PAGE {
                        return Err(denied("publication page quota"));
                    }
                    count += n;
                    if count > items {
                        return Err(denied("publication item count"));
                    }
                    for frame in &frames {
                        if frame
                            .migration
                            .as_ref()
                            .is_some_and(|m| m.descriptor.is_some())
                        {
                            return Err(denied("inline publication migration descriptor"));
                        }
                        let h = &frame.operation_ref()?.header;
                        let scope = h.extensions.scope;
                        let fence = upper
                            .iter()
                            .find(|f| f.scope == scope)
                            .ok_or_else(|| denied("publication scope outside declaration"))?;
                        if fence.log(h.public_key).is_none_or(|l| h.seq_num >= l.held)
                            || !identities.insert(frame.id()?)
                            || !positions.insert((scope, h.public_key, h.seq_num))
                        {
                            return Err(denied("publication frame outside declaration/duplicate"));
                        }
                    }
                    for record in &voids {
                        for (id, h, _) in record.verify()? {
                            let scope = h.extensions.scope;
                            let fence = upper
                                .iter()
                                .find(|f| f.scope == scope)
                                .ok_or_else(|| denied("publication scope outside declaration"))?;
                            if fence.log(h.public_key).is_none_or(|l| h.seq_num >= l.held)
                                || !identities.insert(id)
                                || !positions.insert((scope, h.public_key, h.seq_num))
                            {
                                return Err(denied("publication void outside declaration"));
                            }
                        }
                    }
                    rows::receive_voids(
                        replica,
                        &voids,
                        &scopes.iter().copied().collect::<Vec<_>>(),
                    )
                    .await?;
                    if let SessionIo::Channel(io) = &mut self.stream {
                        replica
                            .charge_publication_frames(&frames, &mut io.decoded)
                            .await?;
                    }
                    let (frames, missing) = self.complete_frames(frames)?;
                    if missing != 0 {
                        repair.extend(scopes);
                        repair_all = true;
                        reasons.insert(PublicationRepairReason::MissingChain);
                    }
                    self.keep_escrow(replica, &frames).await?;
                    for frame in &frames {
                        received.push((frame.scope()?, frame.id()?));
                    }
                    for status in replica.receive_migration_all(frames).await {
                        status?;
                    }
                }
                _ => return Err(denied("unexpected publication part")),
            }
        }
        match self.receive().await? {
            Message::Publication(PublicationMessage::Finish {
                parts: n,
                items: m,
                digest,
            }) if n == parts
                && m == items
                && count == items
                && digest == expected
                && hash == expected => {}
            _ => return Err(denied("publication incomplete finish")),
        }
        if let SessionIo::Channel(io) = &mut self.stream {
            io.finish_publication()?;
        }
        for frontier in &upper {
            self.require_holding(replica, frontier.scope).await?;
        }
        let mut state = replica.publication_state(&upper, &received).await?;
        repair.extend(state.repair);
        repair_all |= state.repair_all;
        reasons.extend(state.reasons);
        let repair = replica
            .peer_holds(
                self.peer,
                &self.credentials,
                &repair.into_iter().collect::<Vec<_>>(),
            )
            .await?;
        if repair.len() > link::SIGNAL_SCOPES {
            repair_all = true;
            reasons.insert(PublicationRepairReason::UnknownRequirements);
        }
        state.repair = repair.into_iter().take(link::SIGNAL_SCOPES).collect();
        state.repair_all = repair_all;
        state.reasons = reasons.into_iter().collect();
        let receipt = state;
        let message = Message::Publication(PublicationMessage::Receipt(receipt.clone()));
        if signed_size(&message)? > RECEIPT_BYTES {
            return Err(denied("publication receipt quota"));
        }
        replica.flush().await?;
        for frontier in &upper {
            replica
                .authorize_scope_peer(self.peer, &self.credentials, frontier.scope)
                .await?;
        }
        self.send(message).await?;
        Ok(receipt)
    }
}

fn charge_chain(chain: &CredentialChain, used: &mut usize) -> Result<(), Error> {
    for certificate in &chain.0 {
        charge(certificate.capability.extension.as_bytes(), used)?;
        if let Some(node) = certificate.attestation().certificate() {
            charge(node.capability.extension.as_bytes(), used)?;
        }
    }
    Ok(())
}
pub(super) fn charge_references(
    message: &Message,
    table: &chains::ChainTable,
    used: &mut usize,
) -> Result<(), Error> {
    if let Message::Publication(PublicationMessage::Migration { chain, .. }) = message {
        let chain = table
            .received
            .get(chain)
            .ok_or_else(|| denied("incomplete migration chain"))?;
        charge(&encode(chain)?, used)?;
        charge_chain(chain.credentials(), used)?;
    }
    if let Message::Escrow(EscrowMessage::Carry { records }) = message {
        for record in records {
            let chain = table
                .received
                .get(&record.chain_digest())
                .ok_or_else(|| denied("incomplete escrow chain"))?;
            // Reconstructing a vouch clones its original credentials before verified storage.
            charge(&encode(chain)?, used)?;
            charge_chain(chain.credentials(), used)?;
        }
    }
    Ok(())
}

pub(super) fn charge_metadata(message: &Message, used: &mut usize) -> Result<(), Error> {
    match message {
        Message::Publication(PublicationMessage::Migration { descriptor, .. }) => {
            if let Some(node) = descriptor.endorsement.attestation().certificate() {
                charge(node.capability.extension.as_bytes(), used)?;
            }
        }
        Message::Chains(ChainMessage::Carry { chains }) => {
            for carried in chains {
                charge_chain(carried.credentials(), used)?;
            }
        }
        Message::Publication(PublicationMessage::Page { frames, voids, .. }) => {
            for frame in frames {
                charge(frame.header_bytes(), used)?;
                if let Some(descriptor) =
                    frame.migration.as_ref().and_then(|m| m.descriptor.as_ref())
                {
                    charge_chain(&descriptor.endorsement.chain, used)?;
                    if let Some(node) = descriptor.endorsement.attestation().certificate() {
                        charge(node.capability.extension.as_bytes(), used)?;
                    }
                }
            }
            for record in voids {
                for header in record.header_bytes() {
                    charge(header, used)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

// Scan definite CBOR before serde can allocate collections from hostile lengths.
// Byte strings are opaque; their length is charged, never interpreted as metadata.
pub(crate) fn charge(bytes: &[u8], used: &mut usize) -> Result<(), Error> {
    let mut cursor = 0;
    *used = used
        .checked_add(bytes.len())
        .ok_or_else(|| denied("publication decoded quota"))?;
    fn item(bytes: &[u8], at: &mut usize, depth: usize, used: &mut usize) -> Result<(), Error> {
        if depth > 32 {
            return Err(denied("publication metadata depth quota"));
        }
        *used = used
            .checked_add(128)
            .filter(|n| *n <= DECODED_BYTES)
            .ok_or_else(|| denied("publication decoded quota"))?;
        let head = *bytes
            .get(*at)
            .ok_or_else(|| denied("invalid publication cbor"))?;
        *at += 1;
        let info = head & 31;
        let width = match info {
            0..=23 => 0,
            24 => 1,
            25 => 2,
            26 => 4,
            27 => 8,
            _ => return Err(denied("indefinite publication cbor")),
        };
        let mut n = if width == 0 { u64::from(info) } else { 0 };
        for _ in 0..width {
            n = (n << 8)
                | u64::from(
                    *bytes
                        .get(*at)
                        .ok_or_else(|| denied("invalid publication cbor"))?,
                );
            *at += 1;
        }
        match head >> 5 {
            0 | 1 | 7 => {}
            2 | 3 => {
                let n = usize::try_from(n).map_err(|_| denied("publication metadata length"))?;
                *at = at
                    .checked_add(n)
                    .filter(|end| *end <= bytes.len())
                    .ok_or_else(|| denied("publication metadata length"))?;
            }
            4 | 5 => {
                let n = n
                    .checked_mul(if head >> 5 == 5 { 2 } else { 1 })
                    .filter(|n| *n <= (bytes.len() - *at) as u64)
                    .ok_or_else(|| denied("publication metadata count"))?;
                for _ in 0..n {
                    item(bytes, at, depth + 1, used)?;
                }
            }
            6 => item(bytes, at, depth + 1, used)?,
            _ => return Err(denied("invalid publication cbor")),
        }
        Ok(())
    }
    item(bytes, &mut cursor, 0, used)?;
    if cursor != bytes.len() || *used > DECODED_BYTES {
        return Err(denied("publication metadata quota"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
