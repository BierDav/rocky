use super::*;
use p2panda_core::Signature;
use rocky_credentials::{MetadataEscrow, PeerId};

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_partition_custody(scope BLOB PRIMARY KEY,policy BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_custody_receipts(scope BLOB NOT NULL,peer BLOB NOT NULL,receipt BLOB NOT NULL,PRIMARY KEY(scope,peer)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_custody_metadata(id INTEGER PRIMARY KEY CHECK(id=1),record BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_custody_challenges(scope BLOB NOT NULL,peer BLOB NOT NULL,binding BLOB NOT NULL,challenge BLOB NOT NULL,PRIMARY KEY(scope,peer)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_custody_bootstrap(id INTEGER PRIMARY KEY CHECK(id=1),manifest BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_eviction_custody(scope BLOB PRIMARY KEY,unconfirmed INTEGER NOT NULL)",
];

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PartitionCustody {
    pub peers: BTreeSet<PeerId>,
    pub copies: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CustodyReceipt {
    body: Coverage,
    signature: Signature,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Coverage {
    version: u8,
    descriptor: Commitment,
    scope: ResourceScope,
    tip: TipId,
    signer: PublicKey,
    audience: PublicKey,
    binding: Commitment,
    challenge: Commitment,
    chain: CredentialChain,
    logs: Vec<(PublicKey, u64, Hash)>,
    epochs: BTreeMap<rocky_core::KeyEpoch, Commitment>,
    naming: Option<Commitment>,
    checkpoint: Option<SignedCheckpoint>,
}

impl<A: RowApplication> Inner<A> {
    pub(super) async fn partition_custody(
        &self,
        scope: ResourceScope,
    ) -> Result<PartitionCustody, Error> {
        let mut result = self
            .conn
            .query(
                "SELECT policy FROM secure_partition_custody WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        match result.next().await? {
            Some(row) => decode(&row.get::<Vec<u8>>(0)?),
            None => Ok(PartitionCustody::default()),
        }
    }
    pub(super) async fn custody_peers(&self) -> Result<BTreeSet<PeerId>, Error> {
        let mut peers = BTreeSet::new();
        for row in rows(&self.conn, "SELECT policy FROM secure_partition_custody").await? {
            let [SqlValue::Blob(bytes)] = row.as_slice() else {
                return Err(Error::Corrupt("partition custody".into()));
            };
            peers.extend(decode::<PartitionCustody>(bytes)?.peers);
        }
        Ok(peers)
    }
    pub(super) fn judge_custody_metadata(&self, record: &MetadataEscrow) -> Result<(), Error> {
        let (_, kind) = self
            .config
            .manifest
            .verify_kind(self.config.publisher)
            .map_err(credential)?;
        let DatabaseKind::Personal { owner, .. } = kind else {
            return Err(denied("metadata escrow requires personal genesis"));
        };
        let descriptor = self.config.manifest.descriptor().map_err(credential)?;
        record
            .verify(
                self.config
                    .credentials
                    .database_context(self.config.publisher)
                    .map_err(credential)?,
                descriptor,
            )
            .map_err(credential)?;
        record
            .chain()
            .provenance_with_families(
                self.config.publisher,
                record.signer(),
                self.grant_families(),
            )
            .map_err(credential)?;
        let link = record
            .chain()
            .certificate()
            .map(rocky_credentials::PersonalLink::of)
            .transpose()
            .map_err(credential)?
            .flatten()
            .ok_or_else(|| denied("metadata escrow needs an owner device"))?;
        if link.owner != *owner
            || link.descriptor != descriptor
            || self.config.revocations.tombstones(*owner, record.signer())
            || self
                .config
                .revocations
                .unfenced()
                .ends_node_of(*owner, record.signer(), false)
            || record.signed_at() > now()?.saturating_add(MAX_CLOCK_AHEAD.as_secs())
        {
            return Err(denied("metadata escrow owner is not authorized"));
        }
        Ok(())
    }
    pub(super) async fn custody_metadata(&self) -> Result<Option<MetadataEscrow>, Error> {
        let mut result = self
            .conn
            .query("SELECT record FROM secure_custody_metadata WHERE id=1", ())
            .await?;
        result
            .next()
            .await?
            .map(|row| decode(&row.get::<Vec<u8>>(0)?))
            .transpose()
    }
    pub(super) async fn store_custody_metadata(
        &self,
        record: &MetadataEscrow,
    ) -> Result<(), Error> {
        self.judge_custody_metadata(record)?;
        if let Some(held) = self.custody_metadata().await? {
            if held.record().tag() != record.record().tag() {
                return Err(denied("conflicting naming escrow"));
            }
            if self.judge_custody_metadata(&held).is_ok() {
                return Ok(());
            }
        }
        self.conn.execute("INSERT INTO secure_custody_metadata VALUES(1,?) ON CONFLICT(id) DO UPDATE SET record=excluded.record", [encode(record)?]).await?;
        Ok(())
    }
    async fn naming_tag(&self) -> Result<Option<Commitment>, Error> {
        if !self
            .config
            .manifest
            .verify_kind(self.config.publisher)
            .map_err(credential)?
            .1
            .is_personal()
        {
            return Ok(None);
        }
        let record = self
            .custody_metadata()
            .await?
            .ok_or_else(|| denied("custody requires naming escrow"))?;
        self.judge_custody_metadata(&record)?;
        Ok(Some(record.record().tag()))
    }
    pub(super) async fn durable_custodians(
        &self,
        scope: ResourceScope,
    ) -> Result<Vec<CustodyReceipt>, Error> {
        let policy = self.partition_custody(scope).await?;
        let mut receipts = Vec::new();
        for peer in policy.peers {
            let mut result = self
                .conn
                .query(
                    "SELECT receipt FROM secure_custody_receipts WHERE scope=? AND peer=?",
                    [encode(&scope)?, peer.as_bytes().to_vec()],
                )
                .await?;
            if let Some(row) = result.next().await? {
                let receipt: CustodyReceipt = decode(&row.get::<Vec<u8>>(0)?)?;
                if receipt.body.scope == scope
                    && PeerId::of(receipt.body.signer) == peer
                    && receipt.body.tip == self.workspace.tip
                    && self.check_receipt(&receipt).is_ok()
                {
                    receipts.push(receipt);
                }
            }
        }
        Ok(receipts)
    }
    fn check_receipt(&self, receipt: &CustodyReceipt) -> Result<(), Error> {
        self.check_receipt_at(receipt, now()?)
    }
    pub(crate) fn check_receipt_at(
        &self,
        receipt: &CustodyReceipt,
        time: u64,
    ) -> Result<(), Error> {
        let body = &receipt.body;
        if body.version != 1
            || body.descriptor != self.config.manifest.descriptor().map_err(credential)?
            || body.audience != self.config.node.public_key()
            || body.signer == body.audience
            || body.logs.len() > MAX_CHECKPOINT_LOGS
            || body
                .logs
                .iter()
                .any(|(_, held, _)| *held == 0 || *held > i64::MAX as u64)
            || body.logs.windows(2).any(|pair| pair[0].0 >= pair[1].0)
            || !body.signer.verify(
                &signing_bytes("rocky.custody-receipt.v1", body)?,
                &receipt.signature,
            )
        {
            return Err(denied("invalid custody receipt"));
        }
        let grants = self.verify_chain_scopes(&body.chain, body.signer, time, &[body.scope])?;
        if !roles::holds_whole(&grants, body.scope) {
            return Err(denied("custody receipt grant expired or revoked"));
        }
        if self
            .config
            .manifest
            .verify_kind(self.config.publisher)
            .map_err(credential)?
            .1
            .is_personal()
            && body.naming.is_none()
        {
            return Err(denied("custody receipt lacks naming escrow"));
        }
        if let Some(signed) = &body.checkpoint {
            if signed.statement.scope != body.scope
                || !body.epochs.contains_key(
                    &signed
                        .statement
                        .key_epoch()
                        .ok_or_else(|| denied("custody checkpoint epoch"))?,
                )
            {
                return Err(denied("custody checkpoint outside partition or escrow"));
            }
            signed.verify_authority(self, time)?;
        }
        Ok(())
    }
    pub(super) async fn custody_count(
        &self,
        scope: ResourceScope,
        author: PublicKey,
        sequence: u64,
    ) -> Result<usize, Error> {
        Ok(self
            .durable_custodians(scope)
            .await?
            .iter()
            .filter(|receipt| receipt.covers(author, sequence))
            .count())
    }
    pub(crate) async fn custody_coverage(&self, scope: ResourceScope) -> Result<usize, Error> {
        let frontier = self.frontier(scope).await?;
        let mut required: BTreeMap<_, _> = frontier
            .logs
            .iter()
            .filter(|log| log.held > 0)
            .map(|log| (log.author, log.held - 1))
            .collect();
        for row in collect_rows(self.conn.query("SELECT author,MAX(sequence) FROM secure_operations WHERE scope=? GROUP BY author", [encode(&scope)?]).await?).await? {
            let [SqlValue::Blob(author), SqlValue::Integer(sequence)] = row.as_slice() else { return Err(Error::Corrupt("custody coverage".into())); };
            let author = checkpoint::key_of(author)?;
            required.entry(author).and_modify(|held| *held = (*held).max(*sequence as u64)).or_insert(*sequence as u64);
        }
        if let Some(signed) = self.archived_checkpoint(scope, true).await? {
            for log in signed.statement.frontier {
                required
                    .entry(log.author)
                    .and_modify(|held| *held = (*held).max(log.sequence))
                    .or_insert(log.sequence);
            }
        }
        Ok(self
            .durable_custodians(scope)
            .await?
            .iter()
            .filter(|receipt| {
                required
                    .iter()
                    .all(|(author, sequence)| receipt.covers(*author, *sequence))
            })
            .count())
    }
}
impl CustodyReceipt {
    #[cfg(test)]
    pub(crate) fn forged(&self) -> Self {
        let mut receipt = self.clone();
        receipt.signature = PrivateKey::new().sign(b"forged receipt");
        receipt
    }
    fn covers(&self, author: PublicKey, sequence: u64) -> bool {
        self.body
            .logs
            .iter()
            .any(|(key, through, _)| *key == author && *through > sequence)
    }
    pub(super) fn held(&self, author: PublicKey) -> u64 {
        self.body
            .logs
            .iter()
            .find(|(key, ..)| *key == author)
            .map_or(0, |(_, held, _)| *held)
    }
    pub(super) fn peer(&self) -> PeerId {
        PeerId::of(self.body.signer)
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn custody_escrow_slots(
        &self,
        scope: ResourceScope,
    ) -> Result<BTreeSet<(ResourceScope, rocky_core::KeyEpoch)>, Error> {
        let inner = self.inner.lock(Class::Background).await;
        let mut result = inner
            .conn
            .query(
                "SELECT DISTINCT generation FROM secure_escrow WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        let mut slots = BTreeSet::new();
        while let Some(row) = result.next().await? {
            slots.insert((scope, decode(&row.get::<Vec<u8>>(0)?)?));
        }
        Ok(slots)
    }
    pub(crate) async fn begin_custody_challenge(
        &self,
        peer: PublicKey,
        scope: ResourceScope,
        binding: Commitment,
        challenge: Commitment,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock(Class::Background).await;
        transaction!(@keeping_frontier_cache inner, |inner| {
            inner.conn.execute("INSERT INTO secure_custody_challenges VALUES(?,?,?,?) ON CONFLICT(scope,peer) DO UPDATE SET binding=excluded.binding,challenge=excluded.challenge", [encode(&scope)?, PeerId::of(peer).as_bytes().to_vec(), binding.to_vec(), challenge.to_vec()]).await?;
            Ok(())
        })
    }
    pub async fn custody_naming_escrow(&self) -> Result<Option<MetadataEscrow>, Error> {
        let inner = self.inner.lock(Class::Background).await;
        Ok(inner
            .custody_metadata()
            .await?
            .filter(|record| inner.judge_custody_metadata(record).is_ok()))
    }
    pub(crate) async fn forget_custody_receipt(
        &self,
        peer: PublicKey,
        scope: ResourceScope,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock(Class::Background).await;
        transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "DELETE FROM secure_custody_receipts WHERE scope=? AND peer=?",
                    [encode(&scope)?, PeerId::of(peer).as_bytes().to_vec()],
                )
                .await?;
            Ok(())
        })
    }
    pub async fn set_partition_custody(
        &self,
        scope: ResourceScope,
        policy: PartitionCustody,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock(Class::Background).await;
        if policy.peers.len() > 64
            || policy.copies > policy.peers.len()
            || policy
                .peers
                .contains(&PeerId::of(inner.config.node.public_key()))
        {
            return Err(denied("invalid partition custody policy"));
        }
        inner.graph_for(scope.domain)?;
        transaction!(inner, |inner| {
            inner.conn.execute("INSERT INTO secure_partition_custody VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET policy=excluded.policy", [encode(&scope)?, encode(&policy)?]).await?;
            inner
                .conn
                .execute(
                    "DELETE FROM secure_custody_receipts WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?;
            inner
                .conn
                .execute(
                    "DELETE FROM secure_custody_challenges WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?;
            Ok(())
        })?;
        drop(flush_guard(inner, &self.inner).await?);
        Ok(())
    }
    pub async fn partition_custody(&self, scope: ResourceScope) -> Result<PartitionCustody, Error> {
        self.inner
            .lock(Class::Background)
            .await
            .partition_custody(scope)
            .await
    }
    pub async fn keep_custody_metadata(&self, record: MetadataEscrow) -> Result<(), Error> {
        let mut inner = self.inner.lock(Class::Background).await;
        inner.judge_custody_metadata(&record)?;
        transaction!(inner, |inner| {
            inner.store_custody_metadata(&record).await
        })?;
        drop(flush_guard(inner, &self.inner).await?);
        Ok(())
    }
    pub(crate) async fn custody_receipt(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
        binding: Commitment,
        challenge: Commitment,
    ) -> Result<Option<CustodyReceipt>, Error> {
        let inner = self.inner.lock(Class::Background).await;
        let inner = flush_guard(inner, &self.inner).await?;
        let grants = inner.verify_chain_scopes(credentials, peer, now()?, &[scope])?;
        if !roles::holds_whole(&grants, scope)
            || !inner.holds(scope).await?
            || inner.importing(scope).await?
        {
            return Ok(None);
        }
        let local = inner.local_grants(now()?)?;
        if !roles::holds_whole(&local, scope) {
            return Ok(None);
        }
        let time = now()?;
        let mut bootstrap = inner
            .conn
            .query(
                "SELECT manifest FROM secure_custody_bootstrap WHERE id=1",
                (),
            )
            .await?;
        let Some(row) = bootstrap.next().await? else {
            return Ok(None);
        };
        let manifest = SignedManifest::from_bytes(&row.get::<Vec<u8>>(0)?).map_err(credential)?;
        manifest
            .verify(inner.config.publisher)
            .map_err(credential)?;
        if manifest.descriptor().map_err(credential)?
            != inner.config.manifest.descriptor().map_err(credential)?
        {
            return Ok(None);
        }
        drop(bootstrap);
        if inner.check_archive_quota(0).await.is_err() {
            return Ok(None);
        }
        let naming = match inner.naming_tag().await {
            Ok(tag) => tag,
            Err(_) => return Ok(None),
        };
        let mut epochs = BTreeMap::new();
        let mut logs: BTreeMap<PublicKey, (u64, Hash)> = BTreeMap::new();
        let checkpoint = inner.archived_checkpoint(scope, true).await?;
        if let Some(signed) = &checkpoint {
            signed.verify_authority(&inner, time)?;
            if let Some(epoch) = signed.statement.key_epoch() {
                let records = inner.escrow_of(&[(scope, epoch)].into(), None).await?;
                let Some(record) = records
                    .iter()
                    .find(|record| inner.judge_escrow(record, time).is_ok())
                else {
                    return Ok(None);
                };
                epochs.insert(epoch, record.record().tag());
            }
            for log in &signed.statement.frontier {
                let held = log
                    .sequence
                    .checked_add(1)
                    .filter(|held| *held <= i64::MAX as u64)
                    .ok_or_else(|| denied("custody checkpoint frontier sequence overflow"))?;
                logs.insert(log.author, (held, log.hash));
            }
        }
        let base = logs.clone();
        let mut gaps = BTreeSet::new();
        let mut query = inner
            .conn
            .query(
                "SELECT header FROM secure_operations WHERE scope=? ORDER BY author,sequence",
                [encode(&scope)?],
            )
            .await?;
        while let Some(row) = query.next().await? {
            let header: p2panda_core::Header<Extensions> = decode(&row.get::<Vec<u8>>(0)?)?;
            let author = header.public_key;
            if base
                .get(&author)
                .is_some_and(|(held, _)| header.seq_num < *held)
            {
                continue;
            }
            let previous = logs.get(&author);
            if header.seq_num != previous.map_or(0, |(held, _)| *held)
                || header.backlink != previous.map(|(_, hash)| *hash)
            {
                gaps.insert(author);
            }
            if !epochs.contains_key(&header.extensions.generation) {
                let slot = (scope, header.extensions.generation);
                let records = inner.escrow_of(&[slot].into(), None).await?;
                let Some(record) = records
                    .iter()
                    .find(|record| inner.judge_escrow(record, time).is_ok())
                else {
                    return Ok(None);
                };
                epochs.insert(slot.1, record.record().tag());
            }
            let held = header
                .seq_num
                .checked_add(1)
                .filter(|held| *held <= i64::MAX as u64)
                .ok_or_else(|| denied("custody frame frontier sequence overflow"))?;
            logs.insert(author, (held, header.hash()));
            if logs.len() > MAX_CHECKPOINT_LOGS || epochs.len() > MAX_SECURE_RECORDS {
                return Ok(None);
            }
        }
        let logs = logs
            .into_iter()
            .filter(|(author, _)| !gaps.contains(author))
            .map(|(author, (held, hash))| (author, held, hash))
            .collect();
        let body = Coverage {
            version: 1,
            descriptor: inner.config.manifest.descriptor().map_err(credential)?,
            scope,
            tip: inner.workspace.tip,
            signer: inner.config.node.public_key(),
            audience: peer,
            binding,
            challenge,
            chain: inner.config.credentials.clone(),
            logs,
            epochs,
            naming,
            checkpoint,
        };
        if encode(&body)?.len() > SECURE_MESSAGE_BYTES / 2 {
            return Ok(None);
        }
        let signature = inner
            .config
            .node
            .sign(&signing_bytes("rocky.custody-receipt.v1", &body)?);
        Ok(Some(CustodyReceipt { body, signature }))
    }
    pub(crate) async fn record_custody_receipt(
        &self,
        peer: PublicKey,
        scope: ResourceScope,
        binding: Commitment,
        challenge: Commitment,
        receipt: CustodyReceipt,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock(Class::Background).await;
        let body = &receipt.body;
        if body.signer != peer
            || body.scope != scope
            || body.binding != binding
            || body.challenge != challenge
            || body.tip != inner.workspace.tip
        {
            return Err(denied("custody receipt outside challenge"));
        }
        inner.check_receipt(&receipt)?;
        if body.naming != inner.naming_tag().await? {
            return Err(denied("custody receipt naming escrow mismatch"));
        }
        let mut required = inner
            .conn
            .query(
                "SELECT header FROM secure_operations WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        while let Some(row) = required.next().await? {
            let header: p2panda_core::Header<Extensions> = decode(&row.get::<Vec<u8>>(0)?)?;
            if receipt.covers(header.public_key, header.seq_num)
                && !body.epochs.contains_key(&header.extensions.generation)
                && body.checkpoint.as_ref().is_none_or(|signed| {
                    !signed.statement.frontier.iter().any(|log| {
                        log.author == header.public_key && log.sequence >= header.seq_num
                    })
                })
            {
                return Err(denied("custody receipt lacks epoch escrow"));
            }
        }
        drop(required);
        let time = now()?;
        for (epoch, tag) in &body.epochs {
            let records = inner.escrow_of(&[(scope, *epoch)].into(), None).await?;
            if !records.iter().any(|record| {
                record.record().tag() == *tag && inner.judge_escrow(record, time).is_ok()
            }) {
                return Err(denied("custody receipt escrow mismatch"));
            }
        }
        if !inner
            .partition_custody(scope)
            .await?
            .peers
            .contains(&PeerId::of(peer))
        {
            return Ok(());
        }
        transaction!(inner, |inner| {
            let expected = collect_rows(inner.conn.query("SELECT binding,challenge FROM secure_custody_challenges WHERE scope=? AND peer=?", [encode(&scope)?, PeerId::of(peer).as_bytes().to_vec()]).await?).await?;
            if expected
                != vec![vec![
                    SqlValue::Blob(binding.to_vec()),
                    SqlValue::Blob(challenge.to_vec()),
                ]]
            {
                return Err(denied("custody receipt challenge was consumed or replaced"));
            }
            inner
                .conn
                .execute(
                    "DELETE FROM secure_custody_challenges WHERE scope=? AND peer=?",
                    [encode(&scope)?, PeerId::of(peer).as_bytes().to_vec()],
                )
                .await?;
            inner.conn.execute("INSERT INTO secure_custody_receipts VALUES(?,?,?) ON CONFLICT(scope,peer) DO UPDATE SET receipt=excluded.receipt", [encode(&scope)?, PeerId::of(peer).as_bytes().to_vec(), encode(&receipt)?]).await?;
            Ok(())
        })?;
        drop(flush_guard(inner, &self.inner).await?);
        Ok(())
    }
}
