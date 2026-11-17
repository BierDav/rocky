use crate::{Failure, PeerId, PersonalSignInError, Session, VerifiedPasskeyPrf, request::Request};
use rocky_core::{Commitment, Scope};
use rocky_credentials::PublicKey;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRevocationReceipt {
    pub node: PeerId,
    pub statement: Commitment,
    pub local: bool,
    pub delivered: BTreeSet<PeerId>,
    pub pending: BTreeSet<PeerId>,
}

impl Session {
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
    pub async fn read_personal_device_notice(&self, node: PeerId) -> Result<(), Failure> {
        self.send(|reply| Request::PersonalDeviceNotice(node, false, reply))
            .await
    }
    pub async fn acknowledge_personal_device_notice(&self, node: PeerId) -> Result<(), Failure> {
        self.send(|reply| Request::PersonalDeviceNotice(node, true, reply))
            .await
    }
    pub async fn revoke_personal_device(
        &self,
        node: PeerId,
        prf: VerifiedPasskeyPrf,
    ) -> Result<DeviceRevocationReceipt, PersonalSignInError> {
        self.send(|reply| Request::RevokePersonalDevice(node, prf, reply))
            .await
            .map_err(PersonalSignInError::from_failure)
    }
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
