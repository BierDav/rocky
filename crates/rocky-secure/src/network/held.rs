//! Held-change exchange carries each original author's encrypted frames.
use super::*;

#[derive(Serialize, Deserialize)]
pub(super) enum HeldMessage {
    Pull {
        scope: ResourceScope,
        after: Option<Hash>,
        quota: usize,
    },
    Page {
        scope: ResourceScope,
        frames: Vec<EncryptedFrame>,
        next: Option<Hash>,
    },
    Push {
        scope: ResourceScope,
        frames: Vec<EncryptedFrame>,
    },
    Stored,
}

impl NativeSession {
    /// Exchanges original held frames in both directions, across tips.
    /// Returns whether another session is needed to finish the exchange.
    pub async fn sync_held_changes<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        quota: usize,
    ) -> Result<bool, Error> {
        if !replica
            .held_exchange_allowed(self.peer, &self.credentials, scope)
            .await?
        {
            return Ok(false);
        }
        if !self.try_scoped_holding(replica, &[scope], true).await? {
            return Ok(false);
        }
        let mut after = None;
        loop {
            if self.low(MAX_SECURE_FRAME) {
                return Ok(true);
            }
            let (_, next) = self.pull_held_page(replica, scope, after, quota).await?;
            match next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        after = None;
        loop {
            if self.low(MAX_SECURE_FRAME) {
                return Ok(true);
            }
            let (_, next) = self.push_held_page(replica, scope, after, quota).await?;
            match next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        Ok(false)
    }

    /// Pulls one held-change page, regardless of the peers' followed tips.
    pub async fn pull_held_page<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        after: Option<Hash>,
        quota: usize,
    ) -> Result<(usize, Option<Hash>), Error> {
        self.ensure_held_holding(replica, scope).await?;
        self.send(HeldMessage::Pull {
            scope,
            after,
            quota,
        })
        .await?;
        let HeldMessage::Page {
            scope: received,
            frames,
            next,
        } = self.reply::<HeldMessage>().await?
        else {
            return Err(denied("unexpected held-change response"));
        };
        if received != scope
            || frames.len() > rows::MAX_SECURE_PAGE
            || frames.iter().any(|frame| frame.scope().ok() != Some(scope))
        {
            return Err(denied("held-change page outside its scope or quota"));
        }
        let (frames, incomplete) = self.complete_frames(frames)?;
        if incomplete != 0 {
            return Err(denied(crate::wire::INCOMPLETE_FRAME));
        }
        let count = frames.len();
        replica.receive_held_frames(frames).await?;
        Ok((count, next))
    }

    /// Pushes one locally held page and returns the cursor after it.
    pub async fn push_held_page<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        after: Option<Hash>,
        quota: usize,
    ) -> Result<(usize, Option<Hash>), Error> {
        self.ensure_held_holding(replica, scope).await?;
        let (mut frames, next) = replica
            .held_frame_page(self.peer, &self.credentials, scope, after, quota)
            .await?;
        let count = frames.len();
        self.carry_chains(&mut frames).await?;
        self.send(HeldMessage::Push { scope, frames }).await?;
        if !matches!(self.reply::<HeldMessage>().await?, HeldMessage::Stored) {
            return Err(denied("unexpected held-change response"));
        }
        Ok((count, next))
    }

    pub(super) async fn answer_held<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        message: HeldMessage,
    ) -> Result<(), Error> {
        match message {
            HeldMessage::Pull {
                scope,
                after,
                quota,
            } => {
                let (mut frames, next) = replica
                    .held_frame_page(self.peer, &self.credentials, scope, after, quota)
                    .await?;
                self.carry_chains(&mut frames).await?;
                self.send(HeldMessage::Page {
                    scope,
                    frames,
                    next,
                })
                .await
            }
            HeldMessage::Push { scope, frames } => {
                replica
                    .authorize_held_reader(self.peer, &self.credentials, scope)
                    .await?;
                if frames.len() > rows::MAX_SECURE_PAGE
                    || frames.iter().any(|frame| frame.scope().ok() != Some(scope))
                {
                    return Err(denied("held-change push outside its scope or quota"));
                }
                let (frames, incomplete) = self.complete_frames(frames)?;
                if incomplete != 0 {
                    return Err(denied(crate::wire::INCOMPLETE_FRAME));
                }
                replica.receive_held_frames(frames).await?;
                self.send(HeldMessage::Stored).await
            }
            _ => Err(denied("unexpected held-change request")),
        }
    }
}
