use crate::Demo;
use rocky_core::fixtures;
use rocky_runtime::Error;
use std::sync::Arc;

pub(crate) fn new_coordinator() -> Result<Arc<rocky_coordinator::MemoryCoordinator>, Error> {
    let coordinator = Arc::new(rocky_coordinator::MemoryCoordinator::default());
    let configuration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| Error::Incompatible(e.to_string()))?
        .as_nanos()
        .to_le_bytes();
    coordinator.register(
        fixtures::SCOPE,
        fixtures::OWNER,
        configuration,
        vec![fixtures::LAPTOP, fixtures::PHONE],
        1,
        1,
    )?;
    coordinator.acquire(fixtures::SCOPE, fixtures::OWNER, fixtures::LAPTOP)?;
    Ok(coordinator)
}
impl Demo {
    pub async fn takeover_phone(&self) -> Result<(), Error> {
        let head = self.coordinator.head(fixtures::SCOPE)?;
        let status = self.replicas[1].strong_status().await?;
        if status
            .configuration
            .is_some_and(|c| c != head.configuration)
        {
            return Err(rocky_core::CoordinationError::CoordinationStateLost.into());
        }
        if status.sequence != head.sequence || (head.sequence > 0 && status.head != Some(head.head))
        {
            return Err(Error::StrongPending(
                rocky_runtime::PendingReason::MissingHead,
            ));
        }
        self.coordinator.takeover(
            fixtures::SCOPE,
            fixtures::OWNER,
            fixtures::PHONE,
            head.epoch,
        )?;
        let after = self.coordinator.head(fixtures::SCOPE)?;
        let status = self.replicas[1].strong_status().await?;
        if status.sequence != after.sequence
            || (after.sequence > 0 && status.head != Some(after.head))
        {
            return Err(Error::StrongPending(
                rocky_runtime::PendingReason::MissingHead,
            ));
        }
        // Preparation also checks the head for every command.
        self.replicas[1]
            .execute_pending(self.coordinator.as_ref())
            .await
    }
    pub async fn pump_coordination(
        &self,
        transport: &mut crate::transport::SimulatedTransport,
    ) -> Result<(), crate::transport::Error> {
        transport.reconcile_strong(&self.replicas).await?;
        for _ in 0..32 {
            if !transport
                .deliver_strong(0, &self.replicas, self.coordinator.as_ref())
                .await?
            {
                break;
            }
        }
        for replica in &self.replicas {
            replica.reconcile_strong(self.coordinator.as_ref()).await?;
        }
        let head = self
            .coordinator
            .head(fixtures::SCOPE)
            .map_err(Error::from)?;
        let index = if head.executor == fixtures::LAPTOP {
            0
        } else {
            1
        };
        self.replicas[index]
            .execute_pending(self.coordinator.as_ref())
            .await?;
        Ok(())
    }
}
