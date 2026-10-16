//! Explicit removal of an unfollowed workspace's retained frames.
use super::*;

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_dropped_tip_frames(tip BLOB NOT NULL,hash BLOB NOT NULL,PRIMARY KEY(tip,hash)) WITHOUT ROWID",
];

impl<A: RowApplication> EncryptedReplica<A> {
    /// Drops an unfollowed tip's local frames. Its graph, signed evidence,
    /// idempotency handles and node reservations remain. Local writes need
    /// acknowledged custody; retained dependencies and checkpoints prevent removal.
    pub async fn drop_tip(&self, tip: TipId) -> Result<u64, Error> {
        let mut inner = self.inner.lock().await;
        let graph = inner
            .graphs
            .values()
            .find(|g| g.schema_any(tip).is_ok())
            .ok_or_else(|| denied("unknown tip"))?;
        let canonical = graph.canonical_tip(tip).map_err(credential)?;
        let mut cursor = inner.tip;
        let mut visited = BTreeSet::new();
        while visited.insert(cursor) {
            if cursor == tip || cursor == canonical {
                return Err(denied("cannot drop the followed tip or its ancestors"));
            }
            let mut parent = None;
            for seal in &graph.seals {
                if seal.successor().map_err(credential)? == Some(cursor) {
                    parent = Some(seal.statement.tip);
                    break;
                }
            }
            if parent.is_none() {
                for opening in &graph.openings {
                    if opening.tip().map_err(credential)? == cursor {
                        parent = Some(
                            graph
                                .seal_by_id(opening.statement.seal)
                                .map_err(credential)?
                                .statement
                                .tip,
                        );
                        break;
                    }
                }
            }
            let Some(next) = parent else { break };
            cursor = next;
        }
        let all = rows(
            &inner.conn,
            "SELECT hash,header,LENGTH(header)+LENGTH(body) FROM secure_operations",
        )
        .await?;
        let mut removed = BTreeMap::new();
        let mut headers = Vec::new();
        for record in all {
            let [
                SqlValue::Blob(hash),
                SqlValue::Blob(bytes),
                SqlValue::Integer(size),
            ] = record.as_slice()
            else {
                return Err(Error::Corrupt("tip operation".into()));
            };
            let header: p2panda_core::Header<Extensions> = decode(bytes)?;
            if header.extensions.tip == tip {
                removed.insert(
                    hash.clone(),
                    (
                        *size,
                        header.extensions.scope,
                        header.public_key,
                        header.seq_num,
                    ),
                );
            }
            headers.push(header);
        }
        if removed.is_empty() {
            return Ok(0);
        }
        for header in &headers {
            if header.extensions.tip != tip
                && header
                    .previous
                    .iter()
                    .chain(header.backlink.iter())
                    .any(|hash| removed.contains_key(hash.as_bytes().as_slice()))
            {
                return Err(denied("another tip depends on retained frames"));
            }
        }
        // Held originals and signed void headers can still need a predecessor.
        let mut evidence = Vec::new();
        for record in rows(&inner.conn, "SELECT frame FROM secure_held_frames").await? {
            let [SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("held frame".into()));
            };
            let frame: EncryptedFrame = decode(bytes)?;
            evidence.push(frame.operation()?.header);
        }
        for record in rows(&inner.conn, "SELECT header FROM secure_voids").await? {
            let [SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("void header".into()));
            };
            evidence.push(decode::<p2panda_core::Header<Extensions>>(bytes)?);
        }
        if evidence.iter().any(|header| {
            header
                .previous
                .iter()
                .chain(header.backlink.iter())
                .any(|hash| removed.contains_key(hash.as_bytes().as_slice()))
        }) {
            return Err(denied("signed evidence depends on retained tip frames"));
        }
        let needed = inner.role().await?.custody.max(1) as i64;
        let local_nodes = rows(&inner.conn, "SELECT node FROM secure_line_nodes").await?;
        for (hash, (_, scope, author, sequence)) in &removed {
            for sql in [
                "SELECT 1 FROM secure_held_frames WHERE hash=?",
                "SELECT 1 FROM secure_voids WHERE hash=?",
                "SELECT 1 FROM secure_unoffered WHERE hash=?",
                "SELECT 1 FROM secure_staged WHERE hash=?",
                "SELECT 1 FROM secure_stubs WHERE hash=?",
                "SELECT 1 FROM secure_checkpoint_stubs WHERE hash=?",
                "SELECT 1 FROM secure_checkpoint_build_stubs WHERE hash=?",
            ] {
                if !collect_rows(inner.conn.query(sql, [hash.clone()]).await?)
                    .await?
                    .is_empty()
                {
                    return Err(denied(
                        "tip frames are required by held or coordinated evidence",
                    ));
                }
            }
            let local = *author == inner.config.node.public_key()
                || local_nodes
                    .iter()
                    .any(|row| row.as_slice() == [SqlValue::Blob(author.as_bytes().to_vec())]);
            if local {
                let acknowledged = collect_rows(
                    inner
                        .conn
                        .query(
                            "SELECT peer FROM secure_ack WHERE hash=? AND applied=1",
                            [hash.clone()],
                        )
                        .await?,
                )
                .await?;
                let mut own =
                    BTreeSet::from([
                        rocky_credentials::PeerId::of(inner.config.node.public_key())
                            .as_bytes()
                            .to_vec(),
                    ]);
                for record in &local_nodes {
                    let [SqlValue::Blob(bytes)] = record.as_slice() else {
                        return Err(Error::Corrupt("line node".into()));
                    };
                    let public = PublicKey::try_from(bytes.as_slice())
                        .map_err(|_| Error::Corrupt("line node key".into()))?;
                    own.insert(rocky_credentials::PeerId::of(public).as_bytes().to_vec());
                }
                // Acknowledged per operation or through a reported frontier,
                // as pruning counts them.
                let (_, mut applied) = inner
                    .covering_peers(&encode(scope)?, *author, *sequence)
                    .await?;
                for record in &acknowledged {
                    let [SqlValue::Blob(peer)] = record.as_slice() else {
                        return Err(Error::Corrupt("acknowledgment".into()));
                    };
                    applied.insert(super::roles::peer_id(peer)?);
                }
                let remote = applied
                    .iter()
                    .filter(|peer| !own.contains(peer.as_bytes().as_slice()))
                    .count() as i64;
                if remote < needed {
                    return Err(denied(
                        "dropping local tip writes requires acknowledged custody",
                    ));
                }
            }
            if let Some(id) = inner.find_scope_id(*scope).await?
                && (!collect_rows(
                    inner
                        .conn
                        .query(
                            "SELECT 1 FROM secure_checkpoints WHERE scope=? LIMIT 1",
                            [id],
                        )
                        .await?,
                )
                .await?
                .is_empty()
                    || inner.has_pruned_prefix(*scope).await?)
            {
                return Err(denied("checkpoint history prevents dropping tip frames"));
            }
        }
        let count = removed.len() as u64;
        let bytes = removed.values().map(|(size, _, _, _)| size).sum::<i64>();
        transaction!(inner, |inner| {
            for hash in removed.keys() {
                inner
                    .conn
                    .execute(
                        "INSERT OR IGNORE INTO secure_dropped_tip_frames VALUES(?,?)",
                        [encode(&tip)?, hash.clone()],
                    )
                    .await?;
                for table in [
                    "secure_operations",
                    "secure_outbox",
                    "secure_ack",
                    "secure_applied",
                    "secure_publication_frames",
                ] {
                    inner
                        .conn
                        .execute(format!("DELETE FROM {table} WHERE hash=?"), [hash.clone()])
                        .await?;
                }
                inner
                    .conn
                    .execute("DELETE FROM secure_logs WHERE hash=?", [hash.clone()])
                    .await?;
            }
            inner
                .conn
                .execute(
                    "UPDATE secure_retained SET records=records-?,bytes=bytes-?",
                    [count as i64, bytes],
                )
                .await?;
            inner
                .conn
                .execute(
                    "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                    (),
                )
                .await?;
            Ok(())
        })?;
        inner.publish_revision().await?;
        inner.flush()?;
        Ok(count)
    }
}

impl<A: RowApplication> Inner<A> {
    pub(super) async fn ensure_tip_restored(&self) -> Result<(), Error> {
        let graph = self
            .graphs
            .values()
            .find(|graph| graph.schema_any(self.tip).is_ok());
        let mut missing = self.conn.query(
            "SELECT tip FROM secure_dropped_tip_frames d WHERE NOT EXISTS(SELECT 1 FROM secure_operations o WHERE o.hash=d.hash)", ()).await?;
        while let Some(row) = missing.next().await? {
            let dropped: TipId = decode(&row.get::<Vec<u8>>(0)?)?;
            let mut cursor = self.tip;
            let mut visited = BTreeSet::new();
            while visited.insert(cursor) {
                if dropped == cursor {
                    return Err(denied("catch up the dropped tip before authoring"));
                }
                let Some(graph) = graph else { break };
                let Some((parent, _)) = Self::tip_parent(graph, cursor)? else {
                    break;
                };
                cursor = parent;
            }
        }
        Ok(())
    }
}
