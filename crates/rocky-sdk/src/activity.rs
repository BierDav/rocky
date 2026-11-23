use rocky_credentials::PublicKey;
use std::collections::VecDeque;
use tokio::sync::watch;

#[derive(Clone, Debug, PartialEq)]
pub struct ActivityScope {
    pub table: Option<String>,
    pub partition: Option<rocky_core::Commitment>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct ActivityTransfer {
    pub scope: ActivityScope,
    pub pulled: u64,
    pub pushed: u64,
    pub bytes: u64,
}
#[derive(Clone, Debug, PartialEq)]
pub enum ActivityKind {
    LinkAdmissionFailed {
        error: String,
    },
    DeviceProvisioned {
        node: PublicKey,
        identity: PublicKey,
        profile: crate::GrantProfile,
    },
    IdentityInvited {
        invitation: crate::InvitationId,
        identity: PublicKey,
        profile: crate::GrantProfile,
        expires: u64,
    },
    InvitationRedeemed {
        invitation: crate::InvitationId,
        identity: PublicKey,
        node: PublicKey,
    },
    PeerAdded {
        peer: PublicKey,
    },
    PeerRemoved {
        peer: PublicKey,
    },
    Renewed {
        revocations: crate::RevocationStatus,
        expires: u64,
        keys: usize,
    },
    IdentityRevoked {
        identity: PublicKey,
    },
    NodeUnlinked {
        identity: PublicKey,
        node: PublicKey,
    },
    RevocationCutDroppedStages {
        identity: Option<PublicKey>,
        count: usize,
        heads: Vec<crate::DroppedStage>,
    },
    RevocationSealFailed {
        error: String,
    },
    LineNodeFailed {
        error: String,
    },
    RegistryFailed {
        error: String,
    },
    PeerAttemptFailed {
        peer: PublicKey,
        error: String,
    },
    PeerReconciliationCompleted {
        peer: PublicKey,
    },
    ScopesRefused {
        peer: PublicKey,
        scopes: Vec<ActivityScope>,
    },
    ScopesWaiting {
        peer: PublicKey,
        scopes: Vec<ActivityScope>,
    },
    Synced {
        peer: PublicKey,
        pulled: u64,
        pushed: u64,
        scopes: Vec<ActivityTransfer>,
    },
    CheckpointImported {
        scope: ActivityScope,
        peer: PublicKey,
        checkpoint: rocky_core::Commitment,
        superseded: u64,
    },
    RenewalFailed {
        error: String,
    },
    EpochDeliveryFailed {
        peer: PublicKey,
        error: String,
    },
    BootstrapStarted {
        peer: PublicKey,
        scopes: usize,
    },
    BootstrapFinished {
        peer: PublicKey,
    },
    IdentityAdmitted {
        identity: PublicKey,
        profile: crate::GrantProfile,
    },
    NodeLinked {
        identity: PublicKey,
        node: PublicKey,
    },
    MemberSealFailed {
        error: String,
    },
    ReplayedAwaitingOutcomes {
        transactions: usize,
    },
    RoutineSealFailed {
        error: String,
    },
    LocalRecoveryFailed {
        error: String,
    },
    RoleChanged {
        partial: bool,
    },
    PartitionCreated {
        target: crate::PartitionTarget,
        id: Option<rocky_core::RowIdentity>,
    },
    PartitionShared {
        target: crate::PartitionTarget,
        recipients: usize,
    },
    EnvelopesDelivered {
        peer: PublicKey,
        handed: u64,
        deposited: u64,
        refused: u64,
        received: u64,
        receipts: u64,
    },
    EnvelopeRefused {
        peer: PublicKey,
        error: String,
    },
    PartitionKeyInstalled {
        target: crate::PartitionTarget,
    },
    PartitionBootstrap {
        target: crate::PartitionTarget,
    },
    PartitionSubscribed {
        target: crate::PartitionTarget,
    },
    PartitionUnsubscribed {
        target: crate::PartitionTarget,
    },
    PartitionEvicted {
        target: crate::PartitionTarget,
    },
    UnlinkUnpushed {
        error: String,
    },
    MembershipLinkRecorded {
        identity: PublicKey,
        node: rocky_credentials::PeerId,
        sequence: u64,
    },
    NetworkingEnabled,
    NetworkingDisabled,
    LinkSignalFailed {
        peer: PublicKey,
        error: String,
    },
    LinkAttemptFailed {
        peer: PublicKey,
        error: String,
    },
    LinkClosed {
        peer: PublicKey,
        reason: String,
        error: Option<String>,
    },
    LinkOpened {
        peer: PublicKey,
        reason: String,
    },
    FinalizationFailed {
        error: String,
    },
    TipSchemaUnavailable {
        manifest: rocky_core::Commitment,
        peer: PublicKey,
        error: String,
    },
    SchemaPublicationConflict {
        publication: rocky_core::Commitment,
        terminal: rocky_core::Commitment,
    },
}
impl ActivityKind {
    fn same_kind(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct ActivityEvent {
    pub sequence: u64,
    pub at_epoch_millis: u64,
    pub detail: ActivityKind,
}
#[derive(Clone, Debug, PartialEq)]
pub struct ActivityPage {
    pub events: Vec<ActivityEvent>,
    pub token: u64,
}
const KEPT: usize = 512;
const PAGE: usize = 128;
struct Log {
    next: u64,
    events: VecDeque<ActivityEvent>,
}
pub(crate) struct Activity(watch::Sender<Log>);
impl Activity {
    pub fn new() -> Self {
        Self(
            watch::channel(Log {
                next: 1,
                events: VecDeque::new(),
            })
            .0,
        )
    }
    pub fn record(&self, detail: ActivityKind) -> u64 {
        let at_epoch_millis = rocky_platform::time::since_epoch()
            .map(|elapsed| elapsed.as_millis() as u64)
            .unwrap_or_default();
        let mut sequence = 0;
        self.0.send_modify(|log| {
            sequence = log.next;
            log.events.push_back(ActivityEvent {
                sequence,
                at_epoch_millis,
                detail,
            });
            log.next += 1;
            if log.events.len() > KEPT {
                log.events.pop_front();
            }
        });
        sequence
    }
    pub fn record_changed(&self, detail: ActivityKind) {
        let repeated = self
            .0
            .borrow()
            .events
            .iter()
            .rev()
            .find(|event| event.detail.same_kind(&detail))
            .is_some_and(|event| event.detail == detail);
        if !repeated {
            self.record(detail);
        }
    }
    pub fn after(&self, after: u64) -> ActivityPage {
        let log = self.0.borrow();
        let events: Vec<_> = log
            .events
            .iter()
            .filter(|event| event.sequence > after)
            .take(PAGE)
            .cloned()
            .collect();
        let token = events
            .last()
            .map_or(after.max(log.next - 1), |event| event.sequence);
        ActivityPage { events, token }
    }
    pub async fn next(
        &self,
        after: u64,
        closing: &mut watch::Receiver<bool>,
    ) -> Option<ActivityPage> {
        let mut log = self.0.subscribe();
        loop {
            if log.borrow_and_update().next - 1 > after {
                return Some(self.after(after));
            }
            tokio::select! { changed = log.changed() => changed.ok()?, _ = closing.wait_for(|closing| *closing) => return None, }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn pages_are_ordered_bounded_and_resume_after_the_token() {
        let activity = Activity::new();
        for transactions in 0..(KEPT + 10) {
            activity.record(ActivityKind::ReplayedAwaitingOutcomes { transactions });
        }
        let first = activity.after(0);
        assert_eq!(first.events.len(), PAGE);
        assert_eq!(
            first.events[0].detail,
            ActivityKind::ReplayedAwaitingOutcomes { transactions: 10 }
        );
        assert_eq!(
            activity.after(first.token).events[0].detail,
            ActivityKind::ReplayedAwaitingOutcomes {
                transactions: 10 + PAGE
            }
        );
        let (_, mut closing) = watch::channel(false);
        let last = (KEPT + 10) as u64;
        let waiting = activity.next(last, &mut closing);
        activity.record(ActivityKind::NetworkingEnabled);
        let next = waiting.await.unwrap();
        assert_eq!(next.events[0].detail, ActivityKind::NetworkingEnabled);
        assert_eq!(next.token, last + 1);
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_repeated_event_is_kept_once() {
        let activity = Activity::new();
        activity.record_changed(ActivityKind::ReplayedAwaitingOutcomes { transactions: 2 });
        activity.record_changed(ActivityKind::ReplayedAwaitingOutcomes { transactions: 2 });
        activity.record_changed(ActivityKind::ReplayedAwaitingOutcomes { transactions: 1 });
        assert_eq!(activity.after(0).events.len(), 2);
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn an_empty_log_answers_its_current_token() {
        let activity = Activity::new();
        assert_eq!(
            activity.after(0),
            ActivityPage {
                events: Vec::new(),
                token: 0
            }
        );
        activity.record(ActivityKind::NetworkingEnabled);
        assert_eq!(activity.after(5).token, 5);
    }
}
