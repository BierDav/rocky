use super::*;
use crate::invitation::{AUTHORITY_INVITATIONS_KEY, Book};

#[cfg(test)]
pub(super) enum RedemptionHook {
    Fail,
    Pause(Arc<RedemptionGate>),
}

#[cfg(test)]
pub(super) struct RedemptionGate {
    pub(super) reached: tokio::sync::Notify,
    pub(super) open: tokio::sync::Semaphore,
}

impl Node {
    pub(super) async fn invite(
        &self,
        invite: &crate::Invite,
    ) -> Result<crate::Invitation, Failure> {
        let routes = self.own_routes().await;
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let issued = {
            let settings = self.settings.lock().await;
            let mut book = Book::read(&self.replica).await?;
            let time = now()?;
            let issued = settings
                .invite((&self.vault, routes), &mut book, invite, &registry, time)
                .map_err(named)?;
            self.replica
                .put_authority_state(AUTHORITY_INVITATIONS_KEY, &book.to_bytes()?, time)
                .await?;
            issued
        };
        self.activity
            .record(crate::activity::ActivityKind::IdentityInvited {
                invitation: issued.id,
                person: issued.code.person().map(crate::PersonId::new),
                access: invite.access().clone(),
                expires: issued.expires,
            });
        Ok(issued.invitation())
    }

    pub(super) async fn cancel_invitation(&self, id: crate::InvitationId) -> Result<(), Failure> {
        let settings = self.settings.lock().await;
        if !settings.issuer() {
            return Err(Box::new(crate::Error::refused(
                crate::Right::Administer,
                crate::DenyReason::AdminRequired,
            )));
        }
        let mut book = Book::read(&self.replica).await?;
        let time = now()?;
        book.cancel(id, time)?;
        self.replica
            .put_authority_state(AUTHORITY_INVITATIONS_KEY, &book.to_bytes()?, time)
            .await?;
        drop(settings);
        Ok(())
    }

    pub(super) async fn invitations(&self) -> Result<Vec<crate::PendingInvitation>, Failure> {
        let settings = self.settings.lock().await;
        if !settings.issuer() {
            return Err(Box::new(crate::Error::refused(
                crate::Right::Administer,
                crate::DenyReason::AdminRequired,
            )));
        }
        let book = Book::read(&self.replica).await?;
        drop(settings);
        Ok(book.listed(now()?))
    }

    pub(super) async fn answer_invitation(
        &self,
        request: rocky_credentials::InvitedEnrollmentRequest,
    ) -> Result<IdentityReply, Failure> {
        let _redeeming = self.redeeming.lock().await;
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let bootstrap = self.enrollment_bootstrap().await?;
        let held = self.replica.revocations().await;
        let redeemed = {
            let mut settings = self.settings.lock().await;
            let mut book = Book::read(&self.replica).await?;
            let mut draft = settings.clone();
            draft.learn_revocations(&held)?;
            draft.bootstrap = Some(bootstrap);
            let redeemed = draft
                .answer_invitation(&self.vault, &mut book, request, &registry, now()?)
                .map_err(named)?;
            draft.bootstrap = None;
            let book = book.to_bytes()?;
            settings
                .commit_with_entries(
                    draft,
                    &self.replica,
                    &[(AUTHORITY_INVITATIONS_KEY, Some(book.as_slice()))],
                    None,
                )
                .await?;
            redeemed
        };
        if let Err(error) = self.record_redemption(redeemed.changes).await {
            return Err(if redeemed.first {
                self.released(redeemed.id, redeemed.device.node, error)
                    .await
            } else {
                error
            });
        }
        self.see(redeemed.device.identity, redeemed.device.node)
            .await?;
        if redeemed.first {
            self.activity
                .record(crate::activity::ActivityKind::InvitationRedeemed {
                    invitation: redeemed.id,
                    identity: redeemed.device.identity,
                    node: redeemed.device.node,
                });
        }
        Ok(IdentityReply::Enrolled(redeemed.answer))
    }

    async fn record_redemption(&self, changes: crate::registry::Changes) -> Result<(), Failure> {
        self.before_recording_redemption().await?;
        self.commit(changes).await
    }

    #[cfg(not(test))]
    async fn before_recording_redemption(&self) -> Result<(), Failure> {
        Ok(())
    }

    #[cfg(test)]
    async fn before_recording_redemption(&self) -> Result<(), Failure> {
        let hook = self
            .redemption_hook
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match hook {
            None => Ok(()),
            Some(RedemptionHook::Fail) => Err("injected registry failure".into()),
            Some(RedemptionHook::Pause(gate)) => {
                gate.reached.notify_one();
                gate.open.acquire().await?.forget();
                Ok(())
            }
        }
    }

    async fn released(&self, id: crate::InvitationId, node: PublicKey, error: Failure) -> Failure {
        let released = async {
            let _settings = self.settings.lock().await;
            let mut book = Book::read(&self.replica).await?;
            if book.release(id, node) {
                self.replica
                    .put_authority_state(AUTHORITY_INVITATIONS_KEY, &book.to_bytes()?, now()?)
                    .await?;
            }
            Ok::<_, Failure>(())
        }
        .await;
        match released {
            Ok(()) => error,
            Err(release) => {
                format!("{error}; the invitation stays redeemed by its node: {release}").into()
            }
        }
    }
}
