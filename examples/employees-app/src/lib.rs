//! Example resource policy: public employee names and private payroll companions.
use rocky_core::{
    ApplicationManifest, AuthorLogScope, Commitment, DefinitionItem, DomainId, DomainRule, MeshId,
    ResourceScope, RowApplication, RowChange, RowFields, Scope,
};
use std::collections::BTreeSet;

pub struct Employees {
    publisher: [u8; 32],
}
impl Employees {
    pub fn new(publisher: [u8; 32]) -> Self {
        Self { publisher }
    }
    pub fn definition(&self) -> ApplicationManifest {
        let mut manifest = ApplicationManifest {
            publisher: self.publisher,
            name: "employees".into(),
            app: ApplicationManifest::app_id(self.publisher, "employees"),
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
                rule: "Public employees with private 1:1 payroll companions".into(),
            }],
        };
        for (kind, names) in [
            ("entity", vec!["employees", "employee_private"]),
            ("field", vec!["display_name", "salary", "owner"]),
        ] {
            for name in names {
                let item = DefinitionItem {
                    id: manifest.item_id(kind, name),
                    name: name.into(),
                    shape: "UTF-8 scalar".into(),
                };
                if kind == "entity" {
                    manifest.entities.push(item);
                } else {
                    manifest.fields.push(item);
                }
            }
        }
        manifest.methods.push(DefinitionItem {
            id: manifest.item_id("method", "SetEmployee"),
            name: "SetEmployee".into(),
            shape: "employee ID, name and salary, separated by newlines".into(),
        });
        manifest
    }
    pub fn field(&self, name: &str) -> Commitment {
        self.definition().item_id("field", name)
    }
    pub fn scope(&self, mesh: MeshId, private: bool) -> ResourceScope {
        let manifest = self.definition();
        let name = if private {
            "employee_private"
        } else {
            "employees"
        };
        ResourceScope {
            domain: Scope {
                mesh,
                app: manifest.app,
                domain: DomainId([1; 16]),
            },
            encryption: rocky_core::commitment("employees.reader-set.v1", name.as_bytes()),
            table: manifest.item_id("entity", name),
        }
    }
    pub fn name(&self, value: &str) -> RowFields {
        [(self.field("display_name"), value.as_bytes().to_vec())].into()
    }
    pub fn salary(&self, value: &str) -> RowFields {
        [(self.field("salary"), value.as_bytes().to_vec())].into()
    }
}
impl RowApplication for Employees {
    fn manifest(&self) -> ApplicationManifest {
        self.definition()
    }
    fn permission_fields(&self) -> BTreeSet<Commitment> {
        [self.field("owner")].into()
    }
    fn validate_changes(&self, changes: &[RowChange]) -> Result<(), String> {
        let manifest = self.definition();
        for change in changes {
            let field = if change.scope.table == manifest.item_id("entity", "employees") {
                self.field("display_name")
            } else if change.scope.table == manifest.item_id("entity", "employee_private") {
                self.field("salary")
            } else {
                return Err("unknown employee table".into());
            };
            for fields in change.before.iter().chain(change.after.iter()) {
                if !fields.contains_key(&field)
                    || fields
                        .keys()
                        .any(|id| *id != field && *id != self.field("owner"))
                {
                    return Err("row must contain its declared field".into());
                }
                for bytes in fields.values() {
                    let value = std::str::from_utf8(bytes).map_err(|_| "field is not UTF-8")?;
                    if value.is_empty() || value.len() > 200 {
                        return Err("invalid employee field".into());
                    }
                }
                let value =
                    std::str::from_utf8(&fields[&field]).map_err(|_| "field is not UTF-8")?;
                if value.is_empty() || value.len() > 200 {
                    return Err("invalid employee field".into());
                }
                if field == self.field("salary") && value.parse::<u64>().is_err() {
                    return Err("salary must be an unsigned integer".into());
                }
            }
        }
        Ok(())
    }
}

impl rocky_core::RowCommandApplication for Employees {
    fn command_resources(&self, scope: Scope, method: &str) -> Result<Vec<ResourceScope>, String> {
        if method != "SetEmployee" || scope != self.scope(scope.mesh, false).domain {
            return Err("unknown employee command/domain".into());
        }
        Ok(vec![
            self.scope(scope.mesh, false),
            self.scope(scope.mesh, true),
        ])
    }
    fn command_invocation_scope(
        &self,
        scope: Scope,
        method: &str,
    ) -> Result<ResourceScope, String> {
        self.command_resources(scope, method)?;
        Ok(self.scope(scope.mesh, true))
    }
    fn execute_command(
        &self,
        method: &str,
        arguments: &[u8],
        state: &rocky_core::CommandRows,
    ) -> Result<rocky_core::RowCommandOutput, String> {
        if method != "SetEmployee" {
            return Err("unknown employee command".into());
        }
        let text = std::str::from_utf8(arguments).map_err(|_| "invalid command UTF-8")?;
        let parts: Vec<_> = text.split('\n').collect();
        if parts.len() != 3 && parts.len() != 4 {
            return Err("expected employee ID, name and salary".into());
        }
        let row = rocky_core::commitment("employees.id.v1", parts[0].as_bytes());
        let changes = state
            .iter()
            .map(|(scope, rows)| RowChange {
                scope: *scope,
                row,
                before: rows.get(&row).cloned(),
                after: Some({
                    let mut fields =
                        if scope.table == self.definition().item_id("entity", "employees") {
                            self.name(parts[1])
                        } else {
                            self.salary(parts[2])
                        };
                    if let Some(owner) = parts.get(3) {
                        fields.insert(self.field("owner"), owner.as_bytes().to_vec());
                    }
                    fields
                }),
            })
            .filter(|change| change.before != change.after)
            .collect();
        Ok(rocky_core::RowCommandOutput {
            changes,
            outputs: vec![],
        })
    }
}
