//! A trusted node's composed coordinated log: it ends tips' logs with
//! their seals and acknowledges their terminal entries. Rocky orders no
//! writes through it; applications coordinate their own.
use super::*;
use rocky_credentials::Coordinator;

impl Node {
    /// Makes this device a trusted node of its domain's coordinated logs, or
    /// ends that with `None`. The log is the application's; Rocky never
    /// closes it.
    pub async fn coordinate(
        self: &Arc<Self>,
        log: Option<Arc<dyn Coordinator>>,
    ) -> Result<(), Failure> {
        let log = if let Some(client) = log {
            let settings = self.settings.lock().await;
            let certificate = settings
                .credentials
                .certificate()
                .ok_or("missing node certificate")?
                .clone();
            Some(client.bind(Settings::node_key(&self.vault)?, certificate)?)
        } else {
            None
        };
        self.coordinate_bound(log);
        Ok(())
    }
    pub(crate) fn coordinate_bound(self: &Arc<Self>, log: Option<Arc<dyn Coordinator>>) {
        if let Some(client) = &log {
            client.observe(self.coordination.observer());
        }
        self.replica.set_domain_log(
            log.clone()
                .map(|client| client as Arc<dyn rocky_core::DomainLog>),
        );
        *self
            .coordinator
            .write()
            .unwrap_or_else(PoisonError::into_inner) = log;
    }
}
/// Counter claims through the composed coordination service, as a trusted
/// node or a holder of a claim right. A claim never runs inside a database
/// transaction.
impl Node {
    pub(super) fn coordinator(&self) -> Option<Arc<dyn Coordinator>> {
        self.coordinator
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    fn counters(&self) -> Result<Arc<dyn Coordinator>, Failure> {
        self.coordinator()
            .ok_or_else(|| Box::new(crate::CoordinatorNotConfigured) as Failure)
    }
    pub(crate) async fn claim(
        &self,
        counter: crate::counters::Name,
        claim: crate::counters::Claim,
    ) -> Result<u64, Failure> {
        let scope = self.domain().scope;
        let client = self.counters()?;
        self.present_claims(&*client).await?;
        Ok(client.claim(scope, counter, claim).await?)
    }
    pub(crate) async fn counter(
        &self,
        counter: crate::counters::Name,
        claim: Option<crate::counters::Claim>,
    ) -> Result<crate::counters::Reading, Failure> {
        let scope = self.domain().scope;
        let client = self.counters()?;
        self.present_claims(&*client).await?;
        Ok(client.counter(scope, counter, claim).await?)
    }
}
impl Node {
    /// Whether this device is a trusted node with a composed log, and the
    /// log of the tip it follows.
    pub(super) async fn coordinated_status(&self) -> Result<crate::CoordinatedLogStatus, Failure> {
        let log = self.replica.followed_log(self.domain().scope).await;
        Ok(crate::CoordinatedLogStatus {
            trusted: self.replica.domain_log().is_some(),
            tip: log.tip,
        })
    }
}
