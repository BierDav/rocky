use crate::{AdmissionBoundary, Error, StaticPolicy, TodoApp, TodoState};
use rocky_core::{AppliedOperations, Delivery, Envelope, ProtocolError};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replay {
    pub state: TodoState,
    pub applied: AppliedOperations,
}

/// Canonical topological replay. Duplicate bytes collapse; conflicting identities
/// and unresolved dependencies are errors, never a reported complete projection.
pub fn replay(
    operations: impl IntoIterator<Item = Envelope>,
    policy: &StaticPolicy,
) -> Result<Replay, Error> {
    let app = TodoApp;
    let mut pending = BTreeMap::new();
    for operation in operations {
        let action = app.validate(&operation)?;
        policy.admit(&operation, &action)?;
        let bytes = operation.encode()?;
        if let Some((_, original)) = pending.get(&operation.id) {
            if *original != bytes {
                return Err(ProtocolError::IdentityConflict(operation.id).into());
            }
        } else {
            pending.insert(operation.id, (operation, bytes));
        }
    }
    for (operation, _) in pending.values() {
        for dependency in &operation.dependencies {
            if let Some((predecessor, _)) = pending.get(dependency)
                && predecessor.lamport >= operation.lamport
            {
                return Err(ProtocolError::InvalidLamport {
                    operation: operation.id,
                    dependency: *dependency,
                }
                .into());
            }
        }
    }
    let mut result = Replay {
        state: TodoState::default(),
        applied: AppliedOperations::new(policy.scope),
    };
    while !pending.is_empty() {
        let mut progress = false;
        let ids: Vec<_> = pending.keys().copied().collect();
        for id in ids {
            if let Some((operation, _)) = pending.get(&id) {
                match app.apply_ready(
                    &mut result.state,
                    &mut result.applied,
                    operation,
                    policy,
                    AdmissionBoundary::Incoming,
                )? {
                    Delivery::Pending(_) => {}
                    Delivery::Ready | Delivery::Duplicate => {
                        pending.remove(&id);
                        progress = true;
                    }
                }
            }
        }
        if !progress {
            return Err(Error::UnresolvedDependencies(pending.into_keys().collect()));
        }
    }
    Ok(result)
}
