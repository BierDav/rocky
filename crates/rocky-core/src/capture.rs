//! Evidence of one SQL transaction: the rows it changed, the operations an
//! application derives from them and the token that recovers its outcome.
//! See `docs/runtime/sql-sessions.md`.
use crate::{Application, Commitment, OperationId, ProjectionStatement, SqlValue};

/// Chosen by the caller before a commit. The runtime stores it with the
/// commit, so a caller that lost the acknowledgment can ask for the outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TransactionToken(pub Commitment);

/// One row a transaction changed, as its first and its final state. Values
/// are in the table's declared column order and in stored form.
#[derive(Clone, Debug, PartialEq)]
pub struct CapturedRow {
    /// SQL name of the logical table.
    pub table: String,
    /// The row when the transaction began; None if the key held none.
    pub before: Option<Vec<SqlValue>>,
    /// The row when the transaction ended; None if it was deleted.
    pub after: Option<Vec<SqlValue>>,
    /// Per column: an update of this row assigned it, equal value or not.
    pub assigned: Vec<bool>,
    /// The key was deleted and inserted again, as `INSERT OR REPLACE` does.
    pub reincarnated: bool,
}

/// A precise application action for one captured row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intent {
    /// None targets the operation this intent becomes.
    pub entity: Option<OperationId>,
    pub payload: Vec<u8>,
}

/// Turns captured rows into operations. The runtime executes the SQL once,
/// on the author's snapshot; peers receive these values, never the SQL.
pub trait SqlWrites<A: Application> {
    /// Author number and a one-column query of the own creation-log maximum.
    fn identity_allocation(
        &self,
        _table: &str,
        _node: crate::NodeId,
    ) -> Result<Option<(u32, ProjectionStatement)>, A::Error> {
        Ok(None)
    }
    /// Partition values follow the declaration order; unpartitioned adapters
    /// retain their existing allocation contract.
    fn identity_allocation_partition(
        &self,
        table: &str,
        node: crate::NodeId,
        values: &[SqlValue],
    ) -> Result<Option<(u32, ProjectionStatement)>, A::Error> {
        if values.is_empty() {
            self.identity_allocation(table, node)
        } else {
            Ok(None)
        }
    }
    /// `SELECT`s, in `projection_query` row shape, for the stored rows
    /// `intent` needs to translate `row`.
    fn located(&self, row: &CapturedRow) -> Result<Vec<ProjectionStatement>, A::Error>;
    /// The action that takes `row.before` to `row.after`. None publishes
    /// nothing: no replicated column differs and none must be republished.
    fn intent(&self, state: &A::State, row: &CapturedRow) -> Result<Option<Intent>, A::Error>;
}

/// Commits to the frozen rows of a transaction, in the order given.
pub fn intent_commitment(rows: &[CapturedRow]) -> Commitment {
    let mut bytes = (rows.len() as u64).to_le_bytes().to_vec();
    let image = |bytes: &mut Vec<u8>, image: &Option<Vec<SqlValue>>| match image {
        None => bytes.push(0),
        Some(values) => {
            bytes.push(1);
            bytes.extend((values.len() as u32).to_le_bytes());
            for value in values {
                value.encode(bytes);
            }
        }
    };
    for row in rows {
        bytes.extend((row.table.len() as u32).to_le_bytes());
        bytes.extend(row.table.as_bytes());
        image(&mut bytes, &row.before);
        image(&mut bytes, &row.after);
        bytes.extend((row.assigned.len() as u32).to_le_bytes());
        bytes.extend(row.assigned.iter().map(|assigned| u8::from(*assigned)));
        bytes.push(u8::from(row.reincarnated));
    }
    crate::commitment("rocky.sql.intent.v1", &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_intent_commitment_covers_images_mask_and_incarnation() {
        let row = CapturedRow {
            table: "items".into(),
            before: None,
            after: Some(vec![SqlValue::Integer(1), SqlValue::Text("a".into())]),
            assigned: vec![false, true],
            reincarnated: false,
        };
        let base = intent_commitment(std::slice::from_ref(&row));
        let variants = [
            CapturedRow {
                before: Some(Vec::new()),
                ..row.clone()
            },
            CapturedRow {
                after: None,
                ..row.clone()
            },
            CapturedRow {
                assigned: vec![true, true],
                ..row.clone()
            },
            CapturedRow {
                reincarnated: true,
                ..row.clone()
            },
            CapturedRow {
                table: "item".into(),
                ..row.clone()
            },
        ];
        for variant in variants {
            assert_ne!(intent_commitment(&[variant]), base);
        }
        assert_ne!(intent_commitment(&[row.clone(), row.clone()]), base);
        assert_eq!(intent_commitment(&[row]), base);
    }
}
