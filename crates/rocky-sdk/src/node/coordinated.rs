//! A trusted node's composed coordinated log: it ends tips' logs with
//! their seals and acknowledges their terminal entries. Rocky orders no
//! writes through it; applications coordinate their own.
use super::*;
use rocky_credentials::Coordinator;

impl Node {
    /// Makes this device a trusted node of its domain's coordinated logs, or
    /// ends that with `None`. The log is the application's; Rocky never
    /// closes it. One other than the coordinator the database's settings
    /// name is refused with `coordinator_mismatch`.
    pub async fn coordinate(&self, log: Option<Arc<dyn Coordinator>>) -> Result<(), Failure> {
        let log = if let Some(client) = log {
            if let Some(named) = self.named_coordinator().await?
                && named.identity != client.identity()
            {
                return Err(coordination_refusal(
                    rocky_secure::COORDINATOR_MISMATCH,
                    "this coordinator is not the one the database's owner-signed settings name",
                ));
            }
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
    pub(crate) fn coordinate_bound(&self, log: Option<Arc<dyn Coordinator>>) {
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
    /// Lets this device reach the coordinator the database's settings name
    /// when the application composed none; `None` stops that.
    pub(crate) fn set_connector(
        &self,
        connector: Option<Arc<dyn rocky_credentials::CoordinatorConnector>>,
    ) {
        *self
            .connector
            .write()
            .unwrap_or_else(PoisonError::into_inner) = connector;
    }
    /// The coordinator the database's owner-signed settings name.
    pub(super) async fn named_coordinator(
        &self,
    ) -> Result<Option<rocky_credentials::CoordinatorBundle>, Failure> {
        Ok(self
            .replica
            .tip_graph(self.domain().scope)
            .await?
            .coordinator()?
            .cloned())
    }
    /// A signing device without a composed coordinator reaches the one the
    /// database's settings name through the application's connector, which
    /// verifies its bundle. A failure is kept for the seal it blocks.
    pub(crate) async fn adopt_named_coordinator(&self) {
        let adopted = async {
            let Some(bundle) = self.named_coordinator().await? else {
                return Ok(());
            };
            if self.coordinator().is_some() || self.tip_signer(self.domain().scope).await.is_err() {
                return Ok(());
            }
            let connector = self
                .connector
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .ok_or("the application supplies no coordinator connector")?;
            let client = connector.connect(&bundle)?;
            if client.identity() != bundle.identity {
                return Err(coordination_refusal(
                    rocky_secure::COORDINATOR_MISMATCH,
                    "the connector reached another coordinator than the settings name",
                ));
            }
            self.coordinate(Some(client)).await?;
            tracing::info!("composed the coordinator the database's settings name");
            Ok::<_, Failure>(())
        }
        .await;
        *self
            .connect_failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = adopted.err().map(|error| error.to_string());
    }
    /// Why this device may not seal now: the database's settings name a
    /// coordinator and this device has no composed log of it.
    pub(super) async fn seal_blocker(&self) -> Result<Option<crate::SealSkip>, Failure> {
        let Some(named) = self.named_coordinator().await? else {
            return Ok(None);
        };
        Ok(match self.replica.domain_log() {
            Some(log) if log.identity() == named.identity => None,
            Some(_) => Some(crate::SealSkip::CoordinatorMismatch),
            None => Some(crate::SealSkip::CoordinatorRequired(
                self.connect_failure
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
                    .unwrap_or_else(|| {
                        "the database's settings name a coordinator this device has not composed"
                            .to_owned()
                    }),
            )),
        })
    }
    /// Refuses a composed coordinator other than the one the database's
    /// settings name: an explicit client the application passed at open,
    /// or one composed before the settings arrived.
    pub(crate) async fn refuse_mismatched_coordinator(&self) -> Result<(), Failure> {
        if let (Some(client), Some(named)) = (self.coordinator(), self.named_coordinator().await?)
            && client.identity() != named.identity
        {
            return Err(coordination_refusal(
                rocky_secure::COORDINATOR_MISMATCH,
                "this coordinator is not the one the database's owner-signed settings name",
            ));
        }
        Ok(())
    }
}
/// A refusal with a stable call error code.
pub(super) fn coordination_refusal(code: &'static str, message: &str) -> Failure {
    Box::new(Error::Application(Box::new(rocky_secure::Refusal {
        code,
        message: message.to_owned(),
    })))
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
    /// The composed coordinator for counters: never another than the one
    /// the database's settings name, whose counters are the database's.
    async fn counters(&self) -> Result<Arc<dyn Coordinator>, Failure> {
        let client = self
            .coordinator()
            .ok_or_else(|| Box::new(crate::CoordinatorNotConfigured) as Failure)?;
        self.refuse_mismatched_coordinator().await?;
        Ok(client)
    }
    pub(crate) async fn claim(
        &self,
        counter: crate::counters::Name,
        claim: crate::counters::Claim,
    ) -> Result<u64, Failure> {
        let scope = self.domain().scope;
        let client = self.counters().await?;
        self.present_claims(&*client).await?;
        Ok(client.claim(scope, counter, claim).await?)
    }
    pub(crate) async fn counter(
        &self,
        counter: crate::counters::Name,
        claim: Option<crate::counters::Claim>,
    ) -> Result<crate::counters::Reading, Failure> {
        let scope = self.domain().scope;
        let client = self.counters().await?;
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
