//! SDK-owned partition metadata and external key storage.
use super::*;
use crate::enrollment::{PartitionRecord, partition_key_role};
use rocky_core::{PartitionTuple, TableClass};
use rocky_credentials::{Authority, PartitionGrant, PartitionKeyEnvelope};
impl Node {
    pub(super) fn partition_address(
        &self,
        request: &Value,
    ) -> Result<(ResourceScope, PartitionTuple), Failure> {
        let name = request["table"]
            .as_str()
            .ok_or("partition table must be a string")?;
        let logical = self.domain.logical();
        let table = logical
            .table(name)
            .filter(|table| table.class == TableClass::Mergeable && !table.partition_by.is_empty())
            .ok_or("table is not partitioned")?;
        let values: Vec<Cell> = serde_json::from_value(request["values"].clone())?;
        if values.len() != table.partition_by.len() {
            return Err("partition value count".into());
        }
        let tuple = PartitionTuple::new(
            table
                .partition_by
                .iter()
                .zip(values)
                .map(|(name, value)| {
                    let column = table.column(name).ok_or("unknown partition column")?;
                    Ok((
                        logical.field_id(table, column),
                        rocky_core::PartitionValue::new(
                            rocky_core::KeyColumn {
                                affinity: column.affinity,
                                collation: column.collation,
                            },
                            value.sql()?,
                        )?,
                    ))
                })
                .collect::<Result<Vec<_>, Failure>>()?,
        )?;
        Ok((
            tuple.scope(self.domain.scope, logical.table_id(table))?,
            tuple,
        ))
    }
    pub(super) async fn partition_call(
        &self,
        kind: &str,
        request: &Value,
    ) -> Result<Value, Failure> {
        if kind == "partition_role" {
            self.replica
                .set_role(serde_json::from_value(request["role"].clone())?)
                .await?;
            self.forget_readers().await;
            return Ok(json!({}));
        }
        if matches!(kind, "partition_grant" | "partition_delegate") {
            let device: crate::DeviceKeys = serde_json::from_value(request["device"].clone())?;
            let grants: Vec<PartitionGrant> =
                serde_json::from_value(request["partitions"].clone())?;
            let settings = self.settings.lock().await;
            let terms = (
                request["delegations"].as_u64().unwrap_or(1).try_into()?,
                None,
            );
            let validity = (now()?, settings.ttl, 300);
            let chain = if kind == "partition_delegate" {
                settings.credentials.delegable().delegate_partitions(
                    &PrivateKey::from_bytes(&self.vault.get("user")?),
                    device.user,
                    rocky_core::PrincipalKind::User,
                    vec![],
                    grants,
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
                        grants,
                        terms,
                        settings.policy.statement.epoch,
                        validity,
                    )?,
                ])
            };
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
            return Ok(json!({"supplied":supplied}));
        }
        let (prepared, kind, request) = if kind == "partition_next" {
            let table = request["table"]
                .as_str()
                .ok_or("partition table must be a string")?;
            let identity = self.replica.next_partition_identity(table).await?;
            let mut prepared = request.clone();
            prepared["values"] = json!([{"type":"integer","value":identity.to_string()}]);
            (Some((identity, prepared)), "partition_create", request)
        } else {
            (None, kind, request)
        };
        let request = prepared.as_ref().map_or(request, |(_, request)| request);
        let (scope, tuple) = self.partition_address(request)?;
        if kind == "partition_address" {
            return Ok(json!({"scope":scope,"tuple":tuple}));
        }
        let epoch = request["epoch"].as_u64().unwrap_or(1);
        if epoch == 0 || epoch > i64::MAX as u64 {
            return Err("invalid partition epoch".into());
        }
        match kind {
            "partition_register" | "partition_create" | "partition_install" => {
                self.replica
                    .register_partition(scope, tuple.clone())
                    .await?;
                let key = match kind {
                    "partition_create" => {
                        let settings = self.settings.lock().await;
                        if !settings.issuer() {
                            return Err("partition creation requires an issuer; install an envelope on a reader".into());
                        }
                        let role = partition_key_role(scope, epoch);
                        Some(match self.vault.find(&role)? {
                            Some(key) => key,
                            None => self.vault.create(&role)?,
                        })
                    }
                    "partition_install" => {
                        let bytes: Vec<u8> = serde_json::from_value(request["envelope"].clone())?;
                        let envelope = PartitionKeyEnvelope::from_bytes(&bytes)?;
                        let settings = self.settings.lock().await;
                        Some(envelope.open(
                            &PrivateKey::from_bytes(&self.vault.get("node")?),
                            settings.publisher,
                            self.domain.manifest().hash(),
                            scope,
                            epoch,
                        )?)
                    }
                    _ => None,
                };
                if let Some(key) = key {
                    let settings = self.settings.lock().await;
                    let grants = settings.credentials.verify_partition(
                        settings.publisher,
                        self.domain.manifest().hash(),
                        PrivateKey::from_bytes(&self.vault.get("node")?).public_key(),
                        scope,
                        &tuple,
                        &settings.policy.statement,
                        now()?,
                    )?;
                    if !grants.iter().any(|grant| {
                        grant.rows == rocky_core::RowSelection::All
                            && grant.rights.contains(&rocky_core::Right::Read)
                    }) {
                        return Err("partition key needs whole-partition read authority".into());
                    }
                    let role = partition_key_role(scope, epoch);
                    if self.vault.find(&role)?.is_some_and(|held| held != key) {
                        return Err("partition key epoch already holds different material".into());
                    }
                    self.vault.put(&role, &key)?;
                }
                self.configure(|settings| {
                    let record = settings
                        .partitions
                        .entry(HEXLOWER.encode(&scope.encryption))
                        .or_insert(PartitionRecord {
                            scope,
                            tuple,
                            epochs: vec![],
                        });
                    if key.is_some() && !record.epochs.contains(&epoch) {
                        if record.epochs.len() >= 64 {
                            return Err("partition key epoch capacity reached".into());
                        }
                        record.epochs.push(epoch);
                        record.epochs.sort_unstable();
                    }
                    Ok(())
                })
                .await?;
                if let Some(key) = key {
                    self.replica
                        .install_key_epoch(KeyMaterial {
                            scope,
                            generation: epoch,
                            key,
                        })
                        .await?;
                }
                self.forget_readers().await;
                self.wake_sync();
                Ok(
                    json!({"scope":scope,"epoch":epoch,"id":prepared.as_ref().map(|(identity,_)| identity.to_string())}),
                )
            }
            "partition_envelopes" => {
                let settings = self.settings.lock().await;
                if !settings.issuer() {
                    return Err("partition envelopes require an issuer".into());
                }
                let recipients: std::collections::BTreeSet<PublicKey> =
                    serde_json::from_value(request["recipients"].clone())?;
                let key = self.vault.get(&partition_key_role(scope, epoch))?;
                let envelopes = PartitionKeyEnvelope::seal_many(
                    &PrivateKey::from_bytes(&self.vault.get("publisher")?),
                    self.domain.manifest().hash(),
                    scope,
                    tuple,
                    epoch,
                    key,
                    &recipients,
                )?
                .iter()
                .map(PartitionKeyEnvelope::to_bytes)
                .collect::<Result<Vec<_>, _>>()?;
                Ok(json!({"envelopes":envelopes}))
            }
            "partition_subscribe" => {
                self.replica.subscribe(scope).await?;
                self.wake_sync();
                Ok(json!({}))
            }
            "partition_unsubscribe" => {
                self.replica.unsubscribe(scope).await?;
                self.forget_readers().await;
                Ok(json!({}))
            }
            "partition_evict" => {
                let report = self.replica.evict(scope).await?;
                self.forget_readers().await;
                Ok(json!({"rows":report.rows,"operations":report.operations}))
            }
            "partition_complete" => {
                Ok(json!({"complete":self.replica.table_complete(scope.table).await?}))
            }
            _ => Err(refused("unsupported partition request")),
        }
    }
}
