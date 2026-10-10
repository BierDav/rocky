//! One scope-proved exchange of signed horizon evidence.
use super::*;
use crate::replica::HorizonFinalization;

#[derive(Serialize, Deserialize)]
pub(super) enum HorizonRelayMessage {
    Exchange {
        scope: ResourceScope,
        signed: Option<Box<rocky_credentials::SignedHorizon>>,
        acks: Vec<rocky_credentials::HorizonAck>,
        finalized: Option<Box<HorizonFinalization>>,
    },
    Evidence {
        signed: Option<Box<rocky_credentials::SignedHorizon>>,
        acks: Vec<rocky_credentials::HorizonAck>,
        finalized: Option<Box<HorizonFinalization>>,
    },
}
impl NativeSession {
    pub async fn relay_mergeable_horizon<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
    ) -> Result<(), Error> {
        self.ensure_holding(replica, &[scope]).await?;
        let (signed, acks) = replica.horizon_relay(scope).await?;
        let finalized = replica.relay_finalized_horizon(scope).await?;
        if let Some(evidence) = &finalized {
            self.deposit_finalized_checkpoint(replica, &evidence.checkpoint)
                .await?;
        }
        self.send(HorizonRelayMessage::Exchange {
            scope,
            signed,
            acks,
            finalized,
        })
        .await?;
        let HorizonRelayMessage::Evidence {
            signed,
            acks,
            finalized,
        } = self.reply::<HorizonRelayMessage>().await?
        else {
            return Err(denied("unexpected horizon relay response"));
        };
        let archive = if replica.can_read_scope(scope).await {
            None
        } else {
            finalized
                .as_ref()
                .map(|evidence| evidence.checkpoint.clone())
        };
        replica
            .receive_horizon_relay(scope, signed, acks, finalized)
            .await?;
        if let Some(checkpoint) = archive {
            self.archive_peer_checkpoint(replica, checkpoint).await?;
        }
        Ok(())
    }
    pub(super) async fn answer_horizon_relay<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        message: HorizonRelayMessage,
    ) -> Result<(), Error> {
        let HorizonRelayMessage::Exchange {
            scope,
            signed,
            acks,
            finalized,
        } = message
        else {
            return Err(denied("unexpected horizon relay request"));
        };
        replica
            .receive_horizon_relay(scope, signed, acks, finalized)
            .await?;
        let (signed, acks) = replica.horizon_relay(scope).await?;
        let finalized = replica.relay_finalized_horizon(scope).await?;
        self.send(HorizonRelayMessage::Evidence {
            signed,
            acks,
            finalized,
        })
        .await
    }
}
