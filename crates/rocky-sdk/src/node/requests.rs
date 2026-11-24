use super::*;
use crate::request::Request;

impl Node {
    pub(crate) async fn request(&self, request: Request) {
        self.signed(self.serve_request(request)).await;
    }
    async fn serve_request(&self, request: Request) {
        if let Err(error) = self.admit_request(&request).await {
            request.fail(error);
            return;
        }
        match request {
            Request::HeldChanges(reply) => {
                let _ = reply.send(self.replica.held_changes().await.map_err(Into::into));
            }
            Request::DropHeld(a, reply) => {
                let _ = reply.send(self.drop_held(a).await);
            }
            Request::Retention(a, b, c, reply) => {
                let _ = reply.send(self.retention(&a, b, c).await);
            }
            Request::RetentionLimits(a, reply) => {
                let _ = reply.send(
                    async {
                        if let Some(limits) = a {
                            self.replica.set_retention_limits(limits).await?;
                        }
                        Ok(self.replica.retention_limits().await)
                    }
                    .await,
                );
            }
            Request::AdminStaleAfter(a, reply) => {
                let _ = reply.send(self.set_admin_stale_after(a).await);
            }
            #[cfg(any(test, feature = "test-support"))]
            Request::Activity(a, reply) => {
                let _ = reply.send(Ok(self.activity.after(a)));
            }
            #[cfg(any(test, feature = "test-support"))]
            Request::ActivityNext(a, mut reply) => {
                let result = tokio::select! { result = self.next_activity(a) => result, () = reply.closed() => return, };
                let _ = reply.send(result);
            }
            Request::CreatePartition(partition, reply) => {
                let _ = reply.send(self.create_named_partition(&partition).await);
            }
            #[cfg(any(test, feature = "test-support"))]
            Request::PartitionStatus(a, reply) => {
                let _ = reply.send(self.partition_status(&a).await);
            }
            Request::Status(reply) => {
                let _ = reply.send(self.observed_status().await);
            }
            Request::StatusAfter(after, mut reply) => {
                let result = tokio::select! { result = self.readiness_after(&after) => result, () = reply.closed() => return, };
                let _ = reply.send(result);
            }
            Request::Sync(reply) => {
                let _ = reply.send(self.sync_once().await);
            }
            Request::FirstPull(mut reply) => {
                let mut pulled = self.first_pull.subscribe();
                let result = tokio::select! {
                    pulled = pulled.wait_for(|pulled| *pulled) => pulled.map(drop).map_err(Failure::from),
                    () = reply.closed() => return,
                };
                let _ = reply.send(result);
            }
            Request::SetSync(mode, reply) => {
                let _ = reply.send(self.set_sync(mode).await);
            }
            Request::Logout(logout, reply) => {
                let _ = reply.send(self.logout(logout).await);
            }
            #[cfg(any(test, feature = "test-support"))]
            Request::Tips(probe) => self.probe_tips(probe).await,
            Request::Me(reply) => {
                let _ = reply.send(self.me().await);
            }
            Request::People(reply) => {
                let _ = reply.send(self.people().await);
            }
            Request::SetAccess(person, access, reply) => {
                let _ = reply.send(self.set_access(person, access).await);
            }
            Request::GrantPartition(person, partition, reply) => {
                let _ = reply.send(self.grant_partition(person, &partition).await);
            }
            Request::RevokePartition(person, partition, reply) => {
                let _ = reply.send(self.revoke_partition(person, &partition).await);
            }
            Request::Remove(person, reply) => {
                let _ = reply.send(self.remove(person).await);
            }
            Request::MakeAdmin(person, root, reply) => {
                let _ = reply.send(self.make_admin(person, &root).await);
            }
            Request::RevokeAdmin(person, removal, root, reply) => {
                let _ = reply.send(self.revoke_admin(person, removal, &root).await);
            }
            Request::Invite(invite, reply) => {
                let _ = reply.send(self.invite(&invite).await);
            }
            Request::CancelInvitation(id, reply) => {
                let _ = reply.send(self.cancel_invitation(id).await);
            }
            Request::Invitations(reply) => {
                let _ = reply.send(self.invitations().await);
            }
            #[cfg(any(test, feature = "test-support"))]
            Request::Renew(reply) => {
                let _ = reply.send(self.renew_now().await);
            }
            Request::LinkRequests(reply) => {
                let _ = reply.send(Ok(self.link_requests()));
            }
            Request::ApproveLink(node, code, reply) => {
                let _ = reply.send(self.approve_link(node, &code));
            }
            Request::Contact(reply) => {
                let _ = reply.send(self.contact_info().await);
            }
            Request::AddPeer(contact, reply) => {
                let _ = reply.send(self.add_peer(&contact).await);
            }
            Request::RemovePeer(node, reply) => {
                let _ = reply.send(self.remove_peer(node).await);
            }
            Request::Unlink(node, reply) => {
                let _ = reply.send(self.unlink(node).await.map(drop));
            }
            Request::NextPartition(table, reply) => {
                let _ = reply.send(self.next_partition(&table).await);
            }
            Request::DeviceCredential(reply) => {
                let _ = reply.send(self.device_credential().await);
            }
            Request::SignExtension(signing, reply) => {
                let _ = reply.send(Settings::node_key(&self.vault).map(|node| node.sign(&signing)));
            }
        }
    }

    async fn device_credential(&self) -> Result<crate::DeviceCredential, Failure> {
        let node = Settings::node_key(&self.vault)?.public_key();
        let revocations = self.replica.revocations().await;
        let settings = self.settings.lock().await;
        Ok(crate::DeviceCredential {
            scope: self.domain().scope,
            application: settings.schema.application.clone(),
            publisher: settings.publisher,
            identity: settings.identity,
            node,
            chain: settings.credentials.clone(),
            revocations,
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    async fn next_activity(&self, after: u64) -> Result<crate::activity::ActivityPage, Failure> {
        let mut closing = self.closing.subscribe();
        self.activity
            .next(after, &mut closing)
            .await
            .ok_or_else(|| ServiceError::Closed("database").into())
    }
    pub(super) async fn sync_once(&self) -> Result<crate::SyncReport, Failure> {
        let attached = self.network.read().await;
        let transport = attached.as_ref().map(|link| &*link.transport);
        let work = async {
            let (report, _) = self.sync_optional(transport).await?;
            if report.reconciled() == 0 {
                return Err(ServiceError::NoPeerReached {
                    known: report.peers.len(),
                }
                .into());
            }
            Ok(report)
        };
        let Some(link) = attached.as_ref() else {
            return work.await;
        };
        let mut stage = link.stage.subscribe();
        tokio::select! {
            result = work => result,
            _ = stage.wait_for(|stage| *stage == Stage::Cancelled) => Err(ServiceError::Closed("network service").into()),
        }
    }
    pub(super) async fn set_sync(&self, mode: crate::SyncMode) -> Result<(), Failure> {
        match mode {
            crate::SyncMode::Off => {
                self.set_automatic_sync(false).await?;
                self.set_networking_enabled(false).await
            }
            crate::SyncMode::Manual => {
                self.set_networking_enabled(true).await?;
                self.set_automatic_sync(false).await
            }
            crate::SyncMode::Live => {
                self.set_networking_enabled(true).await?;
                self.set_automatic_sync(true).await
            }
        }
    }
    pub(crate) fn start_in(&self, mode: crate::SyncMode) {
        self.online
            .store(mode != crate::SyncMode::Off, Ordering::SeqCst);
        self.automatic
            .store(mode == crate::SyncMode::Live, Ordering::SeqCst);
    }
    pub(super) fn sync_mode(&self) -> crate::SyncMode {
        if !self.online.load(Ordering::SeqCst) {
            crate::SyncMode::Off
        } else if self.automatic.load(Ordering::SeqCst) {
            crate::SyncMode::Live
        } else {
            crate::SyncMode::Manual
        }
    }
    pub(super) async fn set_automatic_sync(&self, enabled: bool) -> Result<(), Failure> {
        self.automatic.store(enabled, Ordering::SeqCst);
        if enabled {
            self.reset_retry(None);
        }
        self.control_sync();
        if !enabled {
            if let Some(service) = self.current() {
                service
                    .live
                    .close(rocky_secure::network::link::CloseReason::WorkerDisabled);
            }
            let mut running = self.worker_running.subscribe();
            running.wait_for(|running| !running).await?;
            if let Some(service) = self.current() {
                service.live.wait_empty().await;
            }
            self.synced(SyncStatus::new(SyncState::Idle));
        }
        Ok(())
    }
    pub(super) async fn set_networking_enabled(&self, enabled: bool) -> Result<(), Failure> {
        if self.online.swap(enabled, Ordering::SeqCst) != enabled {
            self.activity.record(if enabled {
                crate::activity::ActivityKind::NetworkingEnabled
            } else {
                crate::activity::ActivityKind::NetworkingDisabled
            });
        }
        if enabled {
            self.reset_retry(None);
            if self.sync_status.borrow().state == SyncState::NetworkingDisabled {
                self.synced(SyncStatus::new(SyncState::Idle));
            }
        }
        if !enabled && let Some(service) = self.current() {
            service
                .live
                .close(rocky_secure::network::link::CloseReason::NetworkingDisabled);
        }
        self.control_sync();
        if !enabled && let Some(service) = self.current() {
            service.live.wait_empty().await;
        }
        Ok(())
    }
}
