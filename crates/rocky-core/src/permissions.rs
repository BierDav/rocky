use crate::{Commitment, Scope};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// An encryption/replication boundary, independent of the invariant domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ResourceScope {
    pub domain: Scope,
    pub encryption: Commitment,
    pub table: Commitment,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrincipalKind {
    User,
    Deployment,
    Node,
    /// An author pseudonym: a signing key and author number for one table.
    Pseudonym,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Right {
    Read,
    Update,
    Insert,
    Delete,
    Invoke,
    Execute,
    Custody,
    PublishSchema,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RowSelection {
    All,
    Only(BTreeSet<Commitment>),
}
impl RowSelection {
    pub fn contains(&self, row: &Commitment) -> bool {
        match self {
            Self::All => true,
            Self::Only(rows) => rows.contains(row),
        }
    }
    pub fn includes(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::All, _) => true,
            (Self::Only(a), Self::Only(b)) => b.is_subset(a),
            _ => false,
        }
    }
}

/// Fields and rows stay paired inside each grant; permissions never form a cross product.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceGrant {
    pub scope: ResourceScope,
    pub rows: RowSelection,
    pub rights: BTreeSet<Right>,
    pub fields: BTreeSet<Commitment>,
}
impl ResourceGrant {
    pub fn includes(&self, other: &Self) -> bool {
        self.scope == other.scope
            && self.rows.includes(&other.rows)
            && other.rights.is_subset(&self.rights)
            && other.fields.is_subset(&self.fields)
    }
    pub fn permits(&self, scope: ResourceScope, row: Commitment, right: Right) -> bool {
        self.scope == scope && self.rows.contains(&row) && self.rights.contains(&right)
    }
}

pub type RowFields = BTreeMap<Commitment, Vec<u8>>;

/// Exact before/after images are encrypted with the row, including generated changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowChange {
    pub scope: ResourceScope,
    pub row: Commitment,
    pub before: Option<RowFields>,
    pub after: Option<RowFields>,
}
impl RowChange {
    pub fn right(&self) -> Option<Right> {
        match (&self.before, &self.after) {
            (None, Some(_)) => Some(Right::Insert),
            (Some(_), None) => Some(Right::Delete),
            (Some(before), Some(after)) if before != after => Some(Right::Update),
            _ => None,
        }
    }
    pub fn changed_fields(&self) -> BTreeSet<Commitment> {
        let empty = RowFields::new();
        let before = self.before.as_ref().unwrap_or(&empty);
        let after = self.after.as_ref().unwrap_or(&empty);
        before
            .keys()
            .chain(after.keys())
            .filter(|key| before.get(*key) != after.get(*key))
            .copied()
            .collect()
    }
    pub fn permitted_by(&self, grants: &[ResourceGrant]) -> bool {
        self.right()
            .is_some_and(|right| self.permitted_fields(right, &self.changed_fields(), grants))
    }
    /// One grant on this row holds `right`, and every field has a grant with
    /// `right` on this same row. A delete names no fields.
    fn permitted_fields(
        &self,
        right: Right,
        fields: &BTreeSet<Commitment>,
        grants: &[ResourceGrant],
    ) -> bool {
        let on_row = |grant: &&ResourceGrant| grant.permits(self.scope, self.row, right);
        grants.iter().any(|grant| on_row(&grant))
            && (right == Right::Delete
                || fields.iter().all(|field| {
                    grants
                        .iter()
                        .filter(on_row)
                        .any(|grant| grant.fields.contains(field))
                }))
    }
}

/// A row change with its write mask: the fields an update republishes
/// although their value is unchanged, as the all-assigned update policy
/// requires. Every published field needs a paired row/field grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowWrite {
    pub change: RowChange,
    pub assigned: BTreeSet<Commitment>,
}
impl RowWrite {
    /// Publishes only the values that differ.
    pub fn changed(change: RowChange) -> Self {
        Self {
            change,
            assigned: BTreeSet::new(),
        }
    }
    /// A mask is valid only on an update and only for fields the after image holds.
    pub fn valid(&self) -> bool {
        self.assigned.is_empty()
            || matches!(
                (&self.change.before, &self.change.after),
                (Some(_), Some(after)) if self.assigned.iter().all(|field| after.contains_key(field))
            )
    }
    pub fn right(&self) -> Option<Right> {
        match self.change.right() {
            None if self.valid() && !self.assigned.is_empty() => Some(Right::Update),
            right => right,
        }
    }
    /// Changed fields and assigned fields: everything this write stamps.
    pub fn published_fields(&self) -> BTreeSet<Commitment> {
        let mut fields = self.change.changed_fields();
        fields.extend(self.assigned.iter().copied());
        fields
    }
    pub fn permitted_by(&self, grants: &[ResourceGrant]) -> bool {
        self.valid()
            && self.right().is_some_and(|right| {
                self.change
                    .permitted_fields(right, &self.published_fields(), grants)
            })
    }
}

/// Installed application code validates old/new rows and identifies ACL fields.
/// The runtime applies the supplied precise changes only after the entire batch passes.
pub trait RowApplication: Send + Sync + 'static {
    /// Immutable compiled definition, verified and cached when a replica opens.
    fn manifest(&self) -> crate::ApplicationManifest;
    /// Immutable ACL-controlling fields of this definition; the runtime may cache them.
    fn permission_fields(&self) -> BTreeSet<Commitment>;
    fn validate_changes(&self, changes: &[RowChange]) -> Result<(), String>;
    /// Optional partition identity of an application row.
    fn row_partition(&self, _change: &RowChange) -> Result<Option<crate::PartitionTuple>, String> {
        Ok(None)
    }
    fn row_identity(&self, _change: &RowChange) -> Result<Option<crate::RowIdentity>, String> {
        Ok(None)
    }
    /// Checks a write's mask beyond its images, such as fields that must
    /// publish together.
    fn validate_write(&self, _write: &RowWrite) -> Result<(), String> {
        Ok(())
    }
}

/// Strong row state is separate from mergeable field registers.
pub type CommandRows = BTreeMap<ResourceScope, BTreeMap<Commitment, RowFields>>;

#[derive(Clone, Debug)]
pub struct RowCommandOutput {
    pub changes: Vec<RowChange>,
    /// Application-owned outputs must retain their reader scope.
    pub outputs: Vec<ScopedCommandOutput>,
}

#[derive(Clone, Copy, Debug)]
pub enum CommandOutputKind {
    Result,
    Job,
    Blob,
    Checkpoint,
}

#[derive(Clone, Debug)]
pub struct ScopedCommandOutput {
    pub scope: ResourceScope,
    pub kind: CommandOutputKind,
    pub bytes: Vec<u8>,
}

/// Installed deterministic handlers declare the complete invariant-relevant reader scopes.
pub trait RowCommandApplication: RowApplication {
    fn command_resources(&self, scope: Scope, method: &str) -> Result<Vec<ResourceScope>, String>;
    fn command_invocation_scope(&self, scope: Scope, method: &str)
    -> Result<ResourceScope, String>;
    fn execute_command(
        &self,
        method: &str,
        arguments: &[u8],
        state: &CommandRows,
    ) -> Result<RowCommandOutput, String>;
    /// Whether requests of this version are admitted. Byte-oriented methods
    /// without a descriptor stay at version 1.
    fn supports_version(&self, _method: &str, version: u32) -> bool {
        version == 1
    }
    /// Checks plaintext arguments before they are sealed, and again after
    /// an executor opened them.
    fn validate_arguments(
        &self,
        _method: &str,
        _version: u32,
        _arguments: &[u8],
    ) -> Result<(), String> {
        Ok(())
    }
    /// Checks the row changes of a confirmed result. Unlike row writes,
    /// they may change tables only commands control.
    fn validate_confirmed(&self, changes: &[RowChange]) -> Result<(), String> {
        self.validate_changes(changes)
    }
    /// Executes against the predecessor in `call`. The runtime calls this
    /// outside its transactions and locks and checks the predecessor again
    /// before it prepares the output.
    fn run_command<'a>(&'a self, call: crate::CommandCall<'a>) -> crate::CommandOutputFuture<'a> {
        let output = self
            .execute_command(call.method, call.arguments, call.state)
            .map_err(crate::CommandFailure::Invalid);
        Box::pin(std::future::ready(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppId, DomainId, MeshId};

    fn scope() -> ResourceScope {
        ResourceScope {
            domain: Scope {
                mesh: MeshId([1; 16]),
                app: AppId([2; 16]),
                domain: DomainId([3; 16]),
            },
            encryption: [4; 32],
            table: [5; 32],
        }
    }
    fn grant(rights: &[Right], fields: &[u8]) -> ResourceGrant {
        ResourceGrant {
            scope: scope(),
            rows: RowSelection::All,
            rights: rights.iter().copied().collect(),
            fields: fields.iter().map(|field| [*field; 32]).collect(),
        }
    }
    fn fields(values: &[(u8, &str)]) -> RowFields {
        values
            .iter()
            .map(|(field, value)| ([*field; 32], value.as_bytes().to_vec()))
            .collect()
    }

    #[test]
    fn an_assigned_mask_needs_rights_to_every_republished_field() {
        let change = RowChange {
            scope: scope(),
            row: [9; 32],
            before: Some(fields(&[(1, "a"), (2, "b")])),
            after: Some(fields(&[(1, "c"), (2, "b")])),
        };
        let changed = RowWrite::changed(change.clone());
        let assigned = RowWrite {
            change: change.clone(),
            assigned: [[2; 32]].into(),
        };
        let narrow = [grant(&[Right::Update], &[1])];
        assert!(changed.permitted_by(&narrow));
        assert!(!assigned.permitted_by(&narrow));
        assert!(assigned.permitted_by(&[grant(&[Right::Update], &[1, 2])]));
        assert_eq!(assigned.published_fields(), [[1; 32], [2; 32]].into());

        // An equal row with a mask is an update; without one it is nothing.
        let equal = RowChange {
            after: change.before.clone(),
            ..change.clone()
        };
        assert_eq!(RowWrite::changed(equal.clone()).right(), None);
        let republish = RowWrite {
            change: equal,
            assigned: [[2; 32]].into(),
        };
        assert_eq!(republish.right(), Some(Right::Update));
        assert!(republish.permitted_by(&[grant(&[Right::Update], &[2])]));

        // Masks belong to updates, and to fields the after image holds.
        for invalid in [
            RowWrite {
                change: RowChange {
                    before: None,
                    ..change.clone()
                },
                assigned: [[1; 32]].into(),
            },
            RowWrite {
                change: change.clone(),
                assigned: [[7; 32]].into(),
            },
        ] {
            assert!(!invalid.valid());
            assert!(!invalid.permitted_by(&[grant(&[Right::Update, Right::Insert], &[1, 2, 7])]));
        }
        // Reading or holding custody is not a write right.
        assert!(!changed.permitted_by(&[grant(
            &[
                Right::Read,
                Right::Custody,
                Right::Invoke,
                Right::Execute,
                Right::PublishSchema
            ],
            &[1, 2],
        )]));
    }
}
