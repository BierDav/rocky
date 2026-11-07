use super::*;
use crate::self_enrollment::{LinkRequest, PendingLink};
use rocky_credentials::IdentityRequest;
const SKEW: u64 = 300;
impl Node {
    pub(super) fn answer_link(
        &self,
        settings: &Settings,
        bytes: &[u8],
    ) -> Result<Vec<u8>, Failure> {
        let wire: LinkRequest = serde_json::from_slice(bytes)?;
        let request = IdentityRequest::from_bytes(&wire.request)?;
        let time = now()?;
        request.verify(
            Settings::node_key(&self.vault)?.public_key(),
            settings.identity,
            (time, SKEW),
            settings.database_context()?,
        )?;
        if settings.single_use
            || settings.revocations.revokes(&settings.identity)
            || settings
                .revocations
                .ends_node_of(settings.identity, request.node, false)
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
            if !held.confirmed || held.request != wire.request || confirmed != code {
                return Err("link code is wrong or not confirmed on both devices".into());
            }
            drop(requests);
            return settings.answer_identity(&self.vault, &wire.request);
        }
        if !requests
            .get(&request.node)
            .is_some_and(|held| held.request == wire.request)
        {
            if requests.len() >= crate::join_requests::MAX_PENDING
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
                    request: wire.request,
                    code,
                    received: time,
                    confirmed: false,
                },
            );
        }
        Ok(b"confirmation required".to_vec())
    }
    pub(super) async fn confirm_link(
        &self,
        request: &serde_json::Value,
    ) -> Result<serde_json::Value, Failure> {
        let node: rocky_secure::PublicKey =
            request["node"].as_str().ok_or("missing node")?.parse()?;
        let code = request["code"].as_str().ok_or("missing code")?;
        let mut requests = self
            .link_requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let held = requests.get_mut(&node).ok_or("missing link request")?;
        if now()?.saturating_sub(held.received) >= SKEW || held.code != code {
            return Err("link code is wrong or expired".into());
        }
        held.confirmed = true;
        Ok(serde_json::json!({}))
    }
}
