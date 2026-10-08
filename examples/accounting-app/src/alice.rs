//! Alice records explicit Accounting observations and app-owned review intents.
use rocky_core::*;
use std::collections::BTreeSet;
pub struct Alice {
    publisher: [u8; 32],
}
impl Alice {
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
            encryption: commitment("alice.readers.v1", b"checkboxes"),
            table: self.manifest().item_id("entity", "checkboxes"),
        }
    }
    pub fn jobs_scope(&self, mesh: MeshId) -> ResourceScope {
        ResourceScope {
            table: self.manifest().item_id("entity", "jobs"),
            ..self.scope(mesh)
        }
    }
}
impl RowApplication for Alice {
    fn manifest(&self) -> ApplicationManifest {
        let mut manifest = ApplicationManifest {
            publisher: self.publisher,
            name: "alice".into(),
            app: ApplicationManifest::app_id(self.publisher, "alice"),
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
                rule: "Observed Accounting events and app-owned checkboxes/jobs".into(),
            }],
        };
        for (kind, names) in [
            ("entity", vec!["checkboxes", "jobs"]),
            (
                "field",
                vec![
                    "event",
                    "accounting_revision",
                    "accounting_head",
                    "checked",
                    "workflow_revision",
                    "intent",
                ],
            ),
            ("method", vec!["CheckEvent"]),
        ] {
            for name in names {
                let item = DefinitionItem {
                    id: manifest.item_id(kind, name),
                    name: name.into(),
                    shape: "Recorded workflow input".into(),
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
            let checkbox = change.scope.table == self.manifest().item_id("entity", "checkboxes");
            if !checkbox && change.scope.table != self.manifest().item_id("entity", "jobs") {
                return Err("unknown Alice table".into());
            }
            for fields in change.before.iter().chain(change.after.iter()) {
                let names = if checkbox {
                    vec![
                        "event",
                        "accounting_revision",
                        "accounting_head",
                        "checked",
                        "workflow_revision",
                    ]
                } else {
                    vec!["intent"]
                };
                if fields.len() != names.len()
                    || !names
                        .iter()
                        .all(|name| fields.contains_key(&self.field(name)))
                {
                    return Err("invalid Alice fields".into());
                }
                if checkbox {
                    if fields[&self.field("event")].is_empty()
                        || fields[&self.field("accounting_head")].len() != 64
                        || !matches!(fields[&self.field("checked")].as_slice(), [0] | [1])
                    {
                        return Err("invalid observed event".into());
                    }
                    let revision: u64 =
                        std::str::from_utf8(&fields[&self.field("accounting_revision")])
                            .map_err(|_| "invalid observation revision")?
                            .parse()
                            .map_err(|_| "invalid observation revision")?;
                    let workflow: u64 =
                        std::str::from_utf8(&fields[&self.field("workflow_revision")])
                            .map_err(|_| "invalid workflow revision")?
                            .parse()
                            .map_err(|_| "invalid workflow revision")?;
                    if revision == 0 || workflow == 0 {
                        return Err("invalid observation revision".into());
                    }
                }
            }
        }
        Ok(())
    }
}
impl RowCommandApplication for Alice {
    fn command_resources(&self, scope: Scope, method: &str) -> Result<Vec<ResourceScope>, String> {
        if scope != self.scope(scope.mesh).domain || method != "CheckEvent" {
            return Err("unknown Alice method/domain".into());
        }
        Ok(vec![self.scope(scope.mesh), self.jobs_scope(scope.mesh)])
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
        if method != "CheckEvent" {
            return Err("unknown Alice method".into());
        }
        let input = std::str::from_utf8(arguments).map_err(|_| "invalid Alice input")?;
        let parts: Vec<_> = input.split('\n').collect();
        if parts.len() != 4
            || parts[0].is_empty()
            || parts[2].len() != 64
            || !parts[2].bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("expected event ID, observed revision/head and checked flag".into());
        }
        let revision: u64 = parts[1].parse().map_err(|_| "invalid observed revision")?;
        if revision == 0 {
            return Err("invalid observed revision".into());
        }
        let checked = match parts[3] {
            "0" => false,
            "1" => true,
            _ => return Err("invalid checkbox".into()),
        };
        let mesh = state
            .keys()
            .next()
            .ok_or("missing Alice state")?
            .domain
            .mesh;
        let scope = self.scope(mesh);
        let row = commitment("alice.checkbox.v1", parts[0].as_bytes());
        let mut after: RowFields = [
            (self.field("event"), parts[0].as_bytes().to_vec()),
            (
                self.field("accounting_revision"),
                parts[1].as_bytes().to_vec(),
            ),
            (self.field("accounting_head"), parts[2].as_bytes().to_vec()),
            (self.field("checked"), vec![u8::from(checked)]),
        ]
        .into();
        let before = state
            .get(&scope)
            .ok_or("missing checkboxes")?
            .get(&row)
            .cloned();
        if before.as_ref().is_some_and(|before| {
            after
                .iter()
                .all(|(field, value)| before.get(field) == Some(value))
        }) {
            return Ok(RowCommandOutput {
                changes: vec![],
                outputs: vec![],
            });
        }
        let workflow_revision = before
            .as_ref()
            .map(|fields| {
                let bytes = fields
                    .get(&self.field("workflow_revision"))
                    .ok_or("missing workflow revision")?;
                std::str::from_utf8(bytes)
                    .map_err(|_| "invalid workflow revision")?
                    .parse::<u64>()
                    .map_err(|_| "invalid workflow revision")
            })
            .transpose()?
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("workflow revision exhausted")?;
        after.insert(
            self.field("workflow_revision"),
            workflow_revision.to_string().into_bytes(),
        );
        let jobs = self.jobs_scope(mesh);
        let mut identity = arguments.to_vec();
        identity.extend(workflow_revision.to_le_bytes());
        let job_id = commitment("alice.review-intent.v1", &identity);
        let intent = format!("{}\n{}\n{}", parts[0], revision, u8::from(checked)).into_bytes();
        let mut changes = vec![RowChange {
            scope,
            row,
            before,
            after: Some(after),
        }];
        let mut outputs = vec![];
        if !state
            .get(&jobs)
            .ok_or("missing jobs")?
            .contains_key(&job_id)
        {
            changes.push(RowChange {
                scope: jobs,
                row: job_id,
                before: None,
                after: Some([(self.field("intent"), intent.clone())].into()),
            });
            outputs.push(ScopedCommandOutput {
                scope: jobs,
                kind: CommandOutputKind::Job,
                bytes: intent,
            });
        }
        Ok(RowCommandOutput { changes, outputs })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_checkbox_toggles_have_distinct_jobs_but_identical_state_is_a_noop() {
        let app = Alice::new([1; 32]);
        let scope = app.scope(MeshId([2; 16]));
        let jobs = app.jobs_scope(scope.domain.mesh);
        let mut state: CommandRows =
            [(scope, Default::default()), (jobs, Default::default())].into();
        let mut identities = BTreeSet::new();
        for flag in [1, 0, 1] {
            let input = format!("event\n1\n{}\n{flag}", "ab".repeat(32));
            let output = app
                .execute_command("CheckEvent", input.as_bytes(), &state)
                .unwrap();
            app.validate_changes(&output.changes).unwrap();
            assert_eq!(output.outputs.len(), 1);
            for change in output.changes {
                if change.scope == jobs {
                    assert!(identities.insert(change.row));
                }
                state
                    .get_mut(&change.scope)
                    .unwrap()
                    .insert(change.row, change.after.unwrap());
            }
            let replay = app
                .execute_command("CheckEvent", input.as_bytes(), &state)
                .unwrap();
            assert!(replay.changes.is_empty());
            assert!(replay.outputs.is_empty());
        }
        assert_eq!(identities.len(), 3);
    }
}
