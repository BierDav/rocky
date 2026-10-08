use inventory_app::{InventoryApp, RESERVE};
use rocky_core::{LocalRevision, Scope, ScopedOperationId, SqlValue};
use rocky_runtime::{
    BoundQuery, CommandHandle, CommandReceipt, Completeness, Error, MutationHandle,
    MutationReceipt, ReadOptions, Replica, TypedQuery,
};

/// App SDK: consumers supply domain actions and receive typed data/handles.
pub struct InventoryClient {
    pub replica: Replica<InventoryApp>,
    scope: Scope,
}
#[derive(Debug)]
pub struct StockView {
    pub remaining: u64,
    pub revision: LocalRevision,
    pub completeness: Completeness,
}
impl InventoryClient {
    pub fn new(replica: Replica<InventoryApp>, scope: Scope) -> Self {
        Self { replica, scope }
    }
    pub fn note(
        &self,
        handle: MutationHandle,
        entity: Option<ScopedOperationId>,
        text: &str,
    ) -> Result<MutationReceipt, Error> {
        self.replica
            .mutate_scoped(handle, entity, text.as_bytes().to_vec(), self.scope)
    }
    pub fn reserve(&self, handle: CommandHandle, quantity: u64) -> Result<CommandReceipt, Error> {
        self.replica.submit_command_with_arguments(
            handle,
            RESERVE,
            quantity.to_le_bytes().to_vec(),
            [9; 32],
        )
    }
    pub async fn stock(&self) -> Result<StockView, Error> {
        let result = self
            .replica
            .read_typed(&StockQuery, ReadOptions::new(self.scope, 1))
            .await?;
        Ok(StockView {
            remaining: result
                .rows
                .into_iter()
                .next()
                .ok_or_else(|| Error::Corrupt("missing stock".into()))?,
            revision: result.revision,
            completeness: result.completeness,
        })
    }
}
struct StockQuery;
impl TypedQuery for StockQuery {
    type Row = u64;
    fn query(&self) -> BoundQuery {
        BoundQuery {
            sql: "SELECT remaining FROM stock".into(),
            values: Vec::new(),
        }
    }
    fn decode(&self, rows: Vec<Vec<SqlValue>>) -> Result<Vec<u64>, Error> {
        rows.into_iter()
            .map(|row| match row.as_slice() {
                [SqlValue::Integer(quantity)] if *quantity >= 0 => Ok(*quantity as u64),
                _ => Err(Error::Corrupt("stock query shape".into())),
            })
            .collect()
    }
}
