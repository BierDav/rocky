//! Signed header placements: positions of a seal's frontier in scopes the
//! requester cannot hold, shown by their author-signed headers alone. See
//! `docs/runtime/lazy-compaction.md`.
use super::*;
use crate::replica::{PlacementPage, PlacementRequest};

#[derive(Serialize, Deserialize)]
pub(super) enum PlacementMessage {
    Request(PlacementRequest),
    Page(PlacementPage),
}

impl NativeSession {
    /// Asks the peer to place what this replica wants placed. Returns how
    /// many placements it stored.
    pub async fn pull_placements<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
    ) -> Result<usize, Error> {
        let mut stored = 0;
        for request in replica.wanted_placements().await? {
            if self.low(MAX_SECURE_FRAME) {
                break;
            }
            self.send(PlacementMessage::Request(request)).await?;
            let PlacementMessage::Page(page) = self.reply::<PlacementMessage>().await? else {
                return Err(denied("unexpected placement response"));
            };
            stored += replica.receive_placements(request, page).await?;
        }
        Ok(stored)
    }
    pub(super) async fn answer_placements<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        message: PlacementMessage,
    ) -> Result<(), Error> {
        let PlacementMessage::Request(request) = message else {
            return Err(denied("unexpected placement request"));
        };
        let page = replica
            .placement_page(self.peer, &self.credentials, &self.proved, request)
            .await?;
        self.send(PlacementMessage::Page(page)).await
    }
}
