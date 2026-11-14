//! Held changes remain local until an application reauthors or drops them.
use super::*;
impl Node {
    pub(super) async fn drop_held(&self, id: rocky_core::Commitment) -> Result<(), Failure> {
        self.replica.drop_held(id).await.map_err(refused)?;
        self.observations.touch();
        Ok(())
    }
}
