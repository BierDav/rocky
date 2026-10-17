//! Staged publications: one local SQL transaction of any size whose writes
//! leave as consecutive ordinary bundles. See
//! `docs/decisions/0071-staged-migration-publication.md`.
use super::*;

pub(super) const SCHEMA: &[&str] = &[
    // `published` counts the leading bundles some peer holds.
    "CREATE TABLE IF NOT EXISTS secure_publications(publication BLOB PRIMARY KEY,bundles INTEGER NOT NULL,published INTEGER NOT NULL) WITHOUT ROWID",
    // Retained frames of publications, outside the retained-history bound
    // until they are pruned.
    "CREATE TABLE IF NOT EXISTS secure_publication_frames(hash BLOB PRIMARY KEY,bytes INTEGER NOT NULL) WITHOUT ROWID",
];

/// How far a staged publication has left this replica.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Publication {
    pub id: Commitment,
    /// Bundles the transaction committed.
    pub bundles: u64,
    /// Leading bundles every frame of which a peer reported holding.
    pub published: u64,
}

/// The handle of a publication's bundle at `position`.
pub(super) fn bundle_handle(publication: &Commitment, position: u64) -> Commitment {
    rocky_core::commitment(
        "rocky.secure-publication.v1",
        &[publication.as_slice(), &position.to_le_bytes()].concat(),
    )
}

#[cfg(test)]
thread_local! {
    /// Rows of one publication bundle, lowered by tests on their thread.
    pub(crate) static BUNDLE_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(MAX_BUNDLE_ROWS) };
}
fn bundle_rows() -> usize {
    #[cfg(test)]
    {
        BUNDLE_ROWS.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        MAX_BUNDLE_ROWS
    }
}

/// Splits ordered writes into consecutive bundles within the row, frame
/// and byte bounds of one bundle.
fn chunks(writes: Vec<RowWrite>) -> Result<Vec<Vec<RowWrite>>, Error> {
    // The encoded writes and an empty dependency list stay below the bound.
    const SLACK: usize = 64;
    let mut chunks = Vec::new();
    let mut chunk: Vec<RowWrite> = Vec::new();
    let mut rows = BTreeMap::<ResourceScope, usize>::new();
    let (mut frames, mut bytes) = (0, SLACK);
    for write in writes {
        let size = encode(&write)?.len();
        if size + SLACK > MAX_BUNDLE_BYTES {
            return Err(Error::Quota {
                resource: "bundle bytes",
                limit: MAX_BUNDLE_BYTES,
            });
        }
        let opens = rows.get(&write.change.scope).copied().unwrap_or(0) % ROWS_PER_FRAME == 0;
        if !chunk.is_empty()
            && (chunk.len() == bundle_rows()
                || (opens && frames == MAX_BUNDLE_FRAMES)
                || bytes + size > MAX_BUNDLE_BYTES)
        {
            chunks.push(std::mem::take(&mut chunk));
            rows.clear();
            (frames, bytes) = (0, SLACK);
        }
        if rows.get(&write.change.scope).copied().unwrap_or(0) % ROWS_PER_FRAME == 0 {
            frames += 1;
        }
        *rows.entry(write.change.scope).or_default() += 1;
        bytes += size;
        chunk.push(write);
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    Ok(chunks)
}

impl<A: RowApplication> Inner<A> {
    async fn publication_record(
        &self,
        publication: &Commitment,
    ) -> Result<Option<(u64, u64)>, Error> {
        let stored = collect_rows(
            self.conn
                .query(
                    "SELECT bundles,published FROM secure_publications WHERE publication=?",
                    [publication.to_vec()],
                )
                .await?,
        )
        .await?;
        match stored.first().map(Vec::as_slice) {
            None => Ok(None),
            Some([SqlValue::Integer(bundles), SqlValue::Integer(published)]) => {
                Ok(Some((*bundles as u64, *published as u64)))
            }
            Some(_) => Err(Error::Corrupt("publication record".into())),
        }
    }
    /// The frames a publication committed, in bundle order.
    async fn publication_frames(
        &self,
        publication: &Commitment,
        bundles: u64,
    ) -> Result<Vec<EncryptedFrame>, Error> {
        let mut frames = Vec::new();
        for position in 0..bundles {
            for frame in self
                .bundle_frames(&bundle_handle(publication, position))
                .await?
            {
                frames.extend(frame);
            }
        }
        Ok(frames)
    }
    /// Each member of the bundle under `handle`; None once pruned.
    async fn bundle_frames(
        &self,
        handle: &Commitment,
    ) -> Result<Vec<Option<EncryptedFrame>>, Error> {
        let mut frames = Vec::new();
        for record in collect_rows(
            self.conn
                .query(
                    "SELECT hash FROM secure_bundle_handles WHERE handle=? ORDER BY member",
                    [handle.to_vec()],
                )
                .await?,
        )
        .await?
        {
            let [SqlValue::Blob(hash)] = record.as_slice() else {
                return Err(Error::Corrupt("bundle handle".into()));
            };
            let hash = Hash::try_from(hash.as_slice())
                .map_err(|_| Error::Corrupt("bundle handle".into()))?;
            frames.push(self.frame(hash).await?);
        }
        Ok(frames)
    }
    /// Parents first: writes with an image by the depth of their table's
    /// references, new identities ascending, then deletions children first.
    fn publication_order(&self, writes: Vec<RowWrite>) -> Result<Vec<RowWrite>, Error> {
        let typed = self
            .workspace
            .typed
            .as_ref()
            .ok_or_else(|| Error::Incompatible("no typed tables are installed".into()))?;
        let schema = typed.rows.schema();
        let mut depths = BTreeMap::<String, usize>::new();
        // A reference chain is at most as long as the table list.
        for _ in 0..schema.tables.len() {
            for table in &schema.tables {
                let depth = table
                    .foreign_keys
                    .iter()
                    .filter(|key| !key.table.eq_ignore_ascii_case(&table.sql_name))
                    .map(|key| {
                        depths
                            .get(&key.table.to_ascii_lowercase())
                            .map_or(1, |d| d + 1)
                    })
                    .max()
                    .unwrap_or(0);
                depths.insert(table.sql_name.to_ascii_lowercase(), depth);
            }
        }
        let mut keyed = writes
            .into_iter()
            .map(|write| {
                let depth = typed
                    .by_scope(write.change.scope)
                    .and_then(|table| depths.get(&table.table.sql_name.to_ascii_lowercase()))
                    .copied()
                    .unwrap_or(0);
                let birth = match write.change.before {
                    None => self
                        .app
                        .row_identity(&write.change)
                        .map_err(|message| denied(&message))?
                        .map(|identity| identity.integer()),
                    Some(_) => None,
                };
                let key = match write.change.after {
                    Some(_) => (0, depth as i64, birth.unwrap_or(0)),
                    None => (1, -(depth as i64), 0),
                };
                Ok((key, write))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        keyed.sort_by_key(|(key, _)| *key);
        Ok(keyed.into_iter().map(|(_, write)| write).collect())
    }
    /// Authors `writes` as the consecutive bundles of `publication` inside
    /// the caller's open engine transaction and records it there, every
    /// write stamped with this commit's time. A recorded publication
    /// returns its frames again; the caller rolls back.
    pub(super) async fn stage_publication(
        &mut self,
        publication: Commitment,
        writes: Vec<RowWrite>,
    ) -> Result<(Vec<EncryptedFrame>, bool), Error> {
        if let Some((bundles, _)) = self.publication_record(&publication).await? {
            return Ok((self.publication_frames(&publication, bundles).await?, true));
        }
        let chunks = chunks(self.publication_order(writes)?)?;
        let migration = self.migration_stage(publication).await?;
        self.publishing = migration.is_some();
        let authored = self.author_publication(&publication, &chunks).await;
        self.publishing = false;
        let frames = authored?;
        for position in 0..chunks.len() {
            self.conn
                .execute(
                    "INSERT INTO secure_publication_frames SELECT o.hash,length(o.header)+length(o.body) FROM secure_bundle_handles h JOIN secure_operations o ON o.hash=h.hash WHERE h.handle=?",
                    [bundle_handle(&publication, position as u64).to_vec()],
                )
                .await?;
        }
        self.conn
            .execute(
                "INSERT INTO secure_publications VALUES(?,?,0)",
                bindings(vec![
                    SqlValue::Blob(publication.to_vec()),
                    SqlValue::Integer(chunks.len() as i64),
                ]),
            )
            .await?;
        if let Some(stage) = migration {
            self.build_migration_intent(&stage, &frames).await?;
        }
        self.commit_schema_stage(publication).await?;
        Ok((frames, false))
    }
    async fn author_publication(
        &mut self,
        publication: &Commitment,
        chunks: &[Vec<RowWrite>],
    ) -> Result<Vec<EncryptedFrame>, Error> {
        let time = now()?;
        let mut frames = Vec::new();
        for (position, chunk) in chunks.iter().enumerate() {
            let encoded = encode(&(chunk, Vec::<Hash>::new()))?;
            if encoded.len() > MAX_BUNDLE_BYTES {
                return Err(Error::Quota {
                    resource: "bundle bytes",
                    limit: MAX_BUNDLE_BYTES,
                });
            }
            let input = rocky_core::commitment("rocky.secure-bundle-input.v1", &encoded);
            let handle = bundle_handle(publication, position as u64);
            frames.extend(
                self.author_bundle(handle, input, chunk, &[], time, None)
                    .await?,
            );
        }
        Ok(frames)
    }
    /// Whether some peer reported holding every member of the bundle.
    async fn bundle_published(&self, handle: &Commitment) -> Result<bool, Error> {
        for frame in self.bundle_frames(handle).await? {
            // Pruning waits for the copies retention requires.
            let Some(frame) = frame else { continue };
            let header = frame.operation_ref()?.header.clone();
            let (held, _) = self
                .covering_peers(&encode(&frame.scope()?)?, header.public_key, header.seq_num)
                .await?;
            if held.is_empty() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Every staged publication this replica committed, with how many of
    /// its leading bundles a peer reported holding.
    pub async fn publications(&self) -> Result<Vec<Publication>, Error> {
        let mut inner = self.inner.lock().await;
        let mut publications = Vec::new();
        for record in rows(
            &inner.conn,
            "SELECT publication,bundles,published FROM secure_publications ORDER BY publication",
        )
        .await?
        {
            let [
                SqlValue::Blob(id),
                SqlValue::Integer(bundles),
                SqlValue::Integer(published),
            ] = record.as_slice()
            else {
                return Err(Error::Corrupt("publication record".into()));
            };
            let id: Commitment = id
                .as_slice()
                .try_into()
                .map_err(|_| Error::Corrupt("publication record".into()))?;
            let (bundles, mut published) = (*bundles as u64, *published as u64);
            // A publication on a tip that lost its race stays retained on
            // its branch; it is not this workspace's to publish.
            if let Some(Some(frame)) = inner
                .bundle_frames(&bundle_handle(&id, 0))
                .await?
                .into_iter()
                .next()
                && matches!(
                    inner.frame_reach(&frame)?,
                    super::tips::FrameReach::Held | super::tips::FrameReach::Unfollowed
                )
            {
                continue;
            }
            let reached = published;
            while published < bundles
                && inner
                    .bundle_published(&bundle_handle(&id, published))
                    .await?
            {
                published += 1;
            }
            if published != reached {
                transaction!(&mut inner, |inner| {
                    inner
                        .conn
                        .execute(
                            "UPDATE secure_publications SET published=? WHERE publication=?",
                            bindings(vec![
                                SqlValue::Integer(published as i64),
                                SqlValue::Blob(id.to_vec()),
                            ]),
                        )
                        .await?;
                    Ok(())
                })?;
            }
            publications.push(Publication {
                id,
                bundles,
                published,
            });
        }
        Ok(publications)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(scope: u8, row: u8) -> RowWrite {
        let scope = ResourceScope {
            domain: rocky_core::Scope {
                mesh: rocky_core::MeshId([1; 16]),
                app: rocky_core::AppId([1; 16]),
                domain: rocky_core::DomainId([1; 16]),
            },
            encryption: [scope; 32],
            table: [scope; 32],
        };
        RowWrite::changed(RowChange {
            scope,
            row: [row; 32],
            before: None,
            after: Some(BTreeMap::from([([0; 32], vec![row])])),
        })
    }

    #[test]
    fn chunks_stay_within_rows_and_frames() {
        // One scope: 2,048 rows fill one bundle exactly.
        let one: Vec<_> = (0..=255u8)
            .cycle()
            .take(MAX_BUNDLE_ROWS + 1)
            .map(|row| write(1, row))
            .collect();
        let split = chunks(one).unwrap();
        assert_eq!(
            split.iter().map(Vec::len).collect::<Vec<_>>(),
            [MAX_BUNDLE_ROWS, 1]
        );
        // 65 scopes of one row each need 65 frames.
        let many: Vec<_> = (0..65u8).map(|scope| write(scope, 0)).collect();
        let split = chunks(many).unwrap();
        assert_eq!(
            split.iter().map(Vec::len).collect::<Vec<_>>(),
            [MAX_BUNDLE_FRAMES, 1]
        );
    }
}
