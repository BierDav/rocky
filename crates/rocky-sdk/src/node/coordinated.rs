//! A trusted node's composed coordinated log: it ends tips' logs with
//! their seals and acknowledges their terminal entries. Rocky orders no
//! writes through it; applications coordinate their own.
use super::*;
use rocky_core::DomainLog;

impl Node {
    /// Makes this device a trusted node of its domain's coordinated logs, or
    /// ends that with `None`. The log is the application's; Rocky never
    /// closes it.
    pub fn coordinate(self: &Arc<Self>, log: Option<Arc<dyn DomainLog>>) {
        self.replica.set_domain_log(log);
    }
}
/// Counter claims through the composed coordination service. A claim never
/// runs inside a database transaction.
#[cfg(all(feature = "iroh", not(target_family = "wasm")))]
impl Node {
    fn counters(&self) -> Result<&rocky_coordinator::service::NetworkCoordinator, Failure> {
        self.service
            .as_ref()
            .and_then(|service| service.client())
            .map(|client| &**client)
            .ok_or_else(|| {
                refused("claims need a coordination service composed with this database")
            })
    }
    pub(crate) async fn claim(
        &self,
        counter: crate::counters::Name,
        claim: crate::counters::Claim,
    ) -> Result<u64, Failure> {
        let scope = self.domain().scope;
        Ok(self.counters()?.claim(scope, counter, claim).await?)
    }
    pub(crate) async fn counter(
        &self,
        counter: crate::counters::Name,
        claim: Option<crate::counters::Claim>,
    ) -> Result<crate::counters::Reading, Failure> {
        let scope = self.domain().scope;
        Ok(self.counters()?.counter(scope, counter, claim).await?)
    }
}
impl Node {
    /// Whether this device is a trusted node with a composed log, and the
    /// log of the tip it follows.
    pub(super) async fn coordinated_status(&self) -> Result<Value, Failure> {
        let log = self.replica.followed_log(self.domain().scope).await;
        Ok(json!({
            "trusted": self.replica.domain_log().is_some(),
            "tip": HEXLOWER.encode(&log.tip.0),
        }))
    }
}
