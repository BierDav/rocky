use super::*;
use rocky_credentials::Coordinator;

impl Node {
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
    pub(crate) fn set_connector(
        &self,
        connector: Option<Arc<dyn rocky_credentials::CoordinatorConnector>>,
    ) {
        *self
            .connector
            .write()
            .unwrap_or_else(PoisonError::into_inner) = connector;
    }
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
pub(super) fn coordination_refusal(code: &'static str, message: &str) -> Failure {
    Box::new(Error::Application(Box::new(rocky_secure::Refusal {
        code,
        message: message.to_owned(),
    })))
}
impl Node {
    pub(super) fn coordinator(&self) -> Option<Arc<dyn Coordinator>> {
        self.coordinator
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
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
    pub(super) async fn coordinated_status(&self) -> Result<crate::CoordinatedLogStatus, Failure> {
        let log = self.replica.followed_log(self.domain().scope).await;
        Ok(crate::CoordinatedLogStatus {
            trusted: self.replica.domain_log().is_some(),
            tip: log.tip,
        })
    }
}
