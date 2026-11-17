use super::*;
use rocky_core::{LogResult, LogTerminal};
use rocky_credentials::{LogPoint, Recovery};

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn seal_log(
        &self,
        log: TipLog,
        terminal: LogTerminal,
        coordinator: &dyn DomainLog,
    ) -> Result<u64, Error> {
        match coordinator.seal(log, terminal).await {
            Ok(LogResult::Committed { position, .. }) => Ok(position),
            Ok(LogResult::Acknowledged) => Err(Error::Corrupt(
                "the coordinator did not commit a seal".into(),
            )),
            Err(error) => Err(Error::Coordination(error)),
        }
    }
    pub async fn recovery_proposal(
        &self,
        log: TipLog,
        coordinator: &dyn DomainLog,
    ) -> Result<Recovery, Error> {
        let head = coordinator
            .log_head(log)
            .await
            .map_err(Error::Coordination)?;
        if head.terminal.is_some() {
            return Err(refusal("sealed", "a terminal entry ended the tip's log"));
        }
        let point = LogPoint {
            position: head.commit,
            running: head.running_hash,
        };
        Ok(Recovery {
            retained: point,
            replaced: point,
        })
    }
}
