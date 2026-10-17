//! Disk-backed construction; only one page and the logarithmic tree stack are live.
use super::*;
use rocky_core::{MigrationSum, migration_split};

impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) async fn migration_stage(
        &self,
        publication: Commitment,
    ) -> Result<Option<SchemaStage>, Error> {
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT stage FROM secure_staged_schema WHERE publication=? AND state='staged'",
                    [publication.to_vec()],
                )
                .await?,
        )
        .await?;
        let Some([SqlValue::Blob(bytes)]) = records.first().map(Vec::as_slice) else {
            return Ok(None);
        };
        let stage: SchemaStage = decode(bytes)?;
        Ok(stage
            .statements
            .iter()
            .any(|s| matches!(s, TipStatement::Opening(_)))
            .then_some(stage))
    }
    async fn put_migration_node(
        &self,
        publication: Commitment,
        start: u64,
        sum: MigrationSum,
    ) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT INTO secure_migration_nodes VALUES(?,?,?,?,?)",
                bindings(vec![
                    SqlValue::Blob(publication.to_vec()),
                    SqlValue::Integer(counter(start)?),
                    SqlValue::Integer(counter(sum.frames)?),
                    SqlValue::Integer(counter(sum.bytes)?),
                    SqlValue::Blob(sum.hash.to_vec()),
                ]),
            )
            .await?;
        Ok(())
    }
    async fn migration_node(
        &self,
        publication: Commitment,
        start: u64,
        count: u64,
    ) -> Result<MigrationSum, Error> {
        let records=collect_rows(self.conn.query("SELECT hash,bytes FROM secure_migration_nodes WHERE publication=? AND start=? AND frames=?",bindings(vec![SqlValue::Blob(publication.to_vec()),SqlValue::Integer(counter(start)?),SqlValue::Integer(counter(count)?)] )).await?).await?;
        let Some([SqlValue::Blob(hash), bytes]) = records.first().map(Vec::as_slice) else {
            return Err(Error::Corrupt("migration tree node missing".into()));
        };
        Ok(MigrationSum {
            hash: hash
                .as_slice()
                .try_into()
                .map_err(|_| denied("migration tree hash"))?,
            frames: count,
            bytes: number(bytes)?,
        })
    }
    async fn migration_proof(
        &self,
        s: &MigrationPublicationStatement,
        index: u64,
    ) -> Result<MigrationProof, Error> {
        let (mut start, mut count) = (0, s.frames);
        let mut siblings = Vec::new();
        while count > 1 {
            let left = migration_split(count).map_err(|e| denied(&e.to_string()))?;
            if index < start + left {
                siblings.push(
                    self.migration_node(s.publication, start + left, count - left)
                        .await?,
                );
                count = left;
            } else {
                siblings.push(self.migration_node(s.publication, start, left).await?);
                start += left;
                count -= left;
            }
        }
        siblings.reverse();
        Ok(MigrationProof {
            format: 1,
            index,
            siblings,
        })
    }
    pub(in crate::replica) async fn build_migration_intent(
        &self,
        stage: &SchemaStage,
        frames: &[EncryptedFrame],
    ) -> Result<(), Error> {
        let author = self
            .node_author
            .as_ref()
            .ok_or_else(|| denied("migration writer missing"))?;
        let opening = stage
            .statements
            .iter()
            .find_map(|s| match s {
                TipStatement::Opening(o) => Some(o),
                _ => None,
            })
            .ok_or_else(|| denied("migration opening missing"))?;
        let mut s = MigrationPublicationStatement {
            format: 1,
            scope: stage.scope,
            tip: stage.target,
            opening: opening.id().map_err(credential)?,
            parent: opening.statement.seal,
            schema: opening.statement.schema,
            publication: stage.publication,
            writer: *author.key.public_key().as_bytes(),
            author: author.number,
            frames: 0,
            bytes: 0,
            root: [0; 32],
        };
        for frame in frames {
            let op = frame.operation_ref()?;
            if op.header.extensions.tip != s.tip
                || op.header.public_key != author.key.public_key()
                || op.header.extensions.author_sequence != s.frames
            {
                return Err(denied("migration is not the writer's initial prefix"));
            }
            let bytes = frame.stored_bytes()?;
            self.conn
                .execute(
                    "INSERT INTO secure_migration_source VALUES(?,?,?,?)",
                    bindings(vec![
                        SqlValue::Blob(s.publication.to_vec()),
                        SqlValue::Integer(counter(s.frames)?),
                        SqlValue::Blob(op.hash.as_bytes().to_vec()),
                        SqlValue::Integer(counter(bytes)?),
                    ]),
                )
                .await?;
            s.frames = s
                .frames
                .checked_add(1)
                .ok_or_else(|| denied("migration extent overflow"))?;
            s.bytes = s
                .bytes
                .checked_add(bytes)
                .ok_or_else(|| denied("migration extent overflow"))?;
        }
        let mut stack: Vec<(u64, MigrationSum)> = Vec::new();
        let mut next = 0;
        while next < s.frames {
            let records=collect_rows(self.conn.query("SELECT idx,hash,bytes FROM secure_migration_source WHERE publication=? AND idx>=? ORDER BY idx LIMIT 256",bindings(vec![SqlValue::Blob(s.publication.to_vec()),SqlValue::Integer(counter(next)?)] )).await?).await?;
            if records.is_empty() {
                return Err(Error::Corrupt("migration source gap".into()));
            }
            for record in records {
                let [idx, SqlValue::Blob(hash), bytes] = record.as_slice() else {
                    return Err(Error::Corrupt("migration source".into()));
                };
                let mut start = number(idx)?;
                let mut sum = s
                    .leaf(
                        start,
                        hash.as_slice()
                            .try_into()
                            .map_err(|_| denied("migration frame hash"))?,
                        number(bytes)?,
                    )
                    .map_err(|e| denied(&e.to_string()))?;
                self.put_migration_node(s.publication, start, sum).await?;
                while stack
                    .last()
                    .is_some_and(|(_, left)| left.frames == sum.frames)
                {
                    let (left_start, left) =
                        stack.pop().ok_or_else(|| denied("migration tree stack"))?;
                    sum = MigrationSum::join(left, sum).map_err(|e| denied(&e.to_string()))?;
                    start = left_start;
                    self.put_migration_node(s.publication, start, sum).await?;
                }
                stack.push((start, sum));
                next += 1;
            }
        }
        let mut root = stack.pop().unwrap_or((0, s.empty()));
        while let Some((start, left)) = stack.pop() {
            root = (
                start,
                MigrationSum::join(left, root.1).map_err(|e| denied(&e.to_string()))?,
            );
            self.put_migration_node(s.publication, start, root.1)
                .await?;
        }
        s.root = root.1.hash;
        s.validate().map_err(|e| denied(&e.to_string()))?;
        self.conn.execute("INSERT INTO secure_migration_publications(publication,tip,statement,writer) VALUES(?,?,?,?)",[s.publication.to_vec(),encode(&s.tip)?,encode(&s)?,encode(&author.credentials)?]).await?;
        // Existing authoring owns this slice; proof construction adds no publication-sized vector.
        for frame in frames {
            let index = frame.operation_ref()?.header.extensions.author_sequence;
            let proof = self.migration_proof(&s, index).await?;
            self.classify_migration(&s, frame, &proof).await?;
        }
        self.conn
            .execute(
                "DELETE FROM secure_migration_source WHERE publication=?",
                [s.publication.to_vec()],
            )
            .await?;
        Ok(())
    }
}
