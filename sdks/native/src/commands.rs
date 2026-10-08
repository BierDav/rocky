//! Typed strong commands across the ABI: a handler the foreign application
//! implements and the coordinator it composes. Handler input and decisions
//! cross as the JSON of the call contract; see `docs/runtime/typed-commands.md`.
use crate::NativeError;
use rocky_coordinator::MemoryCoordinator;
use rocky_sdk::{CommandHandler, HandlerInput, HandlerOutput, NodeId, PrincipalId, Scope};
use std::{
    sync::{Arc, RwLock},
    time::Duration,
};

/// Decides commands in the application's language. Calls arrive one at a
/// time on a thread the runtime owns, each within the handler deadline. A
/// thrown exception or a late answer fails that call and nothing else; the
/// same ticket runs again.
#[uniffi::export(with_foreign)]
pub trait NativeCommandHandler: Send + Sync {
    /// `input` is a handler call and the result a decision, both as JSON.
    fn handle(&self, input: String, reads: Arc<NativeHandlerReads>) -> Result<String, NativeError>;
}
struct Foreign(Arc<dyn NativeCommandHandler>);
impl CommandHandler for Foreign {
    fn handle(&self, input: &HandlerInput) -> Result<HandlerOutput, String> {
        let reads = Arc::new(NativeHandlerReads {
            inner: RwLock::new(Some(input.reads.clone())),
        });
        let _lifetime = ReadLifetime(reads.clone());
        let decision = self
            .0
            .handle(rocky_sdk::handler_input_json(input), reads)
            .map_err(|error| error.to_string())?;
        rocky_sdk::handler_output_json(&decision)
    }
}

/// Immutable reads for this callback's confirmed predecessor. Retaining this
/// handle after the callback returns does not retain access to its rows.
#[derive(uniffi::Object)]
pub struct NativeHandlerReads {
    inner: RwLock<Option<rocky_sdk::HandlerReads>>,
}
struct ReadLifetime(Arc<NativeHandlerReads>);
impl Drop for ReadLifetime {
    fn drop(&mut self) {
        if let Ok(mut reads) = self.0.inner.write() {
            *reads = None;
        }
    }
}
impl NativeHandlerReads {
    fn read<T>(
        &self,
        f: impl FnOnce(&rocky_sdk::HandlerReads) -> Result<T, String>,
    ) -> Result<T, NativeError> {
        let reads = self
            .inner
            .read()
            .map_err(|_| invalid("handler reads unavailable"))?;
        f(reads
            .as_ref()
            .ok_or_else(|| invalid("handler reads expired"))?)
        .map_err(invalid)
    }
}
#[uniffi::export]
impl NativeHandlerReads {
    /// Key and returned row are tagged scalar JSON; an absent row is null.
    pub fn get(&self, table: String, key: String) -> Result<String, NativeError> {
        self.read(|reads| rocky_sdk::handler_get_json(reads, &table, &key))
    }
    /// Full keys bound an inclusive lower and exclusive upper range.
    pub fn scan(
        &self,
        table: String,
        lower: Option<String>,
        upper: Option<String>,
        limit: u32,
    ) -> Result<String, NativeError> {
        self.read(|reads| {
            rocky_sdk::handler_scan_json(
                reads,
                &table,
                lower.as_deref(),
                upper.as_deref(),
                limit as usize,
            )
        })
    }
}

/// The coordinator and, on a device that executes, the handler. Exactly
/// one of `coordinator`, in this process, and `service`, the trust bundle
/// of a coordination service reached over the database's iroh network.
#[derive(uniffi::Record)]
pub struct NativeCommands {
    pub coordinator: Option<Arc<DevelopmentCoordinator>>,
    pub service: Option<String>,
    pub handler: Option<Arc<dyn NativeCommandHandler>>,
    /// The handler deadline; 10 s when absent.
    pub deadline_millis: Option<u64>,
}
impl NativeCommands {
    pub(crate) fn compose(self) -> Result<rocky_sdk::Commands, NativeError> {
        let coordinator = match (self.coordinator, self.service) {
            (Some(coordinator), None) => {
                rocky_sdk::Coordination::Composed(coordinator.inner.clone())
            }
            #[cfg(feature = "iroh")]
            (None, Some(bundle)) => rocky_sdk::Coordination::Service(Box::new(
                serde_json::from_str(&bundle).map_err(invalid)?,
            )),
            #[cfg(not(feature = "iroh"))]
            (None, Some(_)) => {
                return Err(invalid("a coordination service needs the iroh network"));
            }
            _ => return Err(invalid("name one coordinator: development or service")),
        };
        Ok(rocky_sdk::Commands {
            coordinator,
            handler: self
                .handler
                .map(|handler| Arc::new(Foreign(handler)) as Arc<dyn CommandHandler>),
            deadline: self.deadline_millis.map(Duration::from_millis),
        })
    }
}

/// A device as its `command_identity` call reports it.
#[derive(serde::Deserialize)]
struct Identity {
    scope: Scope,
    node: String,
    principal: String,
    schema_generation: u32,
    handler_generation: u32,
}
fn invalid(detail: impl std::fmt::Display) -> NativeError {
    NativeError::Failure {
        detail: detail.to_string(),
    }
}
fn id(hex: &str) -> Result<[u8; 16], NativeError> {
    let digits = hex.as_bytes();
    if digits.len() != 32 || !digits.iter().all(u8::is_ascii_hexdigit) {
        return Err(invalid("an identity is 32 hexadecimal digits"));
    }
    let mut bytes = [0; 16];
    for (byte, pair) in bytes.iter_mut().zip(hex.as_bytes().chunks(2)) {
        let pair = std::str::from_utf8(pair).map_err(invalid)?;
        *byte = u8::from_str_radix(pair, 16).map_err(invalid)?;
    }
    Ok(bytes)
}
impl Identity {
    fn parse(json: &str) -> Result<Self, NativeError> {
        serde_json::from_str(json).map_err(invalid)
    }
    fn node(&self) -> Result<NodeId, NativeError> {
        Ok(NodeId(id(&self.node)?))
    }
    fn principal(&self) -> Result<PrincipalId, NativeError> {
        Ok(PrincipalId(id(&self.principal)?))
    }
}

/// An in-process coordinator for development and tests. Its ledger is in
/// memory: every database that shares it lives in this process, and a
/// domain it confirmed for cannot make strong progress after the process
/// ends. It receives identities, generations and commitments, never
/// descriptors, arguments or rows.
#[derive(uniffi::Object, Default)]
pub struct DevelopmentCoordinator {
    inner: Arc<MemoryCoordinator>,
}
#[uniffi::export]
impl DevelopmentCoordinator {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::default()
    }
    /// Registers the domain of `owner`, a `command_identity` reply, with
    /// that device as its first executor. `executors` may take over;
    /// `callers` may invoke. Each is a `command_identity` reply.
    pub fn register(
        &self,
        owner: String,
        executors: Vec<String>,
        callers: Vec<String>,
    ) -> Result<(), NativeError> {
        let owner = Identity::parse(&owner)?;
        let mut eligible = vec![owner.node()?];
        let mut invokers = vec![(owner.node()?, owner.principal()?)];
        for (identity, executes) in executors
            .iter()
            .map(|identity| (identity, true))
            .chain(callers.iter().map(|identity| (identity, false)))
        {
            let identity = Identity::parse(identity)?;
            if identity.scope != owner.scope {
                return Err(invalid("identity of another domain"));
            }
            if executes {
                eligible.push(identity.node()?);
            }
            invokers.push((identity.node()?, identity.principal()?));
        }
        let (scope, control) = (owner.scope, owner.principal()?);
        self.inner
            .register(
                scope,
                control,
                [1; 16],
                eligible,
                owner.schema_generation,
                owner.handler_generation,
            )
            .map_err(invalid)?;
        self.inner
            .set_invokers(scope, control, invokers)
            .map_err(invalid)?;
        self.inner
            .acquire(scope, control, owner.node()?)
            .map_err(invalid)?;
        Ok(())
    }
    /// Fences the current executor and assigns `executor`, which `owner`
    /// registered as eligible.
    pub fn takeover(&self, owner: String, executor: String) -> Result<(), NativeError> {
        let owner = Identity::parse(&owner)?;
        let executor = Identity::parse(&executor)?;
        let epoch = self.inner.head(owner.scope).map_err(invalid)?.epoch;
        self.inner
            .takeover(owner.scope, owner.principal()?, executor.node()?, epoch)
            .map_err(invalid)?;
        Ok(())
    }
    /// The confirmed sequence of the domain of `identity`.
    pub fn sequence(&self, identity: String) -> Result<u64, NativeError> {
        let scope = Identity::parse(&identity)?.scope;
        Ok(self.inner.head(scope).map_err(invalid)?.sequence)
    }
    /// Activates the next definition generation of the domain of `owner`,
    /// which registered it: `activation` is a `command_activation` reply's
    /// `activation`. Returns the coordinator's new head as JSON.
    pub fn activate(&self, owner: String, activation: String) -> Result<String, NativeError> {
        let owner = Identity::parse(&owner)?;
        let activation: rocky_sdk::Activation =
            serde_json::from_str(&activation).map_err(invalid)?;
        if activation.head.scope != owner.scope {
            return Err(invalid("an activation of another domain"));
        }
        let head = self
            .inner
            .activate(owner.principal()?, &activation)
            .map_err(invalid)?;
        serde_json::to_string(&head).map_err(invalid)
    }
    /// Fault controls: an unreachable coordinator, and one whose next
    /// answer is lost after it accepted.
    pub fn set_available(&self, available: bool) -> Result<(), NativeError> {
        self.inner.set_available(available).map_err(invalid)
    }
    pub fn drop_next_reply(&self) -> Result<(), NativeError> {
        self.inner.drop_next_reply().map_err(invalid)
    }
}

#[cfg(test)]
mod read_tests {
    use super::*;
    use rocky_sdk::{HandlerReads, SqlValue};
    fn reads() -> Arc<NativeHandlerReads> {
        let rows = vec![
            vec![SqlValue::Text("wanted".into()), SqlValue::Integer(i64::MAX)],
            vec![
                SqlValue::Text("other".into()),
                SqlValue::Text("unrelated payload".into()),
            ],
        ];
        Arc::new(NativeHandlerReads {
            inner: RwLock::new(Some(
                HandlerReads::new([("stock".into(), (vec![0], rows))].into()).unwrap(),
            )),
        })
    }
    #[test]
    fn only_requested_rows_cross_and_retained_handles_expire() {
        let reads = reads();
        let lifetime = ReadLifetime(reads.clone());
        let key = r#"[{"type":"text","value":"wanted"}]"#;
        let row = reads.get("stock".into(), key.into()).unwrap();
        assert!(row.contains("9223372036854775807"));
        assert!(!row.contains("unrelated payload"));
        assert_eq!(
            reads
                .get(
                    "stock".into(),
                    r#"[{"type":"text","value":"missing"}]"#.into()
                )
                .unwrap(),
            "null"
        );
        let scan = reads
            .scan("stock".into(), Some(key.into()), None, 1)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&scan)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(reads.scan("stock".into(), None, None, 0).is_err());
        assert!(reads.get("undeclared".into(), key.into()).is_err());
        assert!(reads.get("stock".into(), "[]".into()).is_err());
        assert!(reads.get("stock".into(), "invalid".into()).is_err());
        drop(lifetime);
        assert!(
            reads
                .get("stock".into(), key.into())
                .unwrap_err()
                .to_string()
                .contains("expired")
        );
        assert!(reads.scan("stock".into(), None, None, 1).is_err());
    }
    #[test]
    fn callback_unwind_also_revokes_reads() {
        let reads = reads();
        let retained = reads.clone();
        let result = std::panic::catch_unwind(move || {
            let _lifetime = ReadLifetime(reads);
            panic!("foreign callback failed");
        });
        assert!(result.is_err());
        assert!(retained.scan("stock".into(), None, None, 1).is_err());
    }
}
