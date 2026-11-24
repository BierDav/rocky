use super::*;
use crate::request::Request;

impl Node {
    pub(crate) async fn request(&self, request: Request) {
        if let Err(error) = self.admit_request(&request).await {
            request.fail(error);
            return;
        }
        match request {
            Request::WithIdentity(loan, request) => {
                Box::pin(loan.supply(self.request(*request))).await;
            }
            Request::Identities(reply) => {
                let _ = reply.send(self.identities().await);
            }
            Request::IdentityLabel(a, b, reply) => {
                let _ = reply.send(self.label(a, b).await);
            }
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
            Request::TrustedStaleAfter(a, reply) => {
                let _ = reply.send(self.set_trusted_stale_after(a).await);
            }
            Request::Activity(a, reply) => {
                let _ = reply.send(Ok(self.activity.after(a)));
            }
            Request::ActivityNext(a, mut reply) => {
                let result = tokio::select! { result = self.next_activity(a) => result, () = reply.closed() => return, };
                let _ = reply.send(result);
            }
            Request::PartitionAddress(a, b, reply) => {
                let _ = reply.send(self.partition_address(&a, b).await);
            }
            Request::PartitionComplete(a, reply) => {
                let _ = reply.send(self.partition_complete(&a).await);
            }
            Request::CreatePartition(a, reply) => {
                let _ = reply.send(self.create_partition(&a).await);
            }
            Request::InstallPartition(a, b, c, reply) => {
                let _ = reply.send(self.install_partition(&a, &b, &c).await);
            }
            Request::PartitionStatus(a, reply) => {
                let _ = reply.send(self.partition_status(&a).await);
            }
            Request::RegisterPartition(a, b, reply) => {
                let _ = reply.send(self.register_partition(&a, &b).await);
            }
            Request::HoldPartition(a, b, c, reply) => {
                let _ = reply.send(self.hold_partition(&a, &b, c).await);
            }
            Request::ReleasePartition(a, b, reply) => {
                let _ = reply.send(self.release_partition(&a, &b).await);
            }
            Request::EvictPartition(a, b, reply) => {
                let _ = reply.send(self.evict_partition(&a, &b).await);
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
            Request::SetSync(mode, reply) => {
                let _ = reply.send(self.set_sync(mode).await);
            }
            Request::Logout(purge, reply) => {
                let _ = reply.send(self.logout(purge).await);
            }
            #[cfg(any(test, feature = "test-support"))]
            Request::Tips(probe) => self.probe_tips(probe).await,
            Request::InstallTrust(grant, reply) => {
                let _ = reply.send(self.install_trust(&grant).await);
            }
            Request::Trust(identity, root, reply) => {
                let _ = reply.send(self.trust_identity(identity, &root).await);
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
            Request::DeviceRevocationReceipt(node, reply) => {
                let _ = reply.send(Ok(self
                    .settings
                    .lock()
                    .await
                    .device_revocations
                    .get(&node)
                    .cloned()));
            }
            Request::Unlink(node, reply) => {
                let _ = reply.send(self.unlink(node).await.map(drop));
            }
            Request::RevokeIdentity(identity, mode, root, keep, reply) => {
                let _ = reply.send(self.revoke_identity(identity, mode, &root, keep).await);
            }
            Request::Info(reply) => {
                let _ = reply.send(self.device_info().await);
            }
            Request::PartitionCustody(scope, policy, reply) => {
                let result = self
                    .replica
                    .set_partition_custody(scope, policy)
                    .await
                    .map_err(Into::into);
                self.wake_sync();
                let _ = reply.send(result);
            }
            Request::ReplicaRole(role, reply) => {
                let _ = reply.send(self.set_replica_role(role).await);
            }
            Request::NextPartition(table, reply) => {
                let _ = reply.send(self.next_partition(&table).await);
            }
            Request::PartitionRecipients(target, reply) => {
                let _ = reply.send(self.partition_recipients(&target).await);
            }
            Request::GrantPartition(identity, target, reply) => {
                let _ = reply.send(self.grant_partition(identity, &target).await);
            }
            Request::SharePartition(target, recipients, epoch, reply) => {
                let _ = reply.send(self.share_partition(&target, recipients, epoch).await);
            }
            Request::IdentityForNode(node, reply) => {
                let _ = reply.send(self.identity_for_node(node).await);
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
            publisher: settings.publisher,
            identity: settings.identity,
            node,
            chain: settings.credentials.clone(),
            revocations,
        })
    }

    async fn next_activity(&self, after: u64) -> Result<crate::ActivityPage, Failure> {
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
            self.sync_optional(transport)
                .await
                .map(|(report, _)| report)
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
                self.enter_foreground().await?;
                self.set_automatic_sync(false).await
            }
            crate::SyncMode::Foreground => {
                self.set_networking_enabled(true).await?;
                self.enter_foreground().await?;
                self.set_automatic_sync(true).await
            }
            crate::SyncMode::Background => {
                self.set_networking_enabled(true).await?;
                self.set_automatic_sync(true).await?;
                self.enter_background().await
            }
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
                crate::ActivityKind::NetworkingEnabled
            } else {
                crate::ActivityKind::NetworkingDisabled
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
    pub(super) async fn enter_foreground(&self) -> Result<(), Failure> {
        self.foreground.store(true, Ordering::SeqCst);
        self.discovery_foreground(true).await;
        self.reset_retry(None);
        self.wake_sync();
        Ok(())
    }
    pub(super) async fn enter_background(&self) -> Result<(), Failure> {
        self.foreground.store(false, Ordering::SeqCst);
        self.control_sync();
        let mut running = self.worker_running.subscribe();
        running.wait_for(|running| !running).await?;
        if let Some(service) = self.current() {
            service
                .live
                .close(rocky_secure::network::link::CloseReason::Suspended);
            service.live.wait_empty().await;
        }
        self.discovery_foreground(false).await;
        Ok(())
    }
}
