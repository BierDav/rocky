//! Local notices and passkey authority for personal device actions.
use crate::{Failure, PeerId, PersonalSignInError, Session, VerifiedPasskeyPrf, request::Request};
use rocky_core::{Commitment, Scope};
use rocky_credentials::PublicKey;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A verified sign-in observed in this store. Device-provided names and
/// timestamps are deliberately absent; `node` is the stable fingerprint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonalDeviceNotice {
    pub scope: Scope,
    pub owner: PublicKey,
    pub node: PeerId,
    pub first_observed: u64,
    pub read: bool,
    pub acknowledged: bool,
    pub unlinked: Option<u64>,
}

/// Durable local outcome and delivery coverage. Pending peers may be
/// offline; an acknowledged delivery proves receipt, not global freshness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRevocationReceipt {
    pub node: PeerId,
    pub statement: Commitment,
    /// The statement and exposed epochs have committed locally.
    pub local: bool,
    pub delivered: BTreeSet<PeerId>,
    pub pending: BTreeSet<PeerId>,
}

impl Session {
    /// Read the durable outcome without another interactive authorization.
    pub async fn device_revocation_receipt(
        &self,
        node: PeerId,
    ) -> Result<Option<DeviceRevocationReceipt>, Failure> {
        self.send(|reply| Request::DeviceRevocationReceipt(node, reply))
            .await
    }
    pub async fn personal_device_notices(&self) -> Result<Vec<PersonalDeviceNotice>, Failure> {
        self.send(Request::PersonalDeviceNotices).await
    }
    /// Mark read without dismissing the notice.
    pub async fn read_personal_device_notice(&self, node: PeerId) -> Result<(), Failure> {
        self.send(|reply| Request::PersonalDeviceNotice(node, false, reply))
            .await
    }
    /// Dismiss the notice, retaining its deduplication and read state.
    pub async fn acknowledge_personal_device_notice(&self, node: PeerId) -> Result<(), Failure> {
        self.send(|reply| Request::PersonalDeviceNotice(node, true, reply))
            .await
    }
    /// The platform adapter obtains a fresh UV-verified PRF. Revoke exactly
    /// this owner node with Lost, preserving siblings and the held tip cut.
    /// This cannot exclude someone holding the passkey or an exported root.
    pub async fn revoke_personal_device(
        &self,
        node: PeerId,
        prf: VerifiedPasskeyPrf,
    ) -> Result<DeviceRevocationReceipt, PersonalSignInError> {
        self.send(|reply| Request::RevokePersonalDevice(node, prf, reply))
            .await
            .map_err(PersonalSignInError::from_failure)
    }
    /// Explicit destructive owner reset using fresh passkey authority.
    pub async fn sign_out_personal_devices(
        &self,
        prf: VerifiedPasskeyPrf,
        keep: crate::ResetKeep,
    ) -> Result<crate::ResetReceipt, PersonalSignInError> {
        self.send(|reply| Request::PersonalReset(prf, keep, reply))
            .await
            .map_err(PersonalSignInError::from_failure)
    }
}
