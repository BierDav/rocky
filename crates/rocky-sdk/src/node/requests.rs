use super::*;
use crate::request::Request;

impl Node {
    pub(crate) async fn request(&self, request: Request, client: Result<Arc<Client>, Failure>) {
        let client = match client {
            Ok(client) => client,
            Err(error) => {
                request.fail(error);
                return;
            }
        };
        if let Err(error) = self.admit_request(request.kind()).await {
            request.fail(error);
            return;
        }
        match request {
            Request::WithIdentity(loan, request) => {
                Box::pin(loan.supply(self.request(*request, Ok(client)))).await;
            }
            Request::Domain(reply) => {
                let _ = reply.send(self.domain_info().await);
            }
            Request::Generation(reply) => {
                let _ = reply.send(self.generation().await);
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
            Request::Coordinated(reply) => {
                let _ = reply.send(self.coordinated_status().await);
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
                let result = tokio::select! { result = self.next_activity(&client, a) => result, () = reply.closed() => return, };
                let _ = reply.send(result);
            }
            Request::IssuePartitions(a, b, c, d, reply) => {
                let _ = reply.send(self.issue_partitions(a, b, c, d).await);
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
            Request::DiscoveryTopics(reply) => {
                let _ = reply.send(
                    async {
                        let time = now()?;
                        let (current, previous) = match self.settings.try_lock() {
                            Ok(settings) => settings.advance_discovery_topics(&self.vault, time)?,
                            Err(_) => self
                                .settings
                                .published()
                                .discovery_topics(&self.vault, time)?,
                        };
                        Ok(crate::DiscoveryTopics { current, previous })
                    }
                    .await,
                );
            }
            Request::ResolveContact(node, reply) => {
                let _ = reply.send(
                    async {
                        let attached = self.network.read().await;
                        let transport = &*attached
                            .as_ref()
                            .ok_or(ServiceError::NetworkRequired("contact"))?
                            .transport;
                        self.request_contact(transport, node).await
                    }
                    .await,
                );
            }
            Request::Network(reply) => {
                let _ = reply.send(self.observed_network().await);
            }
            Request::Peers(reply) => {
                let _ = reply.send(self.observed_network().await.map(|network| network.peers));
            }
            Request::Status(reply) => {
                let _ = reply.send(self.observed_status().await);
            }
            Request::StatusAfter(after, mut reply) => {
                let result = tokio::select! { result = self.readiness_after(&client, &after) => result, () = reply.closed() => return, };
                let _ = reply.send(result);
            }
            Request::Sync(reply) => {
                let _ = reply.send(self.sync_once().await);
            }
            Request::NetworkingEnabled(enabled, reply) => {
                let _ = reply.send(self.set_networking_enabled(enabled).await);
            }
            Request::AutomaticSync(enabled, reply) => {
                let _ = reply.send(self.set_automatic_sync(enabled).await);
            }
            Request::Foreground(reply) => {
                let _ = reply.send(self.enter_foreground().await);
            }
            Request::Background(reply) => {
                let _ = reply.send(self.enter_background().await);
            }
            Request::Logout(purge, reply) => {
                let _ = reply.send(self.logout(purge).await);
            }
            Request::ChangeTip(change, root, reply) => {
                let _ = reply.send(self.change_tip(change, &root).await);
            }
            Request::InspectTips(reply) => {
                let _ = reply.send(self.inspect_tips(self.domain().scope).await);
            }
            Request::KeptSeals(reply) => {
                let _ = reply.send(self.kept_seals().await);
            }
            Request::SetKeptSeals(config, reply) => {
                let _ = reply.send(self.set_kept_seals(config).await);
            }
            Request::Compaction(reply) => {
                let _ = reply.send(self.compaction(self.domain().scope, 0, Vec::new()).await);
            }
            Request::Compact(keep, reply) => {
                let _ = reply.send(self.compact_tips(keep).await);
            }
            Request::OpenTip(seal, minimum, reply) => {
                let _ = reply.send(
                    self.open_tip(self.domain().scope, seal, minimum)
                        .await
                        .map(|tip| HEXLOWER.encode(&tip.0)),
                );
            }
            Request::Trust(identity, root, reply) => {
                let _ = reply.send(self.trust_identity(identity, &root).await);
            }
            Request::GrantClaims(identity, counters, delegations, reply) => {
                let _ = reply.send(self.grant_claims(identity, counters, delegations).await);
            }
            Request::InstallClaims(grant, reply) => {
                let _ = reply.send(self.install_claims(grant).await);
            }
            Request::HeldClaims(reply) => {
                let _ = reply.send(self.held_claims().await);
            }
            Request::RevokeClaims(identity, reply) => {
                let _ = reply.send(self.revoke_claims(identity).await.map(drop));
            }
            Request::Provision(profile, label, reply) => {
                let _ = reply.send(self.provision(profile, label).await);
            }
            Request::Renew(reply) => {
                let _ = reply.send(self.renew_now().await);
            }
            Request::Discovered(reply) => {
                let _ = reply.send(self.discovered().await);
            }
            Request::ConfirmLink(node, code, reply) => {
                let _ = reply.send(self.confirm_link(node, &code).await);
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
            Request::EnrollmentUpstream(contact, reply) => {
                let _ = reply.send(self.enrollment_upstream(contact.as_deref()).await);
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
            Request::PersonalDeviceNotices(reply) => {
                let _ = reply.send(self.personal_device_notices().await);
            }
            Request::PersonalDeviceNotice(node, acknowledge, reply) => {
                let _ = reply.send(self.personal_device_notice(node, acknowledge).await);
            }
            Request::RevokePersonalDevice(node, prf, reply) => {
                let _ = reply.send(self.revoke_personal_device(node, prf).await);
            }
            Request::PersonalReset(prf, keep, reply) => {
                let _ = reply.send(self.reset_personal(prf, keep).await);
            }
            Request::Unlink(node, reply) => {
                let _ = reply.send(self.unlink(node).await.map(drop));
            }
            Request::Backup(target, reply) => {
                let _ = reply.send(self.backup(&target).await);
            }
            Request::RevokeIdentity(identity, mode, root, keep, reply) => {
                let _ = reply.send(self.revoke_identity(identity, mode, &root, keep).await);
            }
            Request::SignOutEverywhereElse(root, keep, reply) => {
                let _ = reply.send(self.sign_out_everywhere_else(&root, keep).await);
            }
            Request::Freshness(reply) => {
                let _ = reply.send(self.freshness().await);
            }
            Request::Info(reply) => {
                let _ = reply.send(self.session_info().await);
            }
            Request::Grant(device, profile, terms, delegate, reply) => {
                let _ = reply.send(self.grant(device, profile, terms, delegate).await);
            }
            Request::Install(supplied, reply) => {
                let _ = reply.send(self.install_supplied(*supplied).await);
            }
            Request::SignCoordinationGrant(tenant, lifetime, root, reply) => {
                let _ = reply.send(self.sign_coordination_grant(tenant, lifetime, &root).await);
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
            Request::SharePartition(target, recipients, epoch, reply) => {
                let _ = reply.send(self.share_partition(&target, recipients, epoch).await);
            }
            Request::IdentityForNode(node, reply) => {
                let _ = reply.send(self.identity_for_node(node).await);
            }
        }
    }

    async fn next_activity(
        &self,
        client: &Client,
        after: u64,
    ) -> Result<crate::ActivityPage, Failure> {
        let mut closing = client.closing.subscribe();
        self.activity
            .next(after, &mut closing)
            .await
            .ok_or_else(|| ServiceError::Closed("session").into())
    }
    pub(super) async fn sync_once(&self) -> Result<crate::ReconciliationReport, Failure> {
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
