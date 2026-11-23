use crate::{
    CoordinationError,
    certified::now,
    counters::{Claim, Name, Reading},
    machine::{REQUEST_LABEL, Request},
    service::{NetworkCoordinator, Operation, Outcome},
    transport::nonce,
};
use rocky_core::Commitment;
use rocky_sdk::{Database, DeviceCredential};
use std::sync::Arc;

#[derive(Debug)]
pub enum CounterError {
    Coordination(CoordinationError),
    Database(rocky_sdk::Error),
}
impl std::fmt::Display for CounterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Coordination(error) => write!(f, "coordinator: {error}"),
            Self::Database(error) => write!(f, "database: {error}"),
        }
    }
}
impl std::error::Error for CounterError {}
impl From<CoordinationError> for CounterError {
    fn from(error: CoordinationError) -> Self {
        Self::Coordination(error)
    }
}
impl From<rocky_sdk::Error> for CounterError {
    fn from(error: rocky_sdk::Error) -> Self {
        Self::Database(error)
    }
}

pub struct Counters {
    database: Database,
    client: Arc<NetworkCoordinator>,
}
impl Counters {
    pub fn new(database: Database, client: Arc<NetworkCoordinator>) -> Self {
        Self { database, client }
    }
    pub async fn claim(&self, counter: &Name, claim: Claim) -> Result<u64, CounterError> {
        let device = self.database.device_credential().await?;
        let operation = Operation::Claim {
            scope: device.scope,
            counter: counter.clone(),
            claim,
        };
        match self.submit(device, operation).await? {
            Outcome::Claimed(value) => Ok(value),
            _ => Err(CoordinationError::Corrupt.into()),
        }
    }
    pub async fn read(
        &self,
        counter: &Name,
        claim: Option<Claim>,
    ) -> Result<Reading, CounterError> {
        let device = self.database.device_credential().await?;
        let operation = Operation::Counter {
            scope: device.scope,
            counter: counter.clone(),
            claim,
        };
        match self.submit(device, operation).await? {
            Outcome::Counter(reading) => Ok(reading),
            _ => Err(CoordinationError::Corrupt.into()),
        }
    }
    pub async fn share_revocations(&self) -> Result<Commitment, CounterError> {
        let device = self.database.device_credential().await?;
        Ok(self.client.revoke(device.scope, device.revocations).await?)
    }
    async fn submit(
        &self,
        device: DeviceCredential,
        operation: Operation,
    ) -> Result<Outcome, CounterError> {
        let request = Request::unsigned(
            self.client.genesis()?,
            nonce(),
            operation,
            (device.node, None, Some(device.chain)),
            now()?,
        );
        let signature = self
            .database
            .sign_extension(REQUEST_LABEL, &request.extension_body()?)
            .await?;
        Ok(self
            .client
            .submit(&request.with_signature(signature))
            .await?)
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests;
