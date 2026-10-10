//! Existence logs and their bases (`docs/runtime/existence-logs.md`): any
//! device of the domain may ask.
use super::*;
use std::collections::BTreeMap;

/// Existence records per exchanged page and signers per frontier.
const EXISTENCE_PAGE: usize = 128;
const MAX_EXISTENCE_SIGNERS: usize = 4096;

#[derive(Serialize, Deserialize)]
pub(super) enum ExistenceMessage {
    BaseOffer {
        scope: ResourceScope,
        expected: Option<Commitment>,
    },
    Base {
        attachment: Option<Box<ExistenceBaseAttachment>>,
    },
    BaseBegin {
        attachment: Box<ExistenceBaseAttachment>,
    },
    BaseCursor {
        after: Option<i64>,
        complete: bool,
    },
    BaseStage {
        scope: ResourceScope,
        page: ExistenceBasePage,
    },
    BaseFinish {
        scope: ResourceScope,
    },
    BasePull {
        scope: ResourceScope,
        base: Commitment,
        after: Option<i64>,
    },
    BaseRows {
        page: ExistenceBasePage,
    },
    Frontier {
        scope: ResourceScope,
    },
    Held {
        frontier: BTreeMap<PublicKey, u64>,
    },
    Pull {
        scope: ResourceScope,
        known: BTreeMap<PublicKey, u64>,
        after: Option<(PublicKey, u64)>,
    },
    Records {
        page: ExistencePage,
    },
    Push {
        scope: ResourceScope,
        records: Vec<ExistenceRecord>,
    },
}

impl NativeSession {
    pub(super) async fn pull_existence_base<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        expected: Option<Commitment>,
    ) -> Result<bool, Error> {
        self.send(ExistenceMessage::BaseOffer { scope, expected })
            .await?;
        let ExistenceMessage::Base { attachment } = self.reply::<ExistenceMessage>().await? else {
            return Err(denied("unexpected existence base response"));
        };
        let Some(attachment) = attachment else {
            return if expected.is_some() {
                Err(denied("checkpoint existence attachment is unavailable"))
            } else {
                Ok(true)
            };
        };
        if attachment.scope != scope || expected.is_some_and(|id| attachment.id().ok() != Some(id))
        {
            return Err(denied("existence attachment differs from checkpoint"));
        }
        if let Some(held) = replica.installed_existence_base(scope).await? {
            if held.id()? == attachment.id()? || (expected.is_some() && held.covers(&attachment)?) {
                return Ok(true);
            }
            if expected.is_none()
                && attachment.cut()?.logs.iter().all(|next| {
                    held.cut().is_ok_and(|cut| {
                        cut.logs
                            .iter()
                            .find(|log| log.signer == next.signer)
                            .is_some_and(|log| next.sequence <= log.sequence)
                    })
                })
            {
                return Ok(true);
            }
        }
        let base = attachment.id()?;
        let (mut after, mut complete) = replica.begin_existence_base(*attachment).await?;
        while !complete {
            if self.low(MAX_SECURE_FRAME) {
                return Ok(false);
            }
            self.send(ExistenceMessage::BasePull { scope, base, after })
                .await?;
            let ExistenceMessage::BaseRows { page } = self.reply::<ExistenceMessage>().await?
            else {
                return Err(denied("unexpected existence base page"));
            };
            if page.base != base {
                return Err(denied("existence base changed during transfer"));
            }
            after = page.next;
            complete = after.is_none();
            replica.stage_existence_base(scope, page).await?;
        }
        replica.finish_existence_base(scope).await?;
        Ok(true)
    }
    pub(super) async fn push_existence_base<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
    ) -> Result<bool, Error> {
        let Some(base) = replica
            .existence_base_offer(self.peer, &self.credentials, scope)
            .await?
        else {
            return Ok(true);
        };
        self.send(ExistenceMessage::BaseOffer {
            scope,
            expected: None,
        })
        .await?;
        let ExistenceMessage::Base { attachment: held } = self.reply::<ExistenceMessage>().await?
        else {
            return Err(denied("unexpected existence base response"));
        };
        if let Some(held) = held {
            if held.id()? == base.id()? {
                return Ok(true);
            }
            if !held.cut()?.logs.iter().all(|previous| {
                base.cut().is_ok_and(|cut| {
                    cut.logs.iter().any(|next| {
                        next.signer == previous.signer
                            && next.sequence >= previous.sequence
                            && (next.sequence != previous.sequence || next.head == previous.head)
                    })
                })
            }) {
                return Err(denied(
                    "existence bases have incomparable cuts; obtain an issuer successor",
                ));
            }
        }
        let id = base.id()?;
        self.send(ExistenceMessage::BaseBegin {
            attachment: Box::new(base),
        })
        .await?;
        let ExistenceMessage::BaseCursor {
            mut after,
            mut complete,
        } = self.reply::<ExistenceMessage>().await?
        else {
            return Err(denied("unexpected existence cursor"));
        };
        while !complete {
            if self.low(MAX_SECURE_FRAME) {
                return Ok(false);
            }
            let page = replica
                .existence_base_page(
                    self.peer,
                    &self.credentials,
                    scope,
                    id,
                    after,
                    EXISTENCE_PAGE,
                )
                .await?;
            after = page.next;
            complete = after.is_none();
            self.send(ExistenceMessage::BaseStage { scope, page })
                .await?;
            self.acknowledged("unexpected existence stage acknowledgment")
                .await?;
        }
        self.send(ExistenceMessage::BaseFinish { scope }).await?;
        self.acknowledged("unexpected existence base acknowledgment")
            .await?;
        Ok(true)
    }
    /// Exchanges the existence log of `scope` with the peer: pulls the
    /// records this replica lacks, then pushes those the peer lacks.
    /// Returns how many records were received and sent.
    pub async fn sync_existence<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
    ) -> Result<(usize, usize), Error> {
        if !self.pull_existence_base(replica, scope, None).await? {
            return Err(denied(
                "existence base session exhausted; resume on the next session",
            ));
        }
        if !self.push_existence_base(replica, scope).await? {
            return Err(denied(
                "existence base session exhausted; resume on the next session",
            ));
        }
        let mut received = 0;
        let mut after = None;
        loop {
            let known = replica.existence_frontier(scope).await?;
            self.send(ExistenceMessage::Pull {
                scope,
                known,
                after,
            })
            .await?;
            let ExistenceMessage::Records { page } = self.reply::<ExistenceMessage>().await? else {
                return Err(denied("unexpected existence response"));
            };
            if page.records.len() > EXISTENCE_PAGE
                || page.records.iter().any(|record| record.scope() != scope)
            {
                return Err(denied("existence page outside request"));
            }
            received += page.records.len();
            replica.receive_existence_all(page.records).await?;
            after = page.next;
            if after.is_none() {
                break;
            }
        }
        self.send(ExistenceMessage::Frontier { scope }).await?;
        let ExistenceMessage::Held { mut frontier } = self.reply::<ExistenceMessage>().await?
        else {
            return Err(denied("unexpected existence response"));
        };
        let mut sent = 0;
        let mut after = None;
        loop {
            let page = replica
                .existence_page(
                    self.peer,
                    &self.credentials,
                    scope,
                    &frontier,
                    after,
                    EXISTENCE_PAGE,
                )
                .await?;
            if !page.records.is_empty() {
                sent += page.records.len();
                self.send(ExistenceMessage::Push {
                    scope,
                    records: page.records,
                })
                .await?;
                let ExistenceMessage::Held { frontier: theirs } =
                    self.reply::<ExistenceMessage>().await?
                else {
                    return Err(denied("unexpected existence response"));
                };
                frontier = theirs;
            }
            after = page.next;
            if after.is_none() {
                return Ok((received, sent));
            }
        }
    }
    pub(super) async fn answer_existence<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        message: ExistenceMessage,
    ) -> Result<(), Error> {
        match message {
            ExistenceMessage::BaseOffer { scope, expected } => {
                let attachment = replica
                    .existence_base_at(self.peer, &self.credentials, scope, expected)
                    .await?
                    .map(Box::new);
                self.send(ExistenceMessage::Base { attachment }).await
            }
            ExistenceMessage::BaseBegin { attachment } => {
                replica
                    .authorize_existence_peer(self.peer, &self.credentials, attachment.scope)
                    .await?;
                let (after, complete) = replica.begin_existence_base(*attachment).await?;
                self.send(ExistenceMessage::BaseCursor { after, complete })
                    .await
            }
            ExistenceMessage::BaseStage { scope, page } => {
                replica
                    .authorize_existence_peer(self.peer, &self.credentials, scope)
                    .await?;
                replica.stage_existence_base(scope, page).await?;
                self.send(Message::Acknowledged).await
            }
            ExistenceMessage::BaseFinish { scope } => {
                replica
                    .authorize_existence_peer(self.peer, &self.credentials, scope)
                    .await?;
                replica.finish_existence_base(scope).await?;
                self.send(Message::Acknowledged).await
            }
            ExistenceMessage::BasePull { scope, base, after } => {
                let page = replica
                    .existence_base_page(
                        self.peer,
                        &self.credentials,
                        scope,
                        base,
                        after,
                        EXISTENCE_PAGE,
                    )
                    .await?;
                self.send(ExistenceMessage::BaseRows { page }).await
            }
            ExistenceMessage::Frontier { scope } => {
                replica
                    .authorize_existence_peer(self.peer, &self.credentials, scope)
                    .await?;
                let frontier = replica.existence_frontier(scope).await?;
                self.send(ExistenceMessage::Held { frontier }).await
            }
            ExistenceMessage::Pull {
                scope,
                known,
                after,
            } => {
                if known.len() > MAX_EXISTENCE_SIGNERS {
                    return Err(denied("existence frontier quota"));
                }
                let page = replica
                    .existence_page(
                        self.peer,
                        &self.credentials,
                        scope,
                        &known,
                        after,
                        EXISTENCE_PAGE,
                    )
                    .await?;
                self.send(ExistenceMessage::Records { page }).await
            }
            ExistenceMessage::Push { scope, records } => {
                replica
                    .authorize_existence_peer(self.peer, &self.credentials, scope)
                    .await?;
                if records.len() > EXISTENCE_PAGE
                    || records.iter().any(|record| record.scope() != scope)
                {
                    return Err(denied("existence push outside its log or quota"));
                }
                replica.receive_existence_all(records).await?;
                let frontier = replica.existence_frontier(scope).await?;
                self.send(ExistenceMessage::Held { frontier }).await
            }
            _ => Err(denied("unexpected native request")),
        }
    }
}
