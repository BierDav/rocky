use crate::*;
use rocky_core::ScopedOperationId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerEvidence {
    pub peer: NodeId,
    pub received: bool,
    pub applied: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MutationProgress {
    pub handle: MutationHandle,
    pub committed: Option<ScopedOperationId>,
    pub peers: Vec<PeerEvidence>,
}
impl<A> Replica<A>
where
    A: StoredApplication,
    A::State: Clone,
    A::Policy: Send + 'static,
{
    pub fn mutate_scoped(
        &self,
        handle: MutationHandle,
        entity: Option<ScopedOperationId>,
        payload: Vec<u8>,
        scope: Scope,
    ) -> Result<MutationReceipt, Error> {
        if scope != self.inner.scope || entity.is_some_and(|entity| entity.scope != scope) {
            return Err(rocky_core::ProtocolError::ForeignScope.into());
        }
        self.mutate_with_handle(
            handle,
            Mutation {
                entity: entity.map(|entity| entity.operation),
                payload,
            },
        )
    }
    pub async fn track_mutation(
        &self,
        handle: MutationHandle,
        peers: &[NodeId],
    ) -> Result<MutationProgress, Error> {
        check_peers(peers)?;
        let committed = self
            .lookup_mutation(handle)
            .await?
            .map(|operation| operation.scoped_id());
        let mut evidence = Vec::new();
        for peer in peers {
            let acknowledgments = self.peer_acknowledgments(*peer).await?;
            let ack =
                committed.and_then(|id| acknowledgments.iter().find(|ack| ack.id == id.operation));
            evidence.push(PeerEvidence {
                peer: *peer,
                received: ack.is_some(),
                applied: ack.is_some_and(|ack| ack.applied),
            });
        }
        Ok(MutationProgress {
            handle,
            committed,
            peers: evidence,
        })
    }
}

pub struct ProgressObserver<A: StoredApplication> {
    observer: Observer<A::Row>,
    changes: watch::Receiver<u64>,
    handle: MutationHandle,
    peers: Vec<NodeId>,
    delivered: Option<u64>,
}
impl<A> Replica<A>
where
    A: StoredApplication,
    A::State: Clone,
    A::Policy: Send + 'static,
{
    pub fn observe_progress(
        &self,
        handle: MutationHandle,
        peers: Vec<NodeId>,
    ) -> Result<ProgressObserver<A>, Error> {
        check_peers(&peers)?;
        Ok(ProgressObserver {
            observer: self.observe()?,
            changes: self.inner.changes.clone(),
            handle,
            peers,
            delivered: None,
        })
    }
}
impl<A> ProgressObserver<A>
where
    A: StoredApplication,
    A::State: Clone,
    A::Policy: Send + 'static,
{
    pub async fn next(&mut self) -> Result<MutationProgress, Error> {
        loop {
            let current = *self.changes.borrow_and_update();
            if self.delivered.is_none_or(|delivered| current != delivered) {
                break;
            }
            self.changes.changed().await.map_err(|_| Error::Closed)?;
        }
        let generation = *self.changes.borrow();
        let replica = Replica::<A> {
            inner: self.observer.runtime.upgrade().ok_or(Error::Closed)?,
        };
        let snapshot = replica.track_mutation(self.handle, &self.peers).await?;
        self.delivered = Some(generation);
        Ok(snapshot)
    }
}
fn check_peers(peers: &[NodeId]) -> Result<(), Error> {
    if peers.len() > 64 {
        return Err(Error::Quota {
            resource: "tracked peers",
            limit: 64,
        });
    }
    Ok(())
}
