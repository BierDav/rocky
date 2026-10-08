use crate::{Action, Error};
use rocky_core::{Envelope, NodeId, PrincipalId, Scope, fixtures};

/// Trusted development input, not an authenticated capability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticPolicy {
    pub scope: Scope,
    pub caller: PrincipalId,
    pub nodes: Vec<NodeId>,
    pub read: bool,
    pub publish: bool,
    pub actions: [bool; 4],
    pub title: bool,
    pub completed: bool,
    pub version: u64,
    pub rows: Option<Vec<RowGrant>>,
}
impl Default for StaticPolicy {
    fn default() -> Self {
        Self {
            scope: fixtures::SCOPE,
            caller: fixtures::OWNER,
            nodes: vec![fixtures::LAPTOP, fixtures::PHONE],
            read: true,
            publish: true,
            actions: [true; 4],
            title: true,
            completed: true,
            version: 1,
            rows: None,
        }
    }
}
impl StaticPolicy {
    pub(crate) fn admit(&self, envelope: &Envelope, action: &Action) -> Result<(), Error> {
        envelope.validate()?;
        if envelope.scope != self.scope {
            return Err(Error::DeniedScope);
        }
        if envelope.caller != self.caller {
            return Err(Error::DeniedCaller);
        }
        if !self.nodes.contains(&envelope.id.node) {
            return Err(Error::DeniedNode);
        }
        if !self.actions[action.kind() as usize] {
            return Err(Error::DeniedAction(action.kind()));
        }
        for field in action.fields() {
            if (*field == crate::TITLE_FIELD_ID && !self.title)
                || (*field == crate::COMPLETED_FIELD_ID && !self.completed)
            {
                return Err(Error::DeniedField(*field));
            }
        }
        if let Some(grants) = &self.rows {
            let grant = grants
                .iter()
                .find(|grant| grant.entity == envelope.entity)
                .ok_or(Error::DeniedRead)?;
            if !grant.read || (matches!(action, Action::DeleteTodo) && !grant.delete) {
                return Err(Error::DeniedRead);
            }
            for field in action.fields() {
                if !grant.fields.contains(field) {
                    return Err(Error::DeniedField(*field));
                }
            }
        }
        Ok(())
    }
    pub fn admit_read(&self, scope: Scope, caller: PrincipalId) -> Result<(), Error> {
        if scope != self.scope {
            return Err(Error::DeniedScope);
        }
        if caller != self.caller {
            return Err(Error::DeniedCaller);
        }
        if !self.read {
            return Err(Error::DeniedRead);
        }
        Ok(())
    }
}

/// A grant pairs a row with its permitted fields; grants do not cross rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowGrant {
    pub entity: rocky_core::OperationId,
    pub read: bool,
    pub fields: Vec<u32>,
    pub delete: bool,
}
