//! Logical database bindings: the tables one SQL session names and the
//! domain each belongs to. See `docs/runtime/database-bindings.md`.
use crate::{Commitment, LogicalSchema, Scope, commitment, identifier};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Tables and domains one binding may name.
pub const MAX_BOUND_TABLES: usize = 4096;
pub const MAX_BOUND_DOMAINS: usize = 64;
const MAX_BINDING_BYTES: usize = 1024 * 1024;

/// One table a session sees: an exact table of one domain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundTable {
    /// The name statements use.
    pub name: String,
    pub scope: Scope,
    /// SQL name of the table in the domain's logical schema.
    pub table: String,
}
/// The tables visible through one SQL session. It grants nothing: each
/// domain's installed policy still admits every operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseBinding {
    pub name: String,
    /// Starts at one and grows by exactly one whenever the tables change.
    pub generation: u64,
    /// Sorted by lowercase name.
    pub tables: Vec<BoundTable>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingError {
    Invalid(String),
    /// Two tables would answer to one name, or one table to two.
    Ambiguous(String),
}
impl std::fmt::Display for BindingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid database binding: {reason}"),
            Self::Ambiguous(name) => write!(f, "ambiguous bound table {name}"),
        }
    }
}
impl std::error::Error for BindingError {}

fn reserved(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("rocky_") || name.starts_with("sqlite_")
}
impl DatabaseBinding {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            generation: 1,
            tables: Vec::new(),
        }
    }
    /// The generated default: every table of one domain under its own name.
    pub fn single(
        name: impl Into<String>,
        scope: Scope,
        schema: &LogicalSchema,
    ) -> Result<Self, BindingError> {
        Self::new(name).domain(scope, schema)
    }
    pub fn generation(mut self, generation: u64) -> Self {
        self.generation = generation;
        self
    }
    /// Binds every table of `schema` in `scope` under its own SQL name.
    pub fn domain(mut self, scope: Scope, schema: &LogicalSchema) -> Result<Self, BindingError> {
        for table in &schema.tables {
            self = self.table(table.sql_name.clone(), scope, table.sql_name.clone())?;
        }
        Ok(self)
    }
    /// Binds every table of `schema` in `scope` as `alias_table`, so several
    /// domains of one schema can share a session.
    pub fn domain_as(
        mut self,
        alias: &str,
        scope: Scope,
        schema: &LogicalSchema,
    ) -> Result<Self, BindingError> {
        for table in &schema.tables {
            let name = format!("{alias}_{}", table.sql_name);
            self = self.table(name, scope, table.sql_name.clone())?;
        }
        Ok(self)
    }
    pub fn table(
        mut self,
        name: impl Into<String>,
        scope: Scope,
        table: impl Into<String>,
    ) -> Result<Self, BindingError> {
        let bound = BoundTable {
            name: name.into(),
            scope,
            table: table.into(),
        };
        let key = bound.name.to_ascii_lowercase();
        if self.tables.iter().any(|other| {
            other.name.to_ascii_lowercase() == key
                || (other.scope == scope && other.table == bound.table)
        }) {
            return Err(BindingError::Ambiguous(bound.name));
        }
        let position = self
            .tables
            .partition_point(|other| other.name.to_ascii_lowercase() < key);
        self.tables.insert(position, bound);
        Ok(self)
    }
    /// The bound domains, sorted.
    pub fn scopes(&self) -> Vec<Scope> {
        let scopes: BTreeSet<_> = self.tables.iter().map(|table| table.scope).collect();
        scopes.into_iter().collect()
    }
    pub fn validate(&self) -> Result<(), BindingError> {
        let invalid = |reason: &str| Err(BindingError::Invalid(reason.into()));
        if !identifier(&self.name) {
            return invalid("name");
        }
        if self.generation == 0 || self.generation > i64::MAX as u64 {
            return invalid("generation");
        }
        if self.tables.is_empty() || self.tables.len() > MAX_BOUND_TABLES {
            return invalid("between one and 4096 tables");
        }
        if self.scopes().len() > MAX_BOUND_DOMAINS {
            return invalid("at most 64 domains");
        }
        let (mut names, mut resources) = (BTreeSet::new(), BTreeSet::new());
        let mut previous = None;
        for table in &self.tables {
            if !identifier(&table.name) || reserved(&table.name) || table.table.is_empty() {
                return Err(BindingError::Invalid(format!("table name {}", table.name)));
            }
            let key = table.name.to_ascii_lowercase();
            if !names.insert(key.clone()) || !resources.insert((table.scope, &table.table)) {
                return Err(BindingError::Ambiguous(table.name.clone()));
            }
            if previous.is_some_and(|previous| previous > key) {
                return invalid("tables are not sorted by name");
            }
            previous = Some(key);
        }
        Ok(())
    }
    /// Canonical bytes: the JSON encoding of a valid binding.
    pub fn canonical(&self) -> Result<Vec<u8>, BindingError> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|error| BindingError::Invalid(error.to_string()))
    }
    /// Accepts only bytes `canonical` would produce.
    pub fn parse(bytes: &[u8]) -> Result<Self, BindingError> {
        if bytes.len() > MAX_BINDING_BYTES {
            return Err(BindingError::Invalid("binding quota".into()));
        }
        let binding: Self = serde_json::from_slice(bytes)
            .map_err(|error| BindingError::Invalid(error.to_string()))?;
        if binding.canonical()? != bytes {
            return Err(BindingError::Invalid("binding is not canonical".into()));
        }
        Ok(binding)
    }
    /// Commits to the name, the generation and every bound table.
    pub fn identity(&self) -> Result<Commitment, BindingError> {
        Ok(commitment("rocky.database-binding.v1", &self.canonical()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppId, DomainId, MeshId};

    fn scope(domain: u8) -> Scope {
        Scope {
            mesh: MeshId([1; 16]),
            app: AppId([2; 16]),
            domain: DomainId([domain; 16]),
        }
    }
    #[test]
    fn names_and_resources_are_exact_and_the_encoding_is_canonical() {
        let binding = DatabaseBinding::new("shop")
            .table("b_items", scope(2), "items")
            .and_then(|binding| binding.table("A_items", scope(1), "items"))
            .unwrap();
        assert_eq!(binding.tables[0].name, "A_items");
        assert_eq!(binding.scopes(), [scope(1), scope(2)]);
        let bytes = binding.canonical().unwrap();
        assert_eq!(DatabaseBinding::parse(&bytes).unwrap(), binding);
        assert_ne!(
            binding.identity().unwrap(),
            binding.clone().generation(2).identity().unwrap()
        );
        // One name for two tables, and two names for one table.
        for (name, domain) in [("a_ITEMS", 3), ("other", 1)] {
            assert!(matches!(
                binding.clone().table(name, scope(domain), "items"),
                Err(BindingError::Ambiguous(_))
            ));
        }
        for name in ["", "rocky_items", "sqlite_x", "a b", "1a"] {
            let invalid = DatabaseBinding::new("shop").table(name, scope(1), "items");
            assert!(invalid.unwrap().validate().is_err(), "{name}");
        }
        let mut unsorted = binding.clone();
        unsorted.tables.reverse();
        assert!(unsorted.validate().is_err());
        assert!(DatabaseBinding::new("shop").validate().is_err());
        assert!(binding.generation(0).validate().is_err());
    }
}
