//! Accounting owns event conditions and review authorization; `Alice` records observed
//! checkboxes and review intents. See the recovery/effects guide.
mod alice;
pub use alice::Alice;
use rocky_core::*;
use std::collections::BTreeSet;
pub struct Accounting {
    publisher: [u8; 32],
}
impl Accounting {
    pub fn new(publisher: [u8; 32]) -> Self {
        Self { publisher }
    }
    pub fn field(&self, name: &str) -> Commitment {
        self.manifest().item_id("field", name)
    }
    pub fn scope(&self, mesh: MeshId) -> ResourceScope {
        ResourceScope {
            domain: Scope {
                mesh,
                app: self.manifest().app,
                domain: DomainId([1; 16]),
            },
            encryption: commitment("accounting.readers.v1", b"events"),
            table: self.manifest().item_id("entity", "events"),
        }
    }
    pub fn event(&self, revision: u64, settled: bool, reviewed: bool) -> RowFields {
        [
            (self.field("revision"), revision.to_string().into_bytes()),
            (self.field("settled"), vec![u8::from(settled)]),
            (self.field("reviewed"), vec![u8::from(reviewed)]),
        ]
        .into()
    }
    pub fn revision(&self, fields: &RowFields) -> Result<u64, String> {
        let revision = std::str::from_utf8(
            fields
                .get(&self.field("revision"))
                .ok_or("missing event revision")?,
        )
        .map_err(|_| "invalid event revision")?
        .parse()
        .map_err(|_| "invalid event revision")?;
        if revision == 0 {
            return Err("event revision must be positive".into());
        }
        Ok(revision)
    }
}
impl RowApplication for Accounting {
    fn manifest(&self) -> ApplicationManifest {
        let mut manifest = ApplicationManifest {
            publisher: self.publisher,
            name: "accounting".into(),
            app: ApplicationManifest::app_id(self.publisher, "accounting"),
            schema: 1,
            projection: 1,
            handler: 1,
            entities: vec![],
            fields: vec![],
            methods: vec![],
            operations: vec![],
            domains: vec![DomainRule {
                domain: DomainId([1; 16]),
                author_log: AuthorLogScope::Domain,
                rule: "Accounting owns current event conditions".into(),
            }],
        };
        for (kind, names) in [
            ("entity", vec!["events"]),
            ("field", vec!["revision", "settled", "reviewed"]),
            ("method", vec!["SetEvent", "ReviewEvent"]),
        ] {
            for name in names {
                let item = DefinitionItem {
                    id: manifest.item_id(kind, name),
                    name: name.into(),
                    shape: "Versioned accounting event".into(),
                };
                match kind {
                    "entity" => manifest.entities.push(item),
                    "field" => manifest.fields.push(item),
                    _ => manifest.methods.push(item),
                }
            }
        }
        manifest
    }
    fn permission_fields(&self) -> BTreeSet<Commitment> {
        BTreeSet::new()
    }
    fn validate_changes(&self, changes: &[RowChange]) -> Result<(), String> {
        for change in changes {
            if change.scope.table != self.manifest().item_id("entity", "events") {
                return Err("unknown Accounting table".into());
            }
            for fields in change.before.iter().chain(change.after.iter()) {
                if fields.len() != 3
                    || !fields.keys().all(|key| {
                        [
                            self.field("revision"),
                            self.field("settled"),
                            self.field("reviewed"),
                        ]
                        .contains(key)
                    })
                {
                    return Err("invalid event fields".into());
                }
                self.revision(fields)?;
                for field in ["settled", "reviewed"] {
                    if !matches!(
                        fields.get(&self.field(field)).map(Vec::as_slice),
                        Some([0]) | Some([1])
                    ) {
                        return Err("invalid event flag".into());
                    }
                }
            }
        }
        Ok(())
    }
}
impl RowCommandApplication for Accounting {
    fn command_resources(&self, scope: Scope, method: &str) -> Result<Vec<ResourceScope>, String> {
        if scope != self.scope(scope.mesh).domain || !["SetEvent", "ReviewEvent"].contains(&method)
        {
            return Err("unknown Accounting method/domain".into());
        }
        Ok(vec![self.scope(scope.mesh)])
    }
    fn command_invocation_scope(
        &self,
        scope: Scope,
        method: &str,
    ) -> Result<ResourceScope, String> {
        Ok(self.command_resources(scope, method)?[0])
    }
    fn execute_command(
        &self,
        method: &str,
        arguments: &[u8],
        state: &CommandRows,
    ) -> Result<RowCommandOutput, String> {
        let input = std::str::from_utf8(arguments).map_err(|_| "invalid Accounting input")?;
        let parts: Vec<_> = input.split('\n').collect();
        if parts.len() != 3 || parts[0].is_empty() {
            return Err("expected event ID, revision and flag".into());
        }
        let revision: u64 = parts[1].parse().map_err(|_| "invalid revision")?;
        let flag = match parts[2] {
            "0" => false,
            "1" => true,
            _ => return Err("invalid flag".into()),
        };
        let scope = *state.keys().next().ok_or("missing Accounting state")?;
        let row = commitment("accounting.event.v1", parts[0].as_bytes());
        let before = state[&scope].get(&row).cloned();
        let after = match method {
            "SetEvent" => {
                let expected = before
                    .as_ref()
                    .map(|fields| self.revision(fields))
                    .transpose()?
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or("revision exhausted")?;
                if revision != expected {
                    return Err("stale Accounting event version".into());
                }
                self.event(revision, flag, false)
            }
            "ReviewEvent" => {
                let mut fields = before.clone().ok_or("unknown Accounting event")?;
                if self.revision(&fields)? != revision
                    || fields.get(&self.field("settled")).map(Vec::as_slice) != Some(&[1])
                {
                    return Err("Accounting event is stale or unsettled".into());
                }
                fields.insert(self.field("reviewed"), vec![u8::from(flag)]);
                fields
            }
            _ => return Err("unknown Accounting method".into()),
        };
        let changes = if before.as_ref() == Some(&after) {
            vec![]
        } else {
            vec![RowChange {
                scope,
                row,
                before,
                after: Some(after),
            }]
        };
        Ok(RowCommandOutput {
            changes,
            outputs: vec![],
        })
    }
}
