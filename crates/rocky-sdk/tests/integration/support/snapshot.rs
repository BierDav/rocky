#![expect(
    dead_code,
    reason = "the SDK's unit and integration tests each use a subset"
)]
use rocky_sdk as sdk;
use serde_json::{Value, json};
use std::future::Future;
type Failure = Box<dyn std::error::Error + Send + Sync>;
pub trait Snapshot {
    fn snapshot(self) -> Value;
}
macro_rules! serialized {
    ($($ty:ty),* $(,)?) => { $(impl Snapshot for $ty { fn snapshot(self) -> Value { serde_json::to_value(self).expect("snapshot") } })* };
}
serialized!(
    Value,
    (),
    bool,
    String,
    sdk::PublicKey,
    sdk::TipStatus,
    sdk::TipInspection,
    sdk::TipCompaction,
    sdk::status::DatabaseStatus,
    sdk::status::SyncReport,
    sdk::status::NetworkStatus,
    sdk::inspection::RetentionLimits
);
pub async fn device(database: &sdk::Database) -> Result<Value, sdk::Error> {
    let authority = database.status().await?.authority;
    Ok(
        json!({"principal":authority.principal,"node":authority.node,"revocations":authority.revocations,"expires":authority.expires_at_epoch_seconds.to_string(),"issuer":authority.is_issuer,"access":authority.access}),
    )
}
pub async fn observe<T: Snapshot, E: Into<Failure>>(
    future: impl Future<Output = Result<T, E>>,
) -> Result<Value, Value> {
    future.await.map(Snapshot::snapshot).map_err(|error| {
        let error: Failure = error.into();
        let Some(failure) = error.downcast_ref::<sdk::Error>() else {
            let code = if error.is::<FixtureRejection>() { "rejected" } else { "internal" };
            return json!({"code": code, "retryable": false, "message": error.to_string(), "blockers": []});
        };
        if let sdk::ErrorKind::Sql { code, message, .. } = failure.kind() {
            let retryable = matches!(code, 5 | 6);
            return json!({"code": if retryable { "busy" } else { "rejected" }, "retryable": retryable, "message": message, "blockers": []});
        }
        json!({"code":failure.code(),"retryable":failure.retryable(),"message":failure.to_string(),"blockers":failure.blockers()})
    })
}
pub async fn ok<T: Snapshot, E: Into<Failure>>(
    future: impl Future<Output = Result<T, E>>,
) -> Value {
    observe(future)
        .await
        .unwrap_or_else(|error| panic!("operation failed: {error}"))
}
pub async fn value<T: Snapshot, E: Into<Failure>>(
    future: impl Future<Output = Result<T, E>>,
) -> Result<Value, Failure> {
    future.await.map(Snapshot::snapshot).map_err(Into::into)
}
pub fn cell(value: sdk::SqlValue) -> Value {
    match value {
        sdk::SqlValue::Null => json!({"type":"null"}),
        sdk::SqlValue::Integer(value) => json!({"type":"integer","value":value.to_string()}),
        sdk::SqlValue::Real(value) => {
            json!({"type":"real","value":format!("{:016x}",value.bits())})
        }
        sdk::SqlValue::Text(value) => json!({"type":"text","value":value}),
        sdk::SqlValue::Blob(value) => json!({"type":"blob","value":value}),
    }
}
pub async fn read(
    database: &sdk::Database,
    sql: &str,
    values: Vec<sdk::SqlValue>,
) -> Result<Value, Failure> {
    let mut rows = database.query(sql, values).await?;
    let columns = rows.columns().to_vec();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let mut cells = Vec::with_capacity(columns.len());
        for index in 0..columns.len() {
            cells.push(cell(row.get_at::<sdk::SqlValue>(index)?));
        }
        out.push(cells);
    }
    Ok(json!({"columns":columns,"rows":out}))
}
impl Snapshot for rocky_core::RowIdentity {
    fn snapshot(self) -> Value {
        json!({"id":self.to_string()})
    }
}
impl Snapshot for sdk::Invitation {
    fn snapshot(self) -> Value {
        json!({"id":self.id,"code":self.code.to_string(),"expires":self.expires.unix_micros().to_string()})
    }
}
impl Snapshot for sdk::ContactInfo {
    fn snapshot(self) -> Value {
        json!({"uri":self.uri,"node":self.node,"routes":self.routes})
    }
}
impl Snapshot for sdk::status::GenerationStatus {
    fn snapshot(self) -> Value {
        let reason = self.follow_required.then_some("follow_required");
        let mut view = serde_json::to_value(self).expect("snapshot");
        view["reason"] = json!(reason);
        view
    }
}
pub fn partition(partition: sdk::Partition) -> Value {
    json!({"group":partition.group,"values":partition.values.into_iter().map(cell).collect::<Vec<_>>()})
}
fn micros(at: sdk::Timestamp) -> String {
    at.unix_micros().to_string()
}
impl Snapshot for sdk::Partition {
    fn snapshot(self) -> Value {
        partition(self)
    }
}
impl Snapshot for Vec<sdk::Person> {
    fn snapshot(self) -> Value {
        json!({"identities":self.into_iter().map(|person|json!({"identity":person.id.to_string(),"access":person.access,"admin":person.admin,"owner":person.owner,"label":person.label,"joined":micros(person.joined),"invited_by":person.invited_by.map(|by|by.to_string()),"invitation":person.invitation,"partitions":person.partitions.into_iter().map(partition).collect::<Vec<_>>(),"nodes":person.devices.into_iter().map(|device|json!({"node":device.id.to_string(),"linked":micros(device.linked),"last_seen":device.last_seen.map(micros)})).collect::<Vec<_>>()})).collect::<Vec<_>>()})
    }
}
impl Snapshot for sdk::Me {
    fn snapshot(self) -> Value {
        json!({"person":self.person.to_string(),"device":self.device.to_string(),"access":self.access,"admin":self.admin,"owner":self.owner,"tables":self.tables.into_iter().map(|(table,rights)|(table,json!({"read":rights.read,"insert":rights.insert,"update":rights.update,"delete":rights.delete}))).collect::<serde_json::Map<_,_>>(),"expires":micros(self.expires)})
    }
}
impl Snapshot for sdk::inspection::RetentionStatus {
    fn snapshot(self) -> Value {
        json!({"policy":self.policy,"covered":self.checkpoint.covered,"uncovered":self.checkpoint.uncovered,"pruned_prefix":self.checkpoint.pruned_prefix,"maintenance_error":self.checkpoint.maintenance_error})
    }
}
impl Snapshot for sdk::CreatedPartition {
    fn snapshot(self) -> Value {
        json!({"id":self.id,"partition":partition(self.partition)})
    }
}
impl Snapshot for sdk::partitions::PartitionStatus {
    fn snapshot(self) -> Value {
        json!({"group":self.group,"address":self.address,"current":self.current,"epochs":self.epochs,"tables":self.tables.into_iter().map(|table|json!({"table":table.table,"scope":table.scope,"partition":table.scope.map(|scope|data_encoding::HEXLOWER.encode(&scope.encryption)),"epochs":table.epochs,"current":table.current,"subscribed":table.subscribed})).collect::<Vec<_>>(),"recipients":self.recipients.into_iter().map(|recipient|json!({"node":recipient.node,"reference":recipient.reference,"epoch":recipient.epoch,"state":match recipient.state { sdk::DeliveryState::Delivered=>"delivered",sdk::DeliveryState::Deposited=>"deposited",sdk::DeliveryState::Pending=>"pending" }})).collect::<Vec<_>>()})
    }
}
impl Snapshot for Vec<sdk::inspection::HeldChange> {
    fn snapshot(self) -> Value {
        fn decoded(bytes: &[u8]) -> Value {
            let mut slice = bytes;
            match sdk::SqlValue::decode(&mut slice) {
                Ok(value) if slice.is_empty() => cell(value),
                _ => cell(sdk::SqlValue::Blob(bytes.to_vec())),
            }
        }
        json!({"count":self.len(),"changes":self.into_iter().map(|change|json!({"id":data_encoding::HEXLOWER.encode(&change.id),"time":change.time,"rows":change.rows.into_iter().map(|row|json!({"table":row.table,"removed":row.removed,"row":data_encoding::HEXLOWER.encode(&row.row),"kind":match row.kind { sdk::inspection::HeldKind::Insert=>"insert",sdk::inspection::HeldKind::Update=>"update",sdk::inspection::HeldKind::Delete=>"delete" },"fields":row.fields.into_iter().map(|field|json!({"name":field.name,"value":decoded(&field.value),"written":field.written,"removed":field.removed,"current":field.current.map(|current|json!({"value":current.value.as_deref().map(decoded),"time":current.time,"author":current.author}))})).collect::<Vec<_>>()})).collect::<Vec<_>>(),"intents":change.intents.into_iter().map(|intent|json!({"table":intent.table,"statements":intent.statements.into_iter().map(|statement|json!({"sql":statement.sql,"params":statement.params.into_iter().map(cell).collect::<Vec<_>>()})).collect::<Vec<_>>()})).collect::<Vec<_>>()})).collect::<Vec<_>>()})
    }
}
pub trait Scalar {
    fn text(&self) -> String;
}
impl Scalar for Value {
    fn text(&self) -> String {
        self.as_str()
            .map_or_else(|| self.to_string(), str::to_owned)
    }
}
impl Scalar for str {
    fn text(&self) -> String {
        self.into()
    }
}
impl Scalar for String {
    fn text(&self) -> String {
        self.clone()
    }
}
impl<T: Scalar + ?Sized> Scalar for &T {
    fn text(&self) -> String {
        (*self).text()
    }
}
macro_rules! scalar { ($($ty:ty),*) => { $(impl Scalar for $ty { fn text(&self) -> String { self.to_string() } })* }; }
scalar!(
    u8,
    u16,
    u32,
    u64,
    usize,
    i32,
    i64,
    rocky_core::RowIdentity,
    rocky_credentials::PublicKey
);
pub fn text(value: &impl Scalar) -> String {
    value.text()
}
pub fn public_key(value: &impl Scalar) -> sdk::PublicKey {
    text(value).parse().expect("public key")
}
pub fn device_id(value: &impl Scalar) -> sdk::DeviceId {
    sdk::DeviceId::new(public_key(value))
}
pub fn schema(json: &str) -> &'static sdk::Schema {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeMap<String, &'static sdk::Schema>>,
    > = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let cached = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(json)
        .copied();
    if let Some(parsed) = cached {
        return parsed;
    }
    let parsed: &'static sdk::Schema =
        Box::leak(Box::new(serde_json::from_str(json).expect("test schema")));
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(json.to_owned(), parsed);
    parsed
}
pub fn integer(value: &impl Scalar) -> i64 {
    value.text().parse().expect("integer fixture")
}
pub fn cell_value(cell: &Value) -> Value {
    let bytes = (cell["type"] == "blob")
        .then(|| serde_json::from_value::<Vec<u8>>(cell["value"].clone()).ok())
        .flatten();
    match bytes.and_then(|bytes| sdk::RowIdentity::from_bytes(&bytes).ok()) {
        Some(id) => Value::String(id.to_string()),
        None => cell["value"].clone(),
    }
}
pub fn cell_id(cell: &Value) -> sdk::RowIdentity {
    let bytes: Vec<u8> = serde_json::from_value(cell["value"].clone()).expect("blob cell");
    sdk::RowIdentity::from_bytes(&bytes).expect("row identity cell")
}
pub fn row_id(value: &impl Scalar) -> sdk::SqlValue {
    value
        .text()
        .parse::<sdk::RowIdentity>()
        .expect("row identity fixture")
        .sql()
}
pub fn digest(value: &impl Scalar) -> [u8; 32] {
    data_encoding::HEXLOWER
        .decode(value.text().as_bytes())
        .expect("hex")
        .try_into()
        .expect("digest")
}
pub fn key(value: &impl Scalar) -> rocky_credentials::PublicKey {
    value.text().parse().expect("public key")
}
pub async fn reply<T: Snapshot, E: Into<Failure>>(
    future: impl Future<Output = Result<T, E>>,
) -> Value {
    match observe(future).await {
        Ok(value) => json!({"status":"ok","value":value}),
        Err(error) => json!({"status":"error","error":error}),
    }
}
#[derive(Clone, Debug)]
pub enum Action {
    Create {
        table: String,
        values: Vec<(String, sdk::SqlValue)>,
    },
    Set {
        table: String,
        field: String,
        value: sdk::SqlValue,
    },
    Delete {
        table: String,
    },
}
pub fn field(value: Value) -> sdk::SqlValue {
    match value {
        Value::Null => sdk::SqlValue::Null,
        Value::Bool(value) => sdk::SqlValue::Integer(i64::from(value)),
        Value::Number(value) => sdk::SqlValue::Integer(value.as_i64().expect("integer field")),
        Value::String(value) => match (value.parse::<i64>(), value.parse::<sdk::RowIdentity>()) {
            (Ok(number), _) if number.to_string() == value => sdk::SqlValue::Integer(number),
            (_, Ok(row_identity_binds_as_its_blob)) => row_identity_binds_as_its_blob.sql(),
            _ => sdk::SqlValue::Text(value),
        },
        other => sdk::SqlValue::Text(other.to_string()),
    }
}
pub fn fields(values: Value) -> Vec<(String, sdk::SqlValue)> {
    values
        .as_object()
        .expect("fixture fields")
        .iter()
        .map(|(name, value)| (name.clone(), field(value.clone())))
        .collect()
}
pub async fn mutate(
    database: &sdk::Database,
    bytes: Vec<u8>,
    action: Action,
    entity: Option<String>,
    author: Option<String>,
) -> Result<Value, Failure> {
    let handle = mutation_handle(database, &bytes, author.as_deref()).await?;
    let key = data_encoding::HEXLOWER.encode(&handle);
    let valid = |name: &str| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    };
    if let Action::Create { table, .. } = &action {
        if !valid(table) {
            return Err("invalid fixture identifier".into());
        }
        if let Ok(mut rows) = database
            .query(
                &format!("SELECT 1 FROM \"{table}\" WHERE key=?"),
                vec![sdk::SqlValue::Text(key.clone())],
            )
            .await
            && rows.next().await?.is_some()
        {
            return Ok(json!({"replayed":true,"handle":key}));
        }
    }
    let id = database
        .write(async |tx| {
            let id = match action {
                Action::Create { table, values } => {
                    if !valid(&table) || values.iter().any(|(name, _)| !valid(name)) {
                        return Err("invalid fixture identifier".into());
                    }
                    let mut columns = vec!["key".to_owned()];
                    let mut bound = vec![sdk::SqlValue::Text(key.clone())];
                    for (name, value) in values {
                        columns.push(format!("\"{name}\""));
                        bound.push(value);
                    }
                    let sql = format!(
                        "INSERT INTO \"{table}\"({}) VALUES({})",
                        columns.join(","),
                        vec!["?"; columns.len()].join(",")
                    );
                    tx.execute(&sql, bound).await?;
                    key.clone()
                }
                Action::Set {
                    table,
                    field,
                    value,
                } => {
                    if !valid(&table) || !valid(&field) {
                        return Err("invalid fixture identifier".into());
                    }
                    let id = entity.ok_or("missing fixture entity")?;
                    let exists = tx
                        .query(
                            &format!("SELECT 1 FROM \"{table}\" WHERE key=?"),
                            vec![sdk::SqlValue::Text(id.clone())],
                        )
                        .await?
                        .next()
                        .await?
                        .is_some();
                    if !exists {
                        return Err(fixture_rejection("fixture entity not found"));
                    }
                    tx.execute(
                        &format!("UPDATE \"{table}\" SET \"{field}\"=? WHERE key=?"),
                        vec![value, sdk::SqlValue::Text(id.clone())],
                    )
                    .await?;
                    id
                }
                Action::Delete { table } => {
                    if !valid(&table) {
                        return Err("invalid fixture identifier".into());
                    }
                    let id = entity.ok_or("missing fixture entity")?;
                    tx.execute(
                        &format!("DELETE FROM \"{table}\" WHERE key=?"),
                        vec![sdk::SqlValue::Text(id.clone())],
                    )
                    .await?;
                    id
                }
            };
            Ok::<_, Failure>(id)
        })
        .await?;
    let frames = Some(1usize);
    Ok(json!({"id":id,"handle":key,"frames":frames}))
}

impl Snapshot for sdk::ActivityPage {
    fn snapshot(self) -> Value {
        fn scope_view(scope: sdk::ActivityScope) -> Value {
            json!({"table":scope.table,"partition":scope.partition.map(|partition|data_encoding::HEXLOWER.encode(&partition))})
        }
        let events=self.events.into_iter().map(|event| {
            let mut detail=match event.detail {
                sdk::ActivityKind::LinkAdmissionFailed { error } => json!({"kind":"link_admission_failed","error":error}),
                sdk::ActivityKind::IdentityInvited { invitation, person, access, expires } => json!({"kind":"identity_invited","invitation":invitation,"person":person,"access":access,"expires":expires.to_string()}),
                sdk::ActivityKind::InvitationRedeemed { invitation, identity, node } => json!({"kind":"invitation_redeemed","invitation":invitation,"identity":identity,"node":node}),
                sdk::ActivityKind::PeerAdded { peer } => json!({"kind":"peer_added","peer":peer}),
                sdk::ActivityKind::PeerRemoved { peer } => json!({"kind":"peer_removed","peer":peer}),
                sdk::ActivityKind::IdentityRevoked { identity } => json!({"kind":"identity_revoked","identity":identity}),
                sdk::ActivityKind::NodeUnlinked { identity, node } => json!({"kind":"node_unlinked","identity":identity,"node":node}),
                sdk::ActivityKind::RevocationCutDroppedStages { identity, count } => json!({"kind":"revocation_cut_dropped_stages","identity":identity,"count":count}),
                sdk::ActivityKind::RightsChanged { person } => json!({"kind":"rights_changed","person":person.to_string()}),
                sdk::ActivityKind::RevocationSealFailed { error } => json!({"kind":"revocation_seal_failed","error":error}),
                sdk::ActivityKind::LineNodeFailed { error } => json!({"kind":"line_node_failed","error":error}),
                sdk::ActivityKind::RegistryFailed { error } => json!({"kind":"registry_failed","error":error}),
                sdk::ActivityKind::PeerAttemptFailed { peer, error } => json!({"kind":"peer_attempt_failed","peer":peer,"error":error}),
                sdk::ActivityKind::PeerReconciliationCompleted { peer } => json!({"kind":"peer_reconciliation_completed","peer":peer}),
                sdk::ActivityKind::ScopesRefused { peer, scopes } => json!({"kind":"scopes_refused","peer":peer,"scopes":scopes.into_iter().map(scope_view).collect::<Vec<_>>()}),
                sdk::ActivityKind::ScopesWaiting { peer, scopes } => json!({"kind":"scopes_waiting","peer":peer,"scopes":scopes.into_iter().map(scope_view).collect::<Vec<_>>()}),
                sdk::ActivityKind::Synced { peer, pulled, pushed, scopes } => json!({"kind":"synced","peer":peer,"pulled":pulled,"pushed":pushed,"scopes":scopes.into_iter().map(|moved| { let mut named=scope_view(moved.scope); named["pulled"]=json!(moved.pulled); named["pushed"]=json!(moved.pushed); named["bytes"]=json!(moved.bytes); named }).collect::<Vec<_>>()}),
                sdk::ActivityKind::CheckpointImported { scope, peer, checkpoint, superseded } => json!({"kind":"checkpoint_imported","scope":scope_view(scope),"peer":peer,"checkpoint":data_encoding::HEXLOWER.encode(&checkpoint),"superseded":superseded}),
                sdk::ActivityKind::RenewalFailed { error } => json!({"kind":"renewal_failed","error":error}),
                sdk::ActivityKind::EpochDeliveryFailed { peer, error } => json!({"kind":"epoch_delivery_failed","peer":peer,"error":error}),
                sdk::ActivityKind::BootstrapStarted { peer, scopes } => json!({"kind":"bootstrap_started","peer":peer,"scopes":scopes}),
                sdk::ActivityKind::BootstrapFinished { peer } => json!({"kind":"bootstrap_finished","peer":peer}),
                sdk::ActivityKind::MemberSealFailed { error } => json!({"kind":"member_seal_failed","error":error}),
                sdk::ActivityKind::ReplayedAwaitingOutcomes { transactions } => json!({"kind":"replayed_awaiting_outcomes","transactions":transactions}),
                sdk::ActivityKind::RoutineSealFailed { error } => json!({"kind":"routine_seal_failed","error":error}),
                sdk::ActivityKind::RightsChangeFailed { error } => json!({"kind":"rights_change_failed","error":error}),
                sdk::ActivityKind::LocalRecoveryFailed { error } => json!({"kind":"local_recovery_failed","error":error}),
                sdk::ActivityKind::RoleChanged { partial } => json!({"kind":"role_changed","partial":partial}),
                sdk::ActivityKind::PartitionCreated { partition: created, id } => json!({"kind":"partition_created","partition":partition(created),"id":id}),
                sdk::ActivityKind::PartitionShared { partition: shared, recipients } => json!({"kind":"partition_shared","partition":partition(shared),"recipients":recipients}),
                sdk::ActivityKind::PartitionShareFailed { partition: failed, error } => json!({"kind":"partition_share_failed","partition":failed.map(partition),"error":error}),
                sdk::ActivityKind::EnvelopesDelivered { peer, handed, deposited, refused, received, receipts } => json!({"kind":"envelopes_delivered","peer":peer,"handed":handed,"deposited":deposited,"refused":refused,"received":received,"receipts":receipts}),
                sdk::ActivityKind::EnvelopeRefused { peer, error } => json!({"kind":"envelope_refused","peer":peer,"error":error}),
                sdk::ActivityKind::PartitionKeyInstalled { partition: installed } => json!({"kind":"partition_key_installed","partition":partition(installed)}),
                sdk::ActivityKind::PartitionBootstrap { partition: held } => json!({"kind":"partition_bootstrap","partition":partition(held)}),
                sdk::ActivityKind::MembershipLinkRecorded { identity, node, sequence } => json!({"kind":"membership_link_recorded","identity":identity,"node":node,"sequence":sequence}),
                sdk::ActivityKind::NetworkingEnabled => json!({"kind":"networking_enabled"}),
                sdk::ActivityKind::NetworkingDisabled => json!({"kind":"networking_disabled"}),
                sdk::ActivityKind::LinkSignalFailed { peer, error } => json!({"kind":"link_signal_failed","peer":peer,"error":error}),
                sdk::ActivityKind::LinkAttemptFailed { peer, error } => json!({"kind":"link_attempt_failed","peer":peer,"error":error}),
                sdk::ActivityKind::LinkClosed { peer, reason, error } => json!({"kind":"link_closed","peer":peer,"reason":reason,"error":error}),
                sdk::ActivityKind::LinkOpened { peer, reason } => json!({"kind":"link_opened","peer":peer,"reason":reason}),
                sdk::ActivityKind::FinalizationFailed { error } => json!({"kind":"finalization_failed","error":error}),
                sdk::ActivityKind::TipSchemaUnavailable { manifest, peer, error } => json!({"kind":"tip_schema_unavailable","manifest":data_encoding::HEXLOWER.encode(&manifest),"peer":peer,"error":error}),
            };
            detail["sequence"]=json!(event.sequence.to_string());
            detail["at"]=json!(event.at_epoch_millis.to_string());
            detail
        }).collect::<Vec<_>>();
        json!({"events":events,"token":self.token.to_string()})
    }
}

pub async fn string<T: Snapshot, E: Into<Failure>>(
    future: impl Future<Output = Result<T, E>>,
) -> Result<Value, String> {
    observe(future).await.map_err(|error| error.to_string())
}

pub async fn mutation_handle(
    database: &sdk::Database,
    bytes: &[u8],
    author: Option<&str>,
) -> Result<rocky_core::Commitment, Failure> {
    let author = match author {
        Some(author) => author.to_owned(),
        None => database.me().await?.device.to_string(),
    };
    let mut identity = author.into_bytes();
    identity.extend(serde_json::to_vec(bytes)?);
    Ok(rocky_core::commitment("fixture.sql.handle", &identity))
}
#[derive(Debug)]
struct FixtureRejection(String);
impl std::fmt::Display for FixtureRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for FixtureRejection {}
fn fixture_rejection(detail: &str) -> Failure {
    Box::new(FixtureRejection(detail.into()))
}
