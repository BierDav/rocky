use crate::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivery {
    Duplicate,
    Ready,
    Pending(Vec<OperationId>),
}

/// Pure applied-history helper, scoped to one domain. Durable pending storage and
/// counter allocation remain runtime responsibilities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedOperations {
    scope: Scope,
    operations: BTreeMap<OperationId, (Vec<u8>, LamportTime)>,
}
impl AppliedOperations {
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            operations: BTreeMap::new(),
        }
    }
    pub fn classify(&self, envelope: &Envelope) -> Result<Delivery, ProtocolError> {
        let bytes = envelope.encode()?;
        if envelope.scope != self.scope {
            return Err(ProtocolError::ForeignScope);
        }
        if let Some((original, _)) = self.operations.get(&envelope.id) {
            return if *original == bytes {
                Ok(Delivery::Duplicate)
            } else {
                Err(ProtocolError::IdentityConflict(envelope.id))
            };
        }
        let mut missing = Vec::new();
        for dependency in &envelope.dependencies {
            match self.operations.get(dependency) {
                Some((_, time)) if *time >= envelope.lamport => {
                    return Err(ProtocolError::InvalidLamport {
                        operation: envelope.id,
                        dependency: *dependency,
                    });
                }
                Some(_) => {}
                None => missing.push(*dependency),
            }
        }
        Ok(if missing.is_empty() {
            Delivery::Ready
        } else {
            Delivery::Pending(missing)
        })
    }
    /// Call only together with a successful application of this operation.
    pub fn record_applied(&mut self, envelope: &Envelope) -> Result<(), ProtocolError> {
        match self.classify(envelope)? {
            Delivery::Duplicate => return Ok(()),
            Delivery::Pending(ids) => return Err(ProtocolError::NotReady(ids)),
            Delivery::Ready => {}
        }
        self.operations
            .insert(envelope.id, (envelope.encode()?, envelope.lamport));
        Ok(())
    }
    /// Undo `record_applied` for an operation whose transaction rolled back.
    pub fn forget(&mut self, id: OperationId) {
        self.operations.remove(&id);
    }
    pub fn frontier(&self) -> AppliedFrontier {
        let mut frontier = BTreeMap::new();
        for id in self.operations.keys() {
            frontier.insert(id.node, id.sequence);
        }
        AppliedFrontier(frontier.into_iter().collect())
    }
    pub fn canonical_bytes(&self, id: OperationId) -> Option<&[u8]> {
        self.operations.get(&id).map(|(bytes, _)| bytes.as_slice())
    }
}
