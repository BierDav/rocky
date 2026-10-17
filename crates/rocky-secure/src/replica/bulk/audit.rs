//! Provenance is audited independently of whether a frame is still applicable.
use super::*;

impl<A: RowApplication> Inner<A> {
    pub(crate) async fn audit_migration_accounts(&self) -> Result<(), Error> {
        let mut after: Vec<u8> = Vec::new();
        loop {
            let records=collect_rows(self.conn.query("SELECT publication,statement,signed,writer,retained_frames,retained_bytes,conflict FROM secure_migration_publications WHERE publication>? ORDER BY publication LIMIT 64",[after.clone()]).await?).await?;
            if records.is_empty() {
                break;
            }
            for record in records {
                let [
                    SqlValue::Blob(publication),
                    SqlValue::Blob(statement),
                    signed,
                    SqlValue::Blob(writer),
                    count,
                    bytes,
                    conflict,
                ] = record.as_slice()
                else {
                    return Err(Error::Corrupt("migration ledger".into()));
                };
                let s: MigrationPublicationStatement = decode(statement)?;
                s.validate().map_err(|e| denied(&e.to_string()))?;
                if publication.as_slice() != s.publication {
                    return Err(Error::Corrupt("migration identity".into()));
                }
                match signed {
                    SqlValue::Blob(encoded) => {
                        let signed =
                            SignedMigrationPublication::from_bytes(encoded).map_err(credential)?;
                        let writer: rocky_credentials::AuthorBinding = decode(writer)?;
                        if signed.statement != s || writer != signed.writer {
                            return Err(Error::Corrupt("migration endorsement differs".into()));
                        }
                        signed
                            .verify(self.config.publisher, self.graph_for(s.scope)?, None)
                            .map_err(credential)?;
                    }
                    SqlValue::Null => {
                        // An unsigned local intent is never transferable. Its private journal
                        // and historically certified writer authenticate its provenance until signing.
                        let staged=collect_rows(self.conn.query("SELECT stage FROM secure_staged_schema WHERE publication=? AND state='committed'",[publication.clone()]).await?).await?;
                        let Some([SqlValue::Blob(encoded)]) = staged.first().map(Vec::as_slice)
                        else {
                            return Err(Error::Corrupt("unsigned public migration".into()));
                        };
                        let stage: SchemaStage = decode(encoded)?;
                        let graph = self.graph_for(s.scope)?;
                        let opening = graph
                            .openings
                            .iter()
                            .find(|o| o.id().ok() == Some(s.opening))
                            .ok_or_else(|| denied("migration opening missing"))?;
                        opening
                            .verify(self.config.publisher, graph)
                            .map_err(credential)?;
                        let close = graph.seal_by_id(s.parent).map_err(credential)?;
                        if opening.tip().map_err(credential)? != s.tip
                            || close.statement.tip != stage.source
                            || !matches!(
                                close.statement.action,
                                rocky_credentials::SealAction::Close
                            )
                            || graph.schema_any(stage.source).map_err(credential)? == s.schema
                            || !graph
                                .succeeds_any(stage.source, s.tip)
                                .map_err(credential)?
                        {
                            return Err(Error::Corrupt("migration intent ancestry".into()));
                        }
                        let chain: CredentialChain = decode(writer)?;
                        let node = PublicKey::from_bytes(&s.writer)
                            .map_err(|_| denied("migration writer key"))?;
                        chain
                            .provenance(self.config.publisher, node)
                            .map_err(credential)?;
                        if stage.target != s.tip
                            || stage.publication != s.publication
                            || opening.statement.seal != s.parent
                            || opening.statement.schema != s.schema
                            || chain.author_number().map_err(credential)? != s.author
                            || chain.0.last().map(|c| c.issuer)
                                != Some(opening.endorsement().signer(self.config.publisher))
                        {
                            return Err(Error::Corrupt("migration intent provenance".into()));
                        }
                    }
                    _ => return Err(Error::Corrupt("migration endorsement".into())),
                }
                match conflict {
                    SqlValue::Null => {}
                    SqlValue::Blob(encoded) => {
                        let rival =
                            SignedMigrationPublication::from_bytes(encoded).map_err(credential)?;
                        rival
                            .verify(self.config.publisher, self.graph_for(s.scope)?, None)
                            .map_err(credential)?;
                        if rival.statement == s
                            || rival.statement.publication != s.publication
                            || rival.statement.tip != s.tip
                        {
                            return Err(Error::Corrupt("migration conflict identity".into()));
                        }
                    }
                    _ => return Err(Error::Corrupt("migration conflict record".into())),
                }
                let (mut actual_count, mut actual_bytes, mut next) = (0u64, 0u64, 0i64);
                loop {
                    let frames=collect_rows(self.conn.query("SELECT f.idx,f.bytes,f.proof,o.header,o.body FROM secure_migration_frames f JOIN secure_operations o ON o.hash=f.hash WHERE f.publication=? AND f.idx>=? ORDER BY f.idx LIMIT 256",bindings(vec![SqlValue::Blob(publication.clone()),SqlValue::Integer(next)])).await?).await?;
                    if frames.is_empty() {
                        break;
                    }
                    for frame in frames {
                        let [
                            index,
                            stored,
                            SqlValue::Blob(proof),
                            SqlValue::Blob(header),
                            SqlValue::Blob(body),
                        ] = frame.as_slice()
                        else {
                            return Err(Error::Corrupt("migration retained proof".into()));
                        };
                        let index = number(index)?;
                        let proof: MigrationProof = decode(proof)?;
                        let frame = EncryptedFrame::bare(header.clone(), body.clone());
                        if proof.index != index || number(stored)? != frame.stored_bytes()? {
                            return Err(Error::Corrupt("migration retained extent".into()));
                        }
                        self.check_migration_leaf(&s, &frame, &proof)?;
                        actual_count = actual_count
                            .checked_add(1)
                            .ok_or_else(|| denied("migration audit overflow"))?;
                        actual_bytes = actual_bytes
                            .checked_add(frame.stored_bytes()?)
                            .ok_or_else(|| denied("migration audit overflow"))?;
                        next = counter(index)?
                            .checked_add(1)
                            .ok_or_else(|| denied("migration index overflow"))?;
                    }
                }
                if actual_count != number(count)?
                    || actual_bytes != number(bytes)?
                    || actual_count > s.frames
                    || actual_bytes > s.bytes
                {
                    return Err(Error::Corrupt("migration retained counters".into()));
                }
                after = publication.clone();
            }
        }
        if !rows(&self.conn,"SELECT 1 FROM secure_migration_frames f LEFT JOIN secure_operations o ON o.hash=f.hash LEFT JOIN secure_migration_publications p ON p.publication=f.publication WHERE o.hash IS NULL OR p.publication IS NULL LIMIT 1").await?.is_empty() { return Err(Error::Corrupt("orphan migration account".into())); }
        Ok(())
    }
}
