use super::wire::Extensions;
use super::*;
use crate::scoped_connection::ScopedConnection;
use crate::storage::{bindings, collect_rows};
use p2panda_core::{Body, Header, RawOperation};
use p2panda_store::operations::LocalOperationStore;
use rocky_core::{ResourceScope, SqlValue};
use std::sync::Arc;

/// This adapter borrows the framework connection; it never opens or commits a transaction.
#[derive(Clone)]
pub(crate) struct TransactionStore(pub Arc<ScopedConnection>);
impl LocalOperationStore<ResourceScope, Extensions> for TransactionStore {
    type Error = Error;
    async fn insert_operation(
        &mut self,
        hash: Hash,
        header: &Header<Extensions>,
        body: Option<&Body>,
        header_bytes: &[u8],
        log: &ResourceScope,
    ) -> Result<bool, Error> {
        if self.0.is_autocommit()? {
            return Err(denied("operation writes require a framework transaction"));
        }
        let body = body.ok_or_else(|| denied("payload retention is required"))?;
        let frame = EncryptedFrame::bare(header_bytes.to_vec(), body.to_bytes());
        let operation = frame.operation_ref()?;
        if operation.hash != hash
            || operation.header.to_bytes() != header.to_bytes()
            || header.extensions.scope != *log
        {
            return Err(denied("operation store binding mismatch"));
        }
        if self.has_operation(hash).await? {
            return Ok(false);
        }
        // An operation is never stored without the chain it names.
        let chain = header.extensions.chain.to_vec();
        let mut kept = self
            .0
            .query(
                "SELECT 1 FROM secure_chains WHERE digest=?",
                [chain.clone()],
            )
            .await?;
        if kept.next().await?.is_none() {
            return Err(denied(crate::wire::INCOMPLETE_FRAME));
        }
        drop(kept);
        let fork = self
            .0
            .query(
                "SELECT hash FROM secure_operations WHERE author=? AND scope=? AND sequence=?",
                bindings(vec![
                    SqlValue::Blob(header.public_key.as_bytes().to_vec()),
                    SqlValue::Blob(encode(log)?),
                    SqlValue::Integer(header.seq_num as i64),
                ]),
            )
            .await?;
        if !collect_rows(fork).await?.is_empty() {
            return Err(denied("author log fork"));
        }
        self.0.execute("INSERT INTO secure_operations(hash,author,scope,sequence,header,body,status,chain) VALUES(?,?,?,?,?,?, 'pending',?)", bindings(vec![SqlValue::Blob(hash.as_bytes().to_vec()), SqlValue::Blob(header.public_key.as_bytes().to_vec()), SqlValue::Blob(encode(log)?), SqlValue::Integer(header.seq_num as i64), SqlValue::Blob(header_bytes.to_vec()), SqlValue::Blob(body.to_bytes()), SqlValue::Blob(chain)])).await?;
        crate::replica::positions::insert(&self.0, header, 0, true, false).await?;
        self.0
            .execute(
                "UPDATE secure_retained SET records=records+1,bytes=bytes+?",
                [(header_bytes.len() + body.to_bytes().len()) as i64],
            )
            .await?;
        Ok(true)
    }
    async fn get_operation(
        &self,
        hash: Hash,
    ) -> Result<Option<(Header<Extensions>, Option<Body>)>, Error> {
        self.get_raw_operation(hash)
            .await?
            .map(|(header, body)| Ok((decode(&header)?, body.map(|body| Body::new(&body)))))
            .transpose()
    }
    async fn get_raw_operation(&self, hash: Hash) -> Result<Option<RawOperation>, Error> {
        let mut rows = self
            .0
            .query(
                "SELECT header,body FROM secure_operations WHERE hash=?",
                [hash.as_bytes().to_vec()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        Ok(Some((row.get::<Vec<u8>>(0)?, Some(row.get::<Vec<u8>>(1)?))))
    }
    async fn has_operation(&self, hash: Hash) -> Result<bool, Error> {
        Ok(self.get_raw_operation(hash).await?.is_some())
    }
    async fn delete_operation(&mut self, _hash: Hash) -> Result<bool, Error> {
        Err(denied("retained recovery history cannot be deleted"))
    }
    async fn delete_payload(&mut self, _hash: Hash) -> Result<bool, Error> {
        Err(denied("retained encrypted payload cannot be deleted"))
    }
}

pub(crate) const SCHEMA: &[&str] = &[
    "CREATE TABLE secure_metadata(id INTEGER PRIMARY KEY CHECK(id=1),identity BLOB NOT NULL,revocations BLOB NOT NULL,revision INTEGER NOT NULL)",
    "CREATE TABLE secure_operations(hash BLOB PRIMARY KEY,author BLOB NOT NULL,scope BLOB NOT NULL,sequence INTEGER NOT NULL,header BLOB NOT NULL,body BLOB NOT NULL,status TEXT NOT NULL,chain BLOB NOT NULL,UNIQUE(author,scope,sequence))",
    "CREATE TABLE secure_rows(scope INTEGER NOT NULL,row BLOB NOT NULL,fields BLOB NOT NULL,live INTEGER GENERATED ALWAYS AS (substr(fields,1,1)=X'01') VIRTUAL,shape BLOB GENERATED ALWAYS AS (substr(fields,2,CASE hex(substr(fields,2,1)) WHEN '18' THEN 2 WHEN '19' THEN 3 WHEN '1A' THEN 5 WHEN '1B' THEN 9 ELSE 1 END)) VIRTUAL,PRIMARY KEY(scope,row)) WITHOUT ROWID",
    "CREATE INDEX secure_rows_dead ON secure_rows(scope,row) WHERE live=0",
    "CREATE TABLE secure_outbox(hash BLOB PRIMARY KEY)",
    "CREATE TABLE secure_handles(handle BLOB PRIMARY KEY,input BLOB NOT NULL,hash BLOB NOT NULL)",
    "CREATE TABLE secure_keys(scope BLOB PRIMARY KEY,generation BLOB NOT NULL,key_hash BLOB NOT NULL)",
    "CREATE TABLE secure_ack(peer BLOB NOT NULL,hash BLOB NOT NULL,applied INTEGER NOT NULL,PRIMARY KEY(peer,hash))",
];
