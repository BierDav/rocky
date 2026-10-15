use inventory_app::InventoryApp;
use rocky_core::{LocalRevision, Scope, ScopedOperationId, SqlValue};
use rocky_runtime::{
    BoundQuery, Completeness, Error, MutationHandle, MutationReceipt, ReadOptions, Replica,
    TypedQuery,
};

/// App SDK: consumers supply domain actions and receive typed data/handles.
pub struct InventoryClient {
    pub replica: Replica<InventoryApp>,
    scope: Scope,
}
#[derive(Debug)]
pub struct NotesView {
    pub notes: Vec<String>,
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
    pub async fn notes(&self) -> Result<NotesView, Error> {
        let result = self
            .replica
            .read_typed(&NotesQuery, ReadOptions::new(self.scope, 128))
            .await?;
        Ok(NotesView {
            notes: result.rows,
            revision: result.revision,
            completeness: result.completeness,
        })
    }
}
struct NotesQuery;
impl TypedQuery for NotesQuery {
    type Row = String;
    fn query(&self) -> BoundQuery {
        BoundQuery {
            sql: "SELECT note FROM drafts ORDER BY node,sequence".into(),
            values: Vec::new(),
        }
    }
    fn decode(&self, rows: Vec<Vec<SqlValue>>) -> Result<Vec<String>, Error> {
        rows.into_iter()
            .map(|row| match row.as_slice() {
                [SqlValue::Text(note)] => Ok(note.clone()),
                _ => Err(Error::Corrupt("notes query shape".into())),
            })
            .collect()
    }
}
