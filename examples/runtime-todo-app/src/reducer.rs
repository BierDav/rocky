use crate::{Action, AdmissionBoundary, Error, StaticPolicy, TodoApp};
use rocky_core::{AppliedOperations, Delivery, Envelope, OperationId, Stamp};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Register<T> {
    pub value: T,
    pub stamp: Stamp,
}
impl<T> Register<T> {
    fn set(&mut self, value: T, stamp: Stamp) {
        if stamp > self.stamp {
            *self = Self { value, stamp };
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TodoEntry {
    Live {
        title: Register<String>,
        completed: Register<bool>,
    },
    /// Discard hidden registers so delete/edit delivery orders retain equal state.
    Tombstone,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TodoState {
    pub(crate) entries: BTreeMap<OperationId, TodoEntry>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibleRow {
    pub id: OperationId,
    pub title: String,
    pub completed: bool,
}
impl TodoState {
    pub fn entry(&self, id: OperationId) -> Option<&TodoEntry> {
        self.entries.get(&id)
    }
    pub fn visible_rows(&self) -> Vec<VisibleRow> {
        self.entries
            .iter()
            .filter_map(|(id, entry)| match entry {
                TodoEntry::Live { title, completed } => Some(VisibleRow {
                    id: *id,
                    title: title.value.clone(),
                    completed: completed.value,
                }),
                TodoEntry::Tombstone => None,
            })
            .collect()
    }
    pub(crate) fn reduce(&mut self, envelope: &Envelope, action: Action) -> Result<(), Error> {
        let stamp = Stamp::from(envelope);
        if let Action::CreateTodo { title, completed } = action {
            self.entries
                .entry(envelope.entity)
                .or_insert(TodoEntry::Live {
                    title: Register {
                        value: title,
                        stamp,
                    },
                    completed: Register {
                        value: completed,
                        stamp,
                    },
                });
            return Ok(());
        }
        let entry = self
            .entries
            .get_mut(&envelope.entity)
            .ok_or(Error::MissingCreate(envelope.entity))?;
        if let TodoEntry::Live { title, completed } = entry {
            match action {
                Action::SetTitle(value) => title.set(value, stamp),
                Action::SetCompleted(value) => completed.set(value, stamp),
                Action::DeleteTodo => *entry = TodoEntry::Tombstone,
                Action::CreateTodo { .. } => {
                    return Err(rocky_core::ProtocolError::InvalidDependencies.into());
                }
            }
        }
        Ok(())
    }
}
impl TodoApp {
    /// Shared local/incoming gateway. Pending operations do not change either
    /// argument. Runtime transactions must durably retain pending bytes separately.
    pub fn apply_ready(
        &self,
        state: &mut TodoState,
        history: &mut AppliedOperations,
        envelope: &Envelope,
        policy: &StaticPolicy,
        boundary: AdmissionBoundary,
    ) -> Result<Delivery, Error> {
        let action = self.validate(envelope)?;
        policy.admit(envelope, &action)?;
        let delivery = history.classify(envelope)?;
        if delivery != Delivery::Ready {
            return Ok(delivery);
        }
        rocky_core::Application::apply_ready(self, state, envelope, boundary)?;
        history.record_applied(envelope)?;
        Ok(Delivery::Ready)
    }
}
