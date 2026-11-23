use rocky_core::Commitment;
use rocky_credentials::{DatabaseContext, PeerId, RevocationSet, SignedResetAdmission};
use rocky_secure::{PrivateKey, PublicKey};
use std::collections::BTreeSet;

pub(crate) fn admit_after_resets(
    known: &mut RevocationSet,
    key: &PrivateKey,
    database: DatabaseContext,
    node: PublicKey,
) -> Result<(), crate::Failure> {
    let heads = known.reset_heads();
    if heads.is_empty() || known.reset_ends(&PeerId::of(key.public_key())) {
        return Ok(());
    }
    known.admit(SignedResetAdmission::sign(
        key,
        database,
        PeerId::of(node),
        heads,
    )?)?;
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResetKeep {
    pub also: BTreeSet<PeerId>,
    pub except: BTreeSet<PeerId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    LocallyAnchored,
    FencedByOwner { reset: Commitment },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreChoice {
    WaitForOtherDevices,
    ThisIsEverything,
}
impl RestoreChoice {
    pub const DEFAULT: Self = Self::WaitForOtherDevices;
    pub const ALL: [Self; 2] = [Self::WaitForOtherDevices, Self::ThisIsEverything];
    pub fn title(self) -> &'static str {
        match self {
            Self::WaitForOtherDevices => "Wait for my other devices",
            Self::ThisIsEverything => "This is everything: sign out everywhere else",
        }
    }
    pub fn detail(self) -> &'static str {
        match self {
            Self::WaitForOtherDevices => {
                "This device may be missing newer changes or a removed device. \
                 They arrive when another of your devices or a backup comes online."
            }
            Self::ThisIsEverything => {
                "Every other device is signed out, including ones this device never saw. \
                 Anything that is not on this device now is discarded everywhere and cannot be \
                 recovered. Your custodians stay. Other devices must sign in again."
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResetReceipt {
    pub reset: Commitment,
    pub kept: BTreeSet<PeerId>,
    pub follows: BTreeSet<Commitment>,
    pub fenced_logs: usize,
    pub pushed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ResetRefused {
    DeviceSignedOut { statement: Commitment },
    CustodianSignedOut { custodian: PeerId },
    EndsSigner,
}
impl ResetRefused {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::DeviceSignedOut { .. } => "device_signed_out",
            Self::CustodianSignedOut { .. } => "custodian_signed_out",
            Self::EndsSigner => "reset_ends_signer",
        }
    }
}
impl std::fmt::Display for ResetRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeviceSignedOut { statement } => write!(
                f,
                "this device was signed out by {}; sign in again as a new device",
                data_encoding::HEXLOWER.encode(statement)
            ),
            Self::CustodianSignedOut { custodian } => {
                write!(f, "custodian {} was already signed out", custodian.to_hex())
            }
            Self::EndsSigner => f.write_str("the reset would sign out this device itself"),
        }
    }
}
impl std::error::Error for ResetRefused {}
