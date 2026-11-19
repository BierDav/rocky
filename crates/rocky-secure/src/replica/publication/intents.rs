use super::*;
use rocky_core::replay::IntentStatement;

const PUBLICATION_INTENTS_TABLE: &str = "CREATE TABLE IF NOT EXISTS secure_publication_intents(publication BLOB NOT NULL,position INTEGER NOT NULL,members BLOB NOT NULL,PRIMARY KEY(publication,position)) WITHOUT ROWID";
const PUBLICATION_CURSORS_TABLE: &str = "CREATE TABLE IF NOT EXISTS secure_publication_cursors(publication BLOB PRIMARY KEY,next INTEGER NOT NULL,total INTEGER NOT NULL) WITHOUT ROWID";
const PUBLICATION_UNSENT_TABLE: &str = "CREATE TABLE IF NOT EXISTS secure_publication_unsent(hash BLOB PRIMARY KEY,publication BLOB NOT NULL) WITHOUT ROWID";
pub(in crate::replica) const SCHEMA: &[&str] = &[
    PUBLICATION_INTENTS_TABLE,
    PUBLICATION_CURSORS_TABLE,
    PUBLICATION_UNSENT_TABLE,
];

pub const PUBLICATION_CHUNK_BUNDLES: u64 = 8;
const ANDROID_WRITER_CACHE_PAGES: u64 = 1024;
const MEASURED_CHUNK_PAGES_ON_THE_REPLAYED_100K_FIXTURE: u64 = 184;
const MEASURED_PAGES_PER_BUNDLE_ON_THE_REPLAYED_100K_FIXTURE: u64 = 36;
const HALF_THE_ANDROID_WRITER_CACHE_PAGES: u64 = ANDROID_WRITER_CACHE_PAGES / 2;
const _: () = assert!(
    MEASURED_CHUNK_PAGES_ON_THE_REPLAYED_100K_FIXTURE
        + MEASURED_PAGES_PER_BUNDLE_ON_THE_REPLAYED_100K_FIXTURE * PUBLICATION_CHUNK_BUNDLES
        < HALF_THE_ANDROID_WRITER_CACHE_PAGES,
    "a publication chunk must dirty under half the Android writer cache"
);

type Member = (
    ResourceScope,
    Vec<IntentStatement>,
    Option<serde_bytes::ByteBuf>,
    Vec<checkpoint::IdentityMark>,
);

#[cfg(feature = "qualification")]
pub static CHUNK_PAGES: std::sync::Mutex<Vec<(u64, u64)>> = std::sync::Mutex::new(Vec::new());

#[cfg(feature = "qualification")]
async fn wal_pages<A: RowApplication>(inner: &Inner<A>, restart: bool) -> Result<u64, Error> {
    let mode = if restart { "TRUNCATE" } else { "PASSIVE" };
    match rows(&inner.conn, &format!("PRAGMA wal_checkpoint({mode})"))
        .await?
        .first()
        .map(Vec::as_slice)
    {
        Some([_, SqlValue::Integer(log), _]) => Ok(*log as u64),
        _ => Err(Error::Corrupt("WAL checkpoint".into())),
    }
}

fn kill(point: &str) {
    #[cfg(feature = "qualification")]
    super::super::crash::qualification_kill(point);
    #[cfg(not(feature = "qualification"))]
    let _ = point;
}

impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) async fn stage_intents(
        &self,
        publication: Commitment,
        intents: super::bundle::Intents,
        births: &BTreeMap<ResourceScope, Vec<checkpoint::IdentityMark>>,
    ) -> Result<bool, Error> {
        if self.migration_stage(publication).await?.is_none() {
            return Err(denied("replayed writes commit only as a plain transaction"));
        }
        if self.publication_cursor(publication).await?.is_some()
            || self.publication_record(&publication).await?.is_some()
        {
            return Ok(false);
        }
        let mut partitions = BTreeMap::<Option<Vec<u8>>, Vec<Member>>::new();
        for (scope, (statements, undo)) in intents {
            let partition = self.partitions.get(&scope).map(encode).transpose()?;
            let undo = match super::replay::undo_blob(&undo)? {
                SqlValue::Blob(bytes) => Some(serde_bytes::ByteBuf::from(bytes)),
                _ => None,
            };
            let born = births.get(&scope).cloned().unwrap_or_default();
            partitions
                .entry(partition)
                .or_default()
                .push((scope, statements, undo, born));
        }
        let total = partitions.len();
        for (position, members) in partitions.into_values().enumerate() {
            if members.len() > MAX_BUNDLE_FRAMES {
                return Err(Error::Quota {
                    resource: "bundle frames",
                    limit: MAX_BUNDLE_FRAMES,
                });
            }
            self.conn
                .execute(
                    "INSERT INTO secure_publication_intents VALUES(?,?,?)",
                    bindings(vec![
                        SqlValue::Blob(publication.to_vec()),
                        SqlValue::Integer(position as i64),
                        SqlValue::Blob(encode(&members)?),
                    ]),
                )
                .await?;
        }
        self.conn
            .execute(
                "INSERT INTO secure_publication_cursors VALUES(?,0,?)",
                bindings(vec![
                    SqlValue::Blob(publication.to_vec()),
                    SqlValue::Integer(total as i64),
                ]),
            )
            .await?;
        Ok(true)
    }
    pub(in crate::replica) async fn publication_cursor(
        &self,
        publication: Commitment,
    ) -> Result<Option<(u64, u64)>, Error> {
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT next,total FROM secure_publication_cursors WHERE publication=?",
                    [publication.to_vec()],
                )
                .await?,
        )
        .await?;
        match records.first().map(Vec::as_slice) {
            None => Ok(None),
            Some([SqlValue::Integer(next), SqlValue::Integer(total)]) => {
                Ok(Some((*next as u64, *total as u64)))
            }
            Some(_) => Err(Error::Corrupt("publication cursor".into())),
        }
    }
    async fn author_intent_chunk(
        &mut self,
        publication: Commitment,
        start: u64,
        end: u64,
    ) -> Result<(), Error> {
        let time = now()?;
        self.local_grants(time)?;
        for position in start..end {
            let records = collect_rows(
                self.conn
                    .query(
                        "SELECT members FROM secure_publication_intents WHERE publication=? AND position=?",
                        bindings(vec![
                            SqlValue::Blob(publication.to_vec()),
                            SqlValue::Integer(position as i64),
                        ]),
                    )
                    .await?,
            )
            .await?;
            let Some([SqlValue::Blob(members)]) = records.first().map(Vec::as_slice) else {
                return Err(Error::Corrupt("staged publication intents".into()));
            };
            let members: Vec<Member> = decode_bounded(members, usize::MAX)?;
            let statements = members
                .iter()
                .map(|(scope, statements, ..)| (*scope, statements.clone()))
                .collect();
            let births: BTreeMap<_, _> = members
                .iter()
                .map(|(scope, .., births)| (*scope, births.clone()))
                .collect();
            let encoded = super::bundle::input_bytes(&[], &[], &statements)?;
            if encoded.len() > MAX_BUNDLE_BYTES {
                return Err(Error::Quota {
                    resource: "bundle bytes",
                    limit: MAX_BUNDLE_BYTES,
                });
            }
            let input = rocky_core::commitment("rocky.secure-bundle-input.v1", &encoded);
            let handle = bundle_handle(&publication, position);
            let frames = match self.lookup_bundle(handle, input).await? {
                Some(Ok(frames)) => frames,
                Some(Err(_)) => return Err(Error::Corrupt("staged publication bundle".into())),
                None => {
                    let intents = members
                        .into_iter()
                        .map(|(scope, statements, undo, _)| {
                            let undo = match undo {
                                Some(bytes) => super::replay::undo_of(&bytes)?,
                                None => Vec::new(),
                            };
                            Ok((scope, (statements, undo)))
                        })
                        .collect::<Result<_, Error>>()?;
                    self.publishing = true;
                    let authored = self.author_sql_bundle((handle, input), intents, time).await;
                    self.publishing = false;
                    let frames = authored?;
                    for frame in &frames {
                        if let Some(births) = births.get(&frame.scope()?) {
                            self.record_births(frame, births).await?;
                        }
                    }
                    frames
                }
            };
            for frame in &frames {
                self.conn
                    .execute(
                        "INSERT OR IGNORE INTO secure_publication_unsent VALUES(?,?)",
                        [frame.id()?.as_bytes().to_vec(), publication.to_vec()],
                    )
                    .await?;
            }
        }
        self.conn
            .execute(
                "UPDATE secure_publication_cursors SET next=? WHERE publication=?",
                bindings(vec![
                    SqlValue::Integer(end as i64),
                    SqlValue::Blob(publication.to_vec()),
                ]),
            )
            .await?;
        Ok(())
    }
    async fn finish_intents(&mut self, publication: Commitment, total: u64) -> Result<(), Error> {
        let stage = self
            .migration_stage(publication)
            .await?
            .ok_or_else(|| denied("migration publication is not staged"))?;
        let frames = self.publication_frames(&publication, total).await?;
        self.record_publication(publication, total, &frames, Some(stage))
            .await?;
        let node = self
            .node_author
            .as_ref()
            .ok_or_else(|| denied("node author credential missing"))?
            .key
            .public_key();
        let time = now()?;
        for position in 0..total {
            let handle = bundle_handle(&publication, position);
            let members: Vec<_> = self
                .bundle_frames(&handle)
                .await?
                .into_iter()
                .flatten()
                .collect();
            if members.len() > 1 {
                let held: Vec<_> = members
                    .iter()
                    .enumerate()
                    .map(|(index, frame)| (frame, index as u16))
                    .collect();
                self.publish_outcome(
                    super::bundle::transaction_id(node, &handle),
                    &held,
                    None,
                    time,
                )
                .await?;
            }
        }
        for table in [
            "secure_publication_intents",
            "secure_publication_cursors",
            "secure_publication_unsent",
        ] {
            self.conn
                .execute(
                    format!("DELETE FROM {table} WHERE publication=?"),
                    [publication.to_vec()],
                )
                .await?;
        }
        Ok(())
    }
    pub(crate) async fn unsent(&self) -> Result<HashSet<Hash>, Error> {
        let mut unsent = HashSet::new();
        for record in rows(&self.conn, "SELECT hash FROM secure_publication_unsent").await? {
            let [SqlValue::Blob(hash)] = record.as_slice() else {
                return Err(Error::Corrupt("unsent publication frame".into()));
            };
            unsent.insert(super::checkpoint::hash_of(hash)?);
        }
        Ok(unsent)
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn publication_staged(&self, publication: Commitment) -> Result<bool, Error> {
        Ok(self
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .publication_cursor(publication)
            .await?
            .is_some())
    }
    pub async fn author_staged_publication(&self, publication: Commitment) -> Result<(), Error> {
        while self.author_publication_step(publication).await? {
            rocky_platform::task::yield_now().await;
        }
        Ok(())
    }
    pub(crate) async fn author_publication_step(
        &self,
        publication: Commitment,
    ) -> Result<bool, Error> {
        let passive = self
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .file
            .passive
            .clone();
        passive.wait().await;
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let Some((next, total)) = inner.publication_cursor(publication).await? else {
            return Ok(false);
        };
        inner
            .checkpoint_over(super::group::WAL_CHECKPOINT_BYTES)
            .await;
        kill(match next {
            0 => "publication-staged",
            next if next < total => "publication-between",
            _ => "publication-final",
        });
        if next >= total {
            transaction!(&mut inner, |inner| {
                inner.finish_intents(publication, total).await
            })?;
            inner.note_commit().await;
            drop(flush_guard(inner, &self.inner).await?);
            return Ok(false);
        }
        let end = total.min(next + PUBLICATION_CHUNK_BUNDLES);
        #[cfg(feature = "qualification")]
        let measured = std::env::var_os("ROCKY_MEASURE_PUBLICATION_PAGES").is_some();
        #[cfg(feature = "qualification")]
        if measured {
            drop(flush_guard(inner, &self.inner).await?);
            inner = self.inner.lock(crate::replica::Class::Background).await;
            if next == 0 {
                let pages = wal_pages(&inner, false).await?;
                CHUNK_PAGES
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((0, pages));
            }
            wal_pages(&inner, true).await?;
        }
        let authored = transaction!(&mut inner, |inner| {
            inner.author_intent_chunk(publication, next, end).await
        });
        if let Err(error @ (Error::Rejected(_) | Error::Blocked(_))) = authored {
            drop(inner);
            let refusal = match error {
                Error::Rejected(refusal) => refusal,
                blocked => blocked.to_string(),
            };
            let blockers = self
                .blockers()
                .await?
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(Error::Rejected(format!(
                "the staged publication stopped at bundle {next} of {total}: {refusal}; blockers: [{blockers}]"
            )));
        }
        authored?;
        inner.note_commit().await;
        #[cfg(feature = "qualification")]
        if measured {
            let pages = wal_pages(&inner, false).await?;
            CHUNK_PAGES
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((end - next, pages));
        }
        drop(inner);
        kill("publication-chunk");
        Ok(true)
    }
}
