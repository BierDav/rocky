//! SDK-owned group partitions: addresses, the group secret in the external
//! vault, derived table keys, sharing and the holding rule.
use super::*;
use crate::groups::{GroupPartition, record_key, secret_role};
use rocky_core::{PartitionTuple, TableClass};
use rocky_credentials::{Authority, GroupSecretEnvelope, PartitionGrant};
use std::collections::BTreeSet;
impl Node {
    fn group_partition(&self, request: &Value) -> Result<GroupPartition, Failure> {
        let values: Vec<Cell> = serde_json::from_value(request["values"].clone())?;
        GroupPartition::resolve(
            &self.domain,
            request["group"].as_str(),
            request["table"].as_str(),
            values
                .into_iter()
                .map(Cell::sql)
                .collect::<Result<_, _>>()?,
        )
    }
    pub(super) fn partition_address(
        &self,
        request: &Value,
    ) -> Result<(ResourceScope, PartitionTuple), Failure> {
        let name = request["table"]
            .as_str()
            .ok_or("partition table must be a string")?;
        self.domain
            .logical()
            .table(name)
            .filter(|table| table.class == TableClass::Mergeable && !table.partition_by.is_empty())
            .ok_or("table is not partitioned")?;
        let partition =
            self.group_partition(&json!({"table": name, "values": request["values"]}))?;
        let member = partition.member(name).ok_or("table is not partitioned")?;
        Ok((member.scope, member.tuple.clone()))
    }
    /// Verifies whole-partition read on every member, as a group secret
    /// reveals every member's key.
    fn reads_group(&self, settings: &Settings, partition: &GroupPartition) -> Result<(), Failure> {
        let node = PrivateKey::from_bytes(&self.vault.get("node")?).public_key();
        let time = now()?;
        for member in &partition.members {
            let grants = settings
                .credentials
                .verify_partition(
                    settings.publisher,
                    self.domain.manifest().hash(),
                    node,
                    member.scope,
                    &member.tuple,
                    &settings.policy.statement,
                    time,
                )
                .unwrap_or_default();
            if !grants.iter().any(|grant| {
                grant.rows == rocky_core::RowSelection::All
                    && grant.rights.contains(&rocky_core::Right::Read)
            }) {
                return Err(refused(format!(
                    "a group secret needs whole-partition read on every table of group {}; {} lacks it",
                    partition.record.group, member.table
                )));
            }
        }
        Ok(())
    }
    /// Registers the readable members and installs every held epoch's
    /// derived key; installing an installed key again does nothing.
    pub(super) async fn hold_keys(
        &self,
        settings: &Settings,
        partition: &GroupPartition,
    ) -> Result<(), Failure> {
        for member in &partition.members {
            if !settings.reads_partition(member) {
                continue;
            }
            self.replica
                .register_partition(member.scope, member.tuple.clone())
                .await?;
            for epoch in &partition.record.epochs {
                self.replica
                    .install_key_epoch(partition.key(&self.vault, member, *epoch)?)
                    .await?;
            }
        }
        Ok(())
    }
    /// Records the group partition, without a key unless it holds one.
    async fn know(&self, partition: &GroupPartition) -> Result<(), Failure> {
        let key = record_key(&partition.record.address);
        if self.settings.lock().await.groups.contains_key(&key) {
            return Ok(());
        }
        let record = partition.record.clone();
        self.configure(|settings| {
            settings.groups.entry(key).or_insert(record);
            Ok(())
        })
        .await
    }
    async fn issue_partitions(&self, kind: &str, request: &Value) -> Result<Value, Failure> {
        let device: crate::DeviceKeys = serde_json::from_value(request["device"].clone())?;
        let grants: Vec<PartitionGrant> = serde_json::from_value(request["partitions"].clone())?;
        let mut settings = self.settings.lock().await;
        let terms = (
            request["delegations"].as_u64().unwrap_or(1).try_into()?,
            None,
        );
        let validity = (now()?, settings.ttl, 300);
        if settings.revoked_device(&device.node) {
            return Err(refused("the device was revoked"));
        }
        let chain = if kind == "partition_delegate" {
            settings.credentials.delegable().delegate_partitions(
                &PrivateKey::from_bytes(&self.vault.get("user")?),
                device.user,
                rocky_core::PrincipalKind::User,
                vec![],
                grants.clone(),
                terms,
                validity,
            )?
        } else {
            if !settings.issuer() {
                return Err("partition grant requires an issuer".into());
            }
            let issuer = PrivateKey::from_bytes(&self.vault.get("publisher")?);
            CredentialChain(vec![
                Authority {
                    publisher: &issuer,
                    manifest: self.domain.manifest().hash(),
                }
                .issue_partition_terms(
                    device.user,
                    vec![],
                    grants.clone(),
                    terms,
                    settings.policy.statement.epoch,
                    validity,
                )?,
            ])
        };
        // An issuer records the holder durably before the chain leaves it.
        if settings.authority.is_some() {
            let mut draft = settings.clone();
            draft.record_holder(&device, &grants).map_err(refused)?;
            draft.save(&self.path)?;
            *settings = draft;
        }
        let supplied = crate::Supplied {
            mesh: settings.mesh,
            domain: settings.domain,
            credentials: rocky_credentials::Credentials {
                publisher: settings.publisher,
                manifest: settings.manifest()?,
                policy: settings.policy.clone(),
                chain,
            },
            keys: vec![],
            issuers: vec![],
        };
        Ok(json!({"supplied":supplied}))
    }
    /// Prepares the group secret on an issuer: the current epoch of a known
    /// partition, otherwise the current policy epoch.
    async fn create_group(&self, partition: GroupPartition) -> Result<Value, Failure> {
        let mut settings = self.settings.lock().await;
        if !settings.issuer() {
            return Err(refused(
                "partition creation requires an issuer; install an envelope on a reader",
            ));
        }
        self.reads_group(&settings, &partition)?;
        let key = record_key(&partition.record.address);
        let mut record = settings
            .groups
            .get(&key)
            .cloned()
            .unwrap_or_else(|| partition.record.clone());
        let epoch = record.current().unwrap_or(settings.policy.statement.epoch);
        record.admit(epoch)?;
        let role = secret_role(&record.address, epoch);
        if self.vault.find(&role)?.is_none() {
            self.vault.create(&role)?;
        }
        record.hold(epoch)?;
        let mut draft = settings.clone();
        draft.groups.insert(key, record.clone());
        draft.save(&self.path)?;
        *self.granted.lock().map_err(|_| "scope catalog poisoned")? = draft.granted(&self.domain);
        *settings = draft;
        let partition = GroupPartition::of(&self.domain, &record)?;
        self.hold_keys(&settings, &partition).await?;
        Ok(json!({"address": record_key(&record.address), "epoch": epoch}))
    }
    /// Accepts one group envelope after it opens and the device reads every
    /// member in whole; only then is anything registered or stored.
    async fn install_group(
        &self,
        partition: GroupPartition,
        request: &Value,
    ) -> Result<Value, Failure> {
        let bytes: Vec<u8> = serde_json::from_value(request["envelope"].clone())?;
        let envelope = GroupSecretEnvelope::from_bytes(&bytes)?;
        let epoch = request["epoch"].as_u64().unwrap_or(envelope.epoch());
        let mut settings = self.settings.lock().await;
        let secret = envelope.open(
            &PrivateKey::from_bytes(&self.vault.get("node")?),
            settings.publisher,
            self.domain.manifest().hash(),
            partition.record.address,
            epoch,
        )?;
        self.reads_group(&settings, &partition)?;
        let key = record_key(&partition.record.address);
        let mut record = settings
            .groups
            .get(&key)
            .cloned()
            .unwrap_or_else(|| partition.record.clone());
        record.admit(epoch)?;
        let role = secret_role(&record.address, epoch);
        match self.vault.find(&role)? {
            Some(held) if held != secret => {
                return Err(refused(
                    "group secret epoch already holds different material",
                ));
            }
            Some(_) => {}
            None => self.vault.put(&role, &secret)?,
        }
        record.hold(epoch)?;
        let mut draft = settings.clone();
        draft.groups.insert(key, record.clone());
        draft.save(&self.path)?;
        *self.granted.lock().map_err(|_| "scope catalog poisoned")? = draft.granted(&self.domain);
        *settings = draft;
        let partition = GroupPartition::of(&self.domain, &record)?;
        self.hold_keys(&settings, &partition).await?;
        Ok(json!({"address": record_key(&record.address), "epoch": epoch}))
    }
    /// One envelope per recipient of the current epoch's group secret.
    async fn share(&self, partition: GroupPartition, request: &Value) -> Result<Value, Failure> {
        let settings = self.settings.lock().await;
        if !settings.issuer() {
            return Err(refused("sharing a group partition requires an issuer"));
        }
        if settings
            .authority
            .as_ref()
            .is_some_and(|authority| authority.rotation.is_some())
        {
            return Err(refused("a revocation is unfinished; reopen to complete it"));
        }
        let record = settings
            .groups
            .get(&record_key(&partition.record.address))
            .filter(|record| record.current().is_some())
            .ok_or_else(|| refused("no group secret is held here; create or install it first"))?;
        let current = record.current().ok_or("no group secret epoch")?;
        let epoch = request["epoch"].as_u64().unwrap_or(current);
        if epoch != current {
            return Err(refused(format!(
                "only the current epoch {current} of a group secret is shared"
            )));
        }
        let recipients: BTreeSet<PublicKey> =
            serde_json::from_value(request["recipients"].clone())?;
        if let Some(revoked) = recipients
            .iter()
            .find(|recipient| settings.revoked_device(recipient))
        {
            return Err(refused(format!(
                "recipient {} was revoked",
                revoked.to_hex()
            )));
        }
        let envelopes = GroupSecretEnvelope::seal_many(
            &PrivateKey::from_bytes(&self.vault.get("publisher")?),
            self.domain.manifest().hash(),
            record.address,
            epoch,
            self.vault.get(&secret_role(&record.address, epoch))?,
            &recipients,
        )?
        .iter()
        .map(|envelope| {
            Ok(json!({"recipient": envelope.recipient().to_hex(), "envelope": envelope.to_bytes()?}))
        })
        .collect::<Result<Vec<_>, Failure>>()?;
        Ok(json!({"epoch": epoch, "envelopes": envelopes}))
    }
    async fn group_status(&self, partition: GroupPartition) -> Result<Value, Failure> {
        let epochs = self
            .settings
            .lock()
            .await
            .groups
            .get(&record_key(&partition.record.address))
            .map(|record| record.epochs.clone())
            .unwrap_or_default();
        let holding: BTreeMap<ResourceScope, rocky_secure::Holding> =
            self.replica.subscriptions().await?.into_iter().collect();
        let mut tables = Vec::new();
        for member in &partition.members {
            tables.push(json!({
                "table": member.table,
                "scope": member.scope,
                // As activity events name the partition.
                "partition": data_encoding::HEXLOWER.encode(&member.scope.encryption),
                "epochs": self.replica.key_epochs(member.scope).await,
                "holding": holding
                    .get(&member.scope)
                    .map(|state| format!("{state:?}").to_lowercase()),
            }));
        }
        Ok(json!({
            "group": partition.record.group,
            "address": record_key(&partition.record.address),
            "epochs": epochs,
            "tables": tables,
        }))
    }
    pub(super) async fn partition_call(
        &self,
        kind: &str,
        request: &Value,
    ) -> Result<Value, Failure> {
        match kind {
            "partition_role" => {
                let role: rocky_secure::RoleConfig =
                    serde_json::from_value(request["role"].clone())?;
                let partial = role.role == rocky_secure::ReplicaRole::Partial;
                self.replica.set_role(role).await?;
                // Partial concerns partitions: every whole unpartitioned table read here stays held.
                if partial {
                    let settings = self.settings.lock().await.clone();
                    for (_, scope) in self.domain.scopes() {
                        if !self.domain.partitioned_table(&scope)
                            && settings.reads_whole(&self.domain, &scope)
                        {
                            self.replica.subscribe(scope).await?;
                        }
                    }
                }
                self.activity
                    .record("role_changed", json!({"partial": partial}));
                self.forget_readers().await;
                self.wake_sync();
                return Ok(json!({}));
            }
            "partition_grant" | "partition_delegate" => {
                return self.issue_partitions(kind, request).await;
            }
            "partition_address" => {
                let (scope, tuple) = self.partition_address(request)?;
                let partition = self.group_partition(request)?;
                return Ok(json!({
                    "scope": scope,
                    "tuple": tuple,
                    "group": partition.record.group,
                    "address": record_key(&partition.record.address),
                }));
            }
            "partition_complete" => {
                let (scope, _) = self.partition_address(request)?;
                return Ok(json!({"complete": self.replica.table_complete(scope.table).await?}));
            }
            "partition_next" => {
                let table = request["table"]
                    .as_str()
                    .ok_or("partition table must be a string")?;
                let identity = self.replica.next_partition_identity(table).await?;
                let partition = self.group_partition(&json!({
                    "table": table,
                    "values": [{"type": "integer", "value": identity.to_string()}],
                }))?;
                let (scope, _) = partition
                    .member(table)
                    .map(|member| (member.scope, ()))
                    .ok_or("table is not partitioned")?;
                let mut created = self.create_group(partition).await?;
                created["id"] = identity.to_string().into();
                created["scope"] = json!(scope);
                self.activity.record(
                    "partition_created",
                    json!({"table": table, "id": identity.to_string()}),
                );
                self.forget_readers().await;
                self.wake_sync();
                return Ok(created);
            }
            _ => {}
        }
        let partition = self.group_partition(request)?;
        let named = json!({"group": request["group"], "values": request["values"]});
        let omit: BTreeSet<String> = match &request["omit"] {
            Value::Null => BTreeSet::new(),
            omit => serde_json::from_value(omit.clone())?,
        };
        let result = match kind {
            "partition_create" => self.create_group(partition).await?,
            "partition_install" => self.install_group(partition, request).await?,
            "partition_share" => {
                let shared = self.share(partition, request).await?;
                let mut event = named.clone();
                event["recipients"] = json!(shared["envelopes"].as_array().map_or(0, Vec::len));
                self.activity.record("partition_shared", event);
                return Ok(shared);
            }
            "partition_status" => return self.group_status(partition).await,
            "partition_register" => {
                for member in partition.covered(&omit).map_err(refused)? {
                    self.replica
                        .register_partition(member.scope, member.tuple.clone())
                        .await?;
                }
                self.know(&partition).await?;
                json!({})
            }
            "partition_subscribe" | "partition_bootstrap" => {
                let covered = partition.covered(&omit).map_err(refused)?;
                for member in &covered {
                    self.replica
                        .register_partition(member.scope, member.tuple.clone())
                        .await?;
                }
                self.know(&partition).await?;
                for member in &covered {
                    self.replica.subscribe(member.scope).await?;
                }
                for member in partition
                    .members
                    .iter()
                    .filter(|member| omit.contains(&member.table))
                {
                    self.replica.unsubscribe(member.scope).await?;
                }
                if kind == "partition_bootstrap" {
                    self.bootstrap_partition(&covered).await?
                } else {
                    json!({})
                }
            }
            "partition_unsubscribe" => {
                for member in partition.released(&omit).map_err(refused)? {
                    self.replica.unsubscribe(member.scope).await?;
                }
                json!({})
            }
            "partition_evict" => {
                let (mut rows, mut operations) = (0u64, 0u64);
                for member in partition.released(&omit).map_err(refused)? {
                    let report = self.replica.evict(member.scope).await?;
                    rows += report.rows as u64;
                    operations += report.operations as u64;
                }
                json!({"rows": rows, "operations": operations})
            }
            _ => return Err(refused("unsupported partition request")),
        };
        let event = match kind {
            "partition_create" => Some("partition_created"),
            "partition_install" => Some("partition_key_installed"),
            "partition_bootstrap" => Some("partition_bootstrap"),
            "partition_subscribe" => Some("partition_subscribed"),
            "partition_unsubscribe" => Some("partition_unsubscribed"),
            "partition_evict" => Some("partition_evicted"),
            _ => None,
        };
        if let Some(event) = event {
            self.activity.record(event, named);
        }
        self.forget_readers().await;
        self.wake_sync();
        Ok(result)
    }
    /// Saves a checkpoint-first intent for the covered members this device
    /// reads; the next sync imports them before catching up.
    async fn bootstrap_partition(
        &self,
        covered: &[&crate::groups::Member],
    ) -> Result<Value, Failure> {
        let marker = self.path.with_file_name("bootstrap.json");
        let added: Vec<ResourceScope> = {
            let settings = self.settings.lock().await;
            covered
                .iter()
                .filter(|member| settings.reads_partition(member))
                .map(|member| member.scope)
                .collect()
        };
        // A pending intent waits for a peer that offers checkpoints, which a
        // keyless custody peer never does: later partitions join it.
        let mut intent = if marker.exists() {
            serde_json::from_slice::<Value>(&std::fs::read(&marker)?)?
        } else {
            json!({"intent": true, "pending": []})
        };
        let Some(pending) = intent.get_mut("pending").and_then(Value::as_array_mut) else {
            // An intent without a scope list covers every granted scope.
            return Ok(json!({"pending": added.len()}));
        };
        for scope in &added {
            let scope = serde_json::to_value(scope)?;
            if !pending.contains(&scope) {
                pending.push(scope);
            }
        }
        let count = pending.len();
        rocky_credentials::write_private(&marker, &serde_json::to_vec(&intent)?)?;
        Ok(json!({"pending": count}))
    }
}
