use crate::*;
use rocky_core::{Coordinator, ScopedOperationId, StrongHead};

pub type PayloadFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<T>, Error>> + Send + 'a>>;
/// Explicit transport boundary. A connected replica supplies retained payloads;
/// a network adapter can report missing payloads without inventing completeness.
pub trait StrongPayloadSource: Send + Sync {
    fn result<'a>(
        &'a self,
        discovered: &'a StrongHead,
        sequence: u64,
    ) -> PayloadFuture<'a, ConfirmedPublication>;
    fn operation(&self, id: ScopedOperationId) -> PayloadFuture<'_, Vec<u8>>;
}
impl<A> StrongPayloadSource for Replica<A>
where
    A: StoredApplication,
    A::State: Clone,
    A::Policy: Send + 'static,
{
    fn result<'a>(
        &'a self,
        discovered: &'a StrongHead,
        sequence: u64,
    ) -> PayloadFuture<'a, ConfirmedPublication> {
        Box::pin(async move {
            if discovered.scope != self.inner.scope {
                return Err(rocky_core::ProtocolError::ForeignScope.into());
            }
            Ok(self
                .confirmed_publications()
                .await?
                .into_iter()
                .find(|publication| {
                    publication.package.sequence == sequence
                        && publication.package.base.configuration == discovered.configuration
                        && (sequence != discovered.sequence
                            || publication.receipt.confirmation.new_head == discovered.head)
                }))
        })
    }
    fn operation(&self, id: ScopedOperationId) -> PayloadFuture<'_, Vec<u8>> {
        Box::pin(Replica::operation(self, id))
    }
}
/// Confirmed results one pass fetches and applies.
const MAX_CATCHUP_RESULTS: usize = 256;
impl<A> Replica<A>
where
    A: StoredApplication,
    A::State: Clone,
    A::Policy: Send + 'static,
{
    /// One bounded discovery/fetch/apply pass; no persistent remote subscription.
    pub async fn read_strong_from(
        &self,
        query: BoundQuery,
        options: ReadOptions,
        coordinator: &dyn Coordinator,
        source: &dyn StrongPayloadSource,
    ) -> Result<StrongRead, Error> {
        let discovered = match self
            .read_strong(query.clone(), options.clone(), coordinator)
            .await?
        {
            StrongRead::Ready(snapshot) => return Ok(StrongRead::Ready(snapshot)),
            StrongRead::Unavailable => return Ok(StrongRead::Unavailable),
            StrongRead::Pending { discovered, .. } => discovered,
        };
        let local = self.strong_status().await?;
        if discovered.sequence.saturating_sub(local.sequence) > MAX_CATCHUP_RESULTS as u64 {
            return Err(Error::Quota {
                resource: "strong read catchup",
                limit: MAX_CATCHUP_RESULTS,
            });
        }
        for sequence in local.sequence.saturating_add(1)..=discovered.sequence {
            let Some(publication) = source.result(&discovered, sequence).await? else {
                return Ok(StrongRead::Pending {
                    discovered,
                    reason: PendingReason::MissingConfirmedPackage,
                });
            };
            if publication.package.sequence != sequence
                || publication.package.request.scope != options.scope
                || publication.package.base.configuration != discovered.configuration
                || (sequence == discovered.sequence
                    && publication.receipt.confirmation.new_head != discovered.head)
            {
                return Err(Error::Rejected(
                    "fetched result does not match discovered head".into(),
                ));
            }
            for id in publication.package.required()? {
                let id = ScopedOperationId {
                    scope: options.scope,
                    operation: id,
                };
                if self.operation(id).await?.is_none() {
                    let Some(bytes) = source.operation(id).await? else {
                        return Ok(StrongRead::Pending {
                            discovered,
                            reason: PendingReason::MissingInput,
                        });
                    };
                    let operation = rocky_core::Envelope::decode(&bytes)?;
                    if operation.scoped_id() != id {
                        return Err(Error::Rejected("fetched operation identity differs".into()));
                    }
                    self.receive(bytes).await?;
                }
            }
            self.process_pending().await?;
            self.receive_result(
                publication.package.encode()?,
                publication.receipt.encode(),
                coordinator,
            )
            .await?;
            tokio::task::yield_now().await;
        }
        self.read_strong(query, options, coordinator).await
    }
}
