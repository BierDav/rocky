//! Held changes: local changes a newer generation's removal holds back,
//! listed for the application to re-author or drop. See
//! `docs/runtime/held-changes.md`.
use super::*;
use rocky_secure::{HeldChange, HeldKind};

/// A held change's id as the JSON contract spells it.
pub(super) fn held_id(request: &Value, name: &str) -> Result<rocky_core::Commitment, Failure> {
    let hex = request[name]
        .as_str()
        .ok_or_else(|| refused(format!("{name} must be a held change id")))?;
    HEXLOWER
        .decode(hex.as_bytes())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| refused(format!("{name} must be a held change id")))
}
/// An encoded field value; a value that is not one SQL value is a blob.
fn cell(bytes: &[u8]) -> Cell {
    let mut slice = bytes;
    match SqlValue::decode(&mut slice) {
        Ok(value) if slice.is_empty() => Cell::from(value),
        _ => Cell::Blob(bytes.to_vec()),
    }
}
fn change(change: &HeldChange) -> Value {
    json!({
        "id": HEXLOWER.encode(&change.id),
        "time": change.time,
        "rows": change.rows.iter().map(|row| json!({
            "table": row.table,
            "removed": row.removed,
            "row": HEXLOWER.encode(&row.row),
            "kind": match row.kind {
                HeldKind::Insert => "insert",
                HeldKind::Update => "update",
                HeldKind::Delete => "delete",
            },
            "fields": row.fields.iter().map(|field| json!({
                "name": field.name,
                "value": cell(&field.value),
                "written": field.written,
                "removed": field.removed,
                "current": field.current.as_ref().map(|current| json!({
                    "value": current.value.as_deref().map(cell),
                    "time": current.time,
                    "author": current.author.to_hex(),
                })),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

impl Node {
    /// Every held change, with what re-authoring each would replace.
    pub(super) async fn held_changes(&self) -> Result<Value, Failure> {
        let changes = self.replica.held_changes().await?;
        Ok(json!({
            "count": changes.len(),
            "changes": changes.iter().map(change).collect::<Vec<_>>(),
        }))
    }
    /// Drops one held change; it is never sent.
    pub(super) async fn drop_held(&self, request: &Value) -> Result<Value, Failure> {
        self.replica
            .drop_held(held_id(request, "id")?)
            .await
            .map_err(refused)?;
        self.observations.touch();
        self.held_changes().await
    }
}
