use super::*;
use crate::self_enrollment::{HeldLink, LinkRequest};
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
        if settings.tombstoned(&settings.identity)
            || settings
                .revocations
                .ends_node_of(settings.identity, request.node, false)
        {
            return Err(Box::new(crate::Error::removed()));
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
                .ok_or(crate::LinkError::UnknownRequest)?;
            if held.request != request_bytes || confirmed != code {
                return Err(Box::new(crate::LinkError::CodeMismatch));
            }
            if !held.confirmed {
                return Err(Box::new(crate::LinkError::NotApproved));
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
                HeldLink {
                    request: request_bytes,
                    code,
                    received: time,
                    confirmed: false,
                },
            );
        }
        Ok(rocky_credentials::IdentityReply::ConfirmationRequired)
    }
    pub(super) fn link_requests(&self) -> Vec<crate::LinkRequest> {
        let time = now().unwrap_or(0);
        self.link_requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(_, held)| time.saturating_sub(held.received) < SKEW)
            .map(|(node, held)| crate::LinkRequest {
                device: crate::DeviceId::new(*node),
                code: crate::LinkCode(held.code.clone()),
                received: crate::Timestamp::from_unix_micros(
                    i64::try_from(held.received)
                        .unwrap_or(i64::MAX)
                        .saturating_mul(1_000_000),
                ),
                approved: held.confirmed,
            })
            .collect()
    }
    pub(super) fn approve_link(
        &self,
        node: PublicKey,
        code: &crate::LinkCode,
    ) -> Result<(), Failure> {
        let mut requests = self
            .link_requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let held = requests
            .get_mut(&node)
            .ok_or(crate::LinkError::UnknownRequest)?;
        if now()?.saturating_sub(held.received) >= SKEW {
            return Err(Box::new(crate::LinkError::UnknownRequest));
        }
        if held.code != code.0 {
            return Err(Box::new(crate::LinkError::CodeMismatch));
        }
        held.confirmed = true;
        Ok(())
    }
}
