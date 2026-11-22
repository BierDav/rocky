use super::*;
use crate::invitation::{AUTHORITY_INVITATIONS_KEY, Book};

impl Node {
    pub(super) async fn invite(
        &self,
        identity: PublicKey,
        profile: Profile,
        terms: crate::InviteTerms,
    ) -> Result<crate::Invitation, Failure> {
        let routes = self.own_routes().await;
        if routes.is_empty() {
            return Err(ServiceError::NetworkRequired("invite an identity").into());
        }
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let issued = {
            let settings = self.settings.lock().await;
            let mut book = Book::read(&self.replica).await?;
            let time = now()?;
            let issued = settings
                .invite(
                    (&self.vault, routes),
                    &mut book,
                    (identity, profile.clone(), &terms),
                    &registry,
                    time,
                )
                .map_err(named)?;
            self.replica
                .put_authority_state(AUTHORITY_INVITATIONS_KEY, &book.to_bytes()?, time)
                .await?;
            issued
        };
        let sequence = self.activity.record(crate::ActivityKind::IdentityInvited {
            invitation: issued.id,
            identity,
            profile: profile.clone(),
            expires: issued.expires,
        });
        Ok(crate::Invitation {
            uri: issued.uri,
            id: issued.id,
            identity,
            profile,
            expires: issued.expires,
            sequence,
        })
    }

    pub(super) async fn withdraw_invitation(&self, id: crate::InvitationId) -> Result<(), Failure> {
        let settings = self.settings.lock().await;
        if !settings.issuer() {
            return Err(refused("only an issuer withdraws invitations"));
        }
        let mut book = Book::read(&self.replica).await?;
        let time = now()?;
        book.withdraw(id, time)?;
        self.replica
            .put_authority_state(AUTHORITY_INVITATIONS_KEY, &book.to_bytes()?, time)
            .await?;
        drop(settings);
        Ok(())
    }

    pub(super) async fn open_invitations(&self) -> Result<Vec<crate::OpenInvitation>, Failure> {
        let settings = self.settings.lock().await;
        if !settings.issuer() {
            return Err(refused("only an issuer holds invitations"));
        }
        let book = Book::read(&self.replica).await?;
        drop(settings);
        Ok(book.listed(now()?))
    }

    pub(super) async fn answer_invitation(
        &self,
        request: rocky_credentials::InvitedEnrollmentRequest,
    ) -> Result<IdentityReply, Failure> {
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
        self.commit(redeemed.changes).await?;
        self.activity
            .record(crate::ActivityKind::InvitationRedeemed {
                invitation: redeemed.id,
                identity: redeemed.device.identity,
                node: redeemed.device.node,
            });
        Ok(IdentityReply::Enrolled(redeemed.answer))
    }
}
