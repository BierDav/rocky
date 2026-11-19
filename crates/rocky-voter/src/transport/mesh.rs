use super::*;
use crate::VoterService;
use rocky_coordinator::protocol::converse;
use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex, PoisonError, Weak},
};

const EXCHANGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

#[derive(Default)]
pub(crate) struct Mesh {
    voters: Mutex<HashMap<u64, Weak<VoterService>>>,
    isolated: Mutex<BTreeSet<u64>>,
}
impl Mesh {
    pub(crate) fn join(&self, node: u64, voter: &Arc<VoterService>) {
        self.voters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(node, Arc::downgrade(voter));
    }
    pub(crate) fn isolate(&self, node: u64) {
        self.isolated
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(node);
    }
    pub(crate) fn heal(&self, node: u64) {
        self.isolated
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&node);
    }
    fn reachable(&self, from: u64, to: u64) -> Option<Arc<VoterService>> {
        let isolated = self.isolated.lock().unwrap_or_else(PoisonError::into_inner);
        if isolated.contains(&from) || isolated.contains(&to) {
            return None;
        }
        self.voters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&to)
            .and_then(Weak::upgrade)
    }
    pub(crate) async fn exchange<T: Serialize + DeserializeOwned>(
        &self,
        from: u64,
        voter: &Voter,
        genesis: rocky_core::Commitment,
        key: &PrivateKey,
        message: Message,
    ) -> Result<T, CoordinationError> {
        let Some(target) = self.reachable(from, voter.id) else {
            tokio::time::sleep(EXCHANGE_TIMEOUT).await;
            return Err(CoordinationError::Unavailable);
        };
        let (mut caller, answering) = tokio::io::duplex(2 * MAX_FRAME);
        tokio::spawn(target.answer_voter(answering));
        tokio::time::timeout(
            EXCHANGE_TIMEOUT,
            converse(&mut caller, voter.key, genesis, key, message),
        )
        .await
        .map_err(|_| CoordinationError::Unavailable)?
    }
}
