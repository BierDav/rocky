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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandTracking {
    pub handle: CommandHandle,
    pub scope: Scope,
    pub request: Option<rocky_core::RequestId>,
    pub state: Option<CommandState>,
    pub outcome: Option<Vec<u8>>,
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
    pub async fn track_command(
        &self,
        handle: CommandHandle,
        peers: &[NodeId],
    ) -> Result<CommandTracking, Error> {
        check_peers(peers)?;
        let request = self.lookup_command(handle).await?;
        let status = self.strong_status().await?;
        let progress = request.as_ref().and_then(|request| {
            status
                .commands
                .iter()
                .find(|progress| progress.request.id == request.id)
        });
        let publications = self.confirmed_publications().await?;
        let publication = request.as_ref().and_then(|request| {
            publications
                .iter()
                .find(|publication| publication.package.request.id == request.id)
        });
        let mut evidence = Vec::new();
        for peer in peers {
            self.peer_acknowledgments(*peer).await?;
            let ack = publication.and_then(|publication| {
                status.peer_acknowledgments.iter().find(|(node, hash, _)| {
                    node == peer && *hash == publication.receipt.confirmation.result
                })
            });
            evidence.push(PeerEvidence {
                peer: *peer,
                received: ack.is_some(),
                applied: ack.is_some_and(|(_, _, applied)| *applied),
            });
        }
        Ok(CommandTracking {
            handle,
            scope: status.scope,
            request: request.map(|request| request.id),
            state: progress.map(|progress| progress.state.clone()),
            outcome: publication.map(|publication| publication.package.rows.clone()),
            peers: evidence,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub enum TrackedHandle {
    Mutation(MutationHandle),
    Command(CommandHandle),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackingSnapshot {
    Mutation(MutationProgress),
    Command(CommandTracking),
}
pub struct ProgressObserver<A: StoredApplication> {
    observer: Observer<A::Row>,
    changes: watch::Receiver<u64>,
    handle: TrackedHandle,
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
        handle: TrackedHandle,
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
    pub async fn next(&mut self) -> Result<TrackingSnapshot, Error> {
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
        let snapshot = match self.handle {
            TrackedHandle::Mutation(handle) => {
                TrackingSnapshot::Mutation(replica.track_mutation(handle, &self.peers).await?)
            }
            TrackedHandle::Command(handle) => {
                TrackingSnapshot::Command(replica.track_command(handle, &self.peers).await?)
            }
        };
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
