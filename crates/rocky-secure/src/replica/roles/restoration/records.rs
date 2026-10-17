use super::*;

impl<A: RowApplication> Inner<A> {
    pub(super) async fn restoration_header(
        &self,
        scope: ResourceScope,
        id: i64,
        sequence: i64,
        expected: Hash,
    ) -> Result<Option<Header<Extensions>>, Error> {
        let node = self.config.node.public_key();
        let mut records = collect_rows(self.conn.query(
            "SELECT hash,scope,author,sequence,header,body,chain,status,(SELECT scope FROM secure_applied a WHERE a.hash=o.hash) FROM secure_operations o WHERE hash=?",
            [expected.as_bytes().to_vec()],
        ).await?).await?;
        if records.is_empty() {
            records = collect_rows(self.conn.query(
                "SELECT hash,scope,author,sequence,header,body,chain,status,(SELECT scope FROM secure_applied a WHERE a.hash=o.hash) FROM secure_operations o WHERE scope=? AND author=? AND sequence=?",
                bindings(vec![SqlValue::Blob(encode(&scope)?), SqlValue::Blob(node.as_bytes().to_vec()), SqlValue::Integer(sequence)]),
            ).await?).await?;
        }
        if let Some(record) = records.first() {
            let [
                SqlValue::Blob(hash),
                SqlValue::Blob(stored_scope),
                SqlValue::Blob(author),
                SqlValue::Integer(at),
                SqlValue::Blob(header),
                SqlValue::Blob(body),
                SqlValue::Blob(chain),
                SqlValue::Text(status),
                applied,
            ] = record.as_slice()
            else {
                return Err(Error::Corrupt("restoration operation record".into()));
            };
            let frame = self.stored_frame(header.clone(), body.clone()).await?;
            let operation = frame.operation_ref()?;
            let header = &operation.header;
            if records.len() != 1
                || operation.hash.as_bytes().as_slice() != hash
                || *stored_scope != encode(&scope)?
                || *author != node.as_bytes()
                || *at != sequence
                || header.extensions.scope != scope
                || header.public_key != node
                || header.seq_num != sequence as u64
                || *chain != header.extensions.chain
            {
                return Err(Error::Corrupt("restoration operation binding".into()));
            }
            self.restoration_source(header)?;
            let bindings: Vec<_> = frame
                .chain()?
                .credentials()
                .0
                .iter()
                .map(|link| link.author_binding.clone())
                .collect();
            rocky_credentials::AuthorBinding::verify_chain(
                &bindings,
                self.config.publisher,
                scope.table,
                header
                    .extensions
                    .author
                    .ok_or_else(|| Error::Corrupt("restoration author number".into()))?,
                node,
            )
            .map_err(credential)?;
            match (status.as_str(), applied) {
                ("applied", SqlValue::Integer(scope_id)) if *scope_id == id => {}
                ("held" | "abandoned", SqlValue::Null) => {}
                ("held" | "abandoned", SqlValue::Integer(scope_id)) if *scope_id == id => {}
                ("pending" | "rejected", SqlValue::Null) => return Ok(None),
                _ => return Err(Error::Corrupt("restoration applied binding".into())),
            }
            return Ok((operation.hash == expected).then(|| header.clone()));
        }
        let records = collect_rows(self.conn.query(
            "SELECT v.scope,v.author,v.sequence,v.hash,v.header,v.record,r.record FROM secure_voids v LEFT JOIN secure_void_records r ON r.digest=v.record WHERE v.hash=? OR (v.scope=? AND v.author=? AND v.sequence=?)",
            bindings(vec![SqlValue::Blob(expected.as_bytes().to_vec()), SqlValue::Blob(encode(&scope)?), SqlValue::Blob(node.as_bytes().to_vec()), SqlValue::Integer(sequence)]),
        ).await?).await?;
        let Some(record) = records.first() else {
            return Ok(None);
        };
        let [
            SqlValue::Blob(stored_scope),
            SqlValue::Blob(author),
            SqlValue::Integer(at),
            SqlValue::Blob(hash),
            SqlValue::Blob(header),
            SqlValue::Blob(digest),
            SqlValue::Blob(bytes),
        ] = record.as_slice()
        else {
            return Err(Error::Corrupt("restoration void record".into()));
        };
        let record: held::SignedVoid = decode(bytes)?;
        let members = record.verify()?;
        if records.len() != 1
            || record.digest()?.as_slice() != digest
            || *stored_scope != encode(&scope)?
            || *author != node.as_bytes()
            || *at != sequence
        {
            return Err(Error::Corrupt("restoration void binding".into()));
        }
        let Some((actual, parsed, _)) = members.into_iter().find(|(actual, parsed, bytes)| {
            actual.as_bytes().as_slice() == hash
                && parsed.seq_num == sequence as u64
                && bytes == header
        }) else {
            return Err(Error::Corrupt("restoration void membership".into()));
        };
        if parsed.public_key != node || parsed.extensions.scope != scope {
            return Err(Error::Corrupt("restoration void owner/scope".into()));
        }
        self.restoration_source(&parsed)?;
        Ok((actual == expected).then_some(parsed))
    }

    fn restoration_source(&self, header: &Header<Extensions>) -> Result<(), Error> {
        let scope = header.extensions.scope;
        let manifest = self
            .graph_for(scope.domain)?
            .schema_any(header.extensions.tip)
            .map_err(credential)?;
        let source = self
            .catalog
            .get(&manifest)
            .ok_or_else(|| Error::Corrupt("restoration source schema missing".into()))?;
        if scope.domain.app != self.definition.app
            || !self.domains.contains(&scope.domain.domain)
            || !source.entities.iter().any(|table| table.id == scope.table)
        {
            return Err(Error::Corrupt("restoration source binding".into()));
        }
        // A removed table may have no current descriptor; its registered
        // concrete address still requires the original authenticated tuple.
        if header.extensions.partition.is_none() && self.partitions.contains_key(&scope) {
            return Err(Error::Corrupt("restoration partition tuple missing".into()));
        }
        self.check_authenticated_partition(scope, header.extensions.partition.as_ref())
    }
}
