//! Mergeable horizons: cuts, logs, acknowledgments and rejoins.
use super::*;

#[derive(Serialize, Deserialize)]
pub(super) enum HorizonMessage {
    MergeableCut {
        scope: ResourceScope,
    },
    MergeableState {
        signed: Option<Box<rocky_credentials::SignedHorizon>>,
        behind: bool,
    },
    HorizonRejoined {
        scope: ResourceScope,
        horizon: Commitment,
    },
    HorizonAck {
        scope: ResourceScope,
        ack: rocky_credentials::HorizonAck,
    },
    Horizon {
        scope: ResourceScope,
        after: Option<PublicKey>,
    },
    HorizonLogs {
        page: HorizonPage,
    },
}

impl NativeSession {
    pub async fn acknowledge_mergeable_horizon<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
    ) -> Result<bool, Error> {
        let Some(signed) = replica.admitted_horizon(scope).await? else {
            return Ok(false);
        };
        if !signed
            .statement
            .peers
            .iter()
            .any(|peer| peer.node == self.local.public_key())
        {
            return Ok(false);
        }
        let Ok(ack) = replica.acknowledge_horizon(scope).await else {
            return Ok(false);
        };
        self.ensure_holding(replica, &[scope]).await?;
        self.send(HorizonMessage::HorizonAck { scope, ack }).await?;
        self.acknowledged("unexpected horizon acknowledgment")
            .await?;
        Ok(true)
    }
    pub(super) async fn answer_horizon<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        message: HorizonMessage,
    ) -> Result<(), Error> {
        match message {
            HorizonMessage::MergeableCut { scope } => {
                let behind = replica
                    .horizon_peer_behind(self.peer, &self.credentials, scope)
                    .await?;
                let signed = replica.admitted_horizon(scope).await?;
                self.send(HorizonMessage::MergeableState { signed, behind })
                    .await
            }
            HorizonMessage::HorizonRejoined { scope, horizon } => {
                replica
                    .acknowledge_horizon_rebootstrap(self.peer, &self.credentials, scope, horizon)
                    .await?;
                self.send(Message::Acknowledged).await
            }
            HorizonMessage::HorizonAck { scope, ack } => {
                if ack.node != self.peer {
                    return Err(denied("horizon acknowledgment differs from session peer"));
                }
                replica.receive_horizon_ack(scope, ack).await?;
                self.send(Message::Acknowledged).await
            }
            HorizonMessage::Horizon { scope, after } => {
                let page = replica
                    .horizon_page(self.peer, &self.credentials, scope, after)
                    .await?;
                self.send(HorizonMessage::HorizonLogs { page }).await
            }
            _ => Err(denied("unexpected native request")),
        }
    }
}
