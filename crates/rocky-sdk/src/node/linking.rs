use super::*;
use crate::self_enrollment::{LinkRequest, PendingLink};
const SKEW: u64 = 300;
impl Node {
    pub(super) fn answer_link(
        &self,
        settings: &Settings,
        wire: LinkRequest,
    ) -> Result<rocky_credentials::IdentityReply, Failure> {
        let request = &wire.request;
        let request_bytes = request.to_bytes()?;
        let time = now()?;
        request.verify(
            Settings::node_key(&self.vault)?.public_key(),
            settings.identity,
            (time, SKEW),
            settings.database_context()?,
        )?;
        if settings.single_use
            || settings.tombstoned(&settings.identity)
            || settings
                .revocations
                .unfenced()
                .ends_node_of(settings.identity, request.node, false)
            || settings.signed_out(&request.node)
        {
            return Err("identity or node cannot link".into());
        }
        let code = request.confirmation_code()?;
        let mut requests = self
            .link_requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        requests.retain(|_, held| time.saturating_sub(held.received) < SKEW);
        if let Some(confirmed) = wire.code {
            let held = requests
                .get(&request.node)
                .ok_or("link request expired or missing")?;
            if !held.confirmed || held.request != request_bytes || confirmed != code {
                return Err("link code is wrong or not confirmed on both devices".into());
            }
            drop(requests);
            return Ok(rocky_credentials::IdentityReply::Enrolled(
                settings.answer_identity(&self.vault, &request_bytes)?,
            ));
        }
        if !requests
            .get(&request.node)
            .is_some_and(|held| held.request == request_bytes)
        {
            if requests.len() >= 32
                && let Some(oldest) = requests
                    .iter()
                    .min_by_key(|(_, held)| held.received)
                    .map(|(node, _)| *node)
            {
                requests.remove(&oldest);
            }
            requests.insert(
                request.node,
                PendingLink {
                    request: request_bytes,
                    code,
                    received: time,
                    confirmed: false,
                },
            );
        }
        Ok(rocky_credentials::IdentityReply::ConfirmationRequired)
    }
    pub(super) async fn confirm_link(&self, node: PublicKey, code: &str) -> Result<(), Failure> {
        let mut requests = self
            .link_requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let held = requests.get_mut(&node).ok_or("missing link request")?;
        if now()?.saturating_sub(held.received) >= SKEW || held.code != code {
            return Err("link code is wrong or expired".into());
        }
        held.confirmed = true;
        Ok(())
    }
}
