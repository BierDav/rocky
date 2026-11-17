use super::*;
use rocky_core::{DomainLog, Scope, TipLog, coordinated_chain};

mod finality;
pub use finality::{FinalityStall, Finalization};
pub(super) mod relay;
pub use relay::StagedCloseStatus;
pub(in crate::replica) use relay::public_finality;
pub(super) mod statements;
mod terminal;
pub use statements::{COORDINATOR_MISMATCH, COORDINATOR_REQUIRED, Refusal};
pub(in crate::replica) use statements::{Unconfirmed, refusal};

impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) fn tip_log(&self, domain: Scope) -> TipLog {
        TipLog {
            scope: domain,
            tip: self.workspace.tip,
        }
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn followed_log(&self, domain: Scope) -> TipLog {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .tip_log(domain)
    }
}
