//! "This is everything: sign out everywhere else": the owner reset and
//! what an application shows around it.
use rocky_core::Commitment;
use rocky_credentials::{DatabaseContext, PeerId, RevocationSet, SignedResetAdmission};
use rocky_secure::{PrivateKey, PublicKey};
use std::collections::BTreeSet;

/// `key`, the node answering `node`'s invitation or link, admits it past
/// the resets `known` holds, so they do not end it. A node those resets
/// end admits nobody, and without a reset there is nothing to admit past.
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

/// Who a reset keeps besides the signing device: by default every
/// custodian this device knows of, since a server or custody replica the
/// reset leaves out stops syncing for good.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResetKeep {
    /// Further nodes to keep. Naming one a statement already ended refuses
    /// the reset.
    pub also: BTreeSet<PeerId>,
    /// Known custodians to sign out anyway.
    pub except: BTreeSet<PeerId>,
}

/// How far this device's view of the database is witnessed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    /// This device's own durable store anchors what it holds.
    LocallyAnchored,
    /// Restored from a source no other device witnessed: newer data or a
    /// revocation may be missing. Personal sign-in from a single custodian
    /// reports it; the current shared model has no such restore yet.
    Unwitnessed,
    /// The owner declared this device's state complete: `reset` ended every
    /// other node except named custodians and fenced their data.
    FencedByOwner { reset: Commitment },
}

/// The two answers an application offers while [`Freshness::Unwitnessed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreChoice {
    /// Keep working and let other devices bring what they hold. The default.
    WaitForOtherDevices,
    /// Call [`crate::Database::sign_out_everywhere_else`].
    ThisIsEverything,
}
impl RestoreChoice {
    pub const DEFAULT: Self = Self::WaitForOtherDevices;
    /// In display order, the default first.
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

/// What a reset recorded, once committed locally and offered to peers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResetReceipt {
    /// The signed statement's ID, which later resets follow.
    pub reset: Commitment,
    /// This node, the known custodians it did not except and `also`.
    pub kept: BTreeSet<PeerId>,
    /// The earlier resets it follows.
    pub follows: BTreeSet<Commitment>,
    /// Writer logs whose applied prefix the fence keeps.
    pub fenced_logs: usize,
    /// Whether every root statement reached the coordinator, if one is configured.
    pub pushed: bool,
}

/// Why [`crate::Database::sign_out_everywhere_else`] refused; nothing was
/// recorded. Callers see it as a [`crate::CallFailure`] with its code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ResetRefused {
    /// Statement `statement` already ended this device: sign in again as a
    /// new device.
    DeviceSignedOut { statement: Commitment },
    /// A held statement already ended the named custodian.
    CustodianSignedOut { custodian: PeerId },
    /// The reset would end this device's own credentials.
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
