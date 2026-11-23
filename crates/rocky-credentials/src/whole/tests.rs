use super::*;
use crate::{Authority, CredentialChain, Granted, PrivateKey, RevocationSet};
use rocky_core::{
    DomainId, LogicalSchema, MeshId, PartitionTuple, PartitionValue, PrincipalKind, Scope,
};
use serde_json::json;

const TIME: u64 = 10_000;

struct Fixture {
    root: PrivateKey,
    identity: PrivateKey,
    node: PrivateKey,
    schema: LogicalSchema,
    domain: Scope,
    tables: WholeTables,
}
impl Fixture {
    fn new(plain: usize) -> Self {
        let root = PrivateKey::from_bytes(&[1; 32]);
        let publisher: String = root
            .public_key()
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let mut tables: Vec<_> = (0..plain)
            .map(|index| {
                json!({"name": format!("t{index:03}"), "class": "replayed",
                    "columns": [{"name": "body", "type": "text"}], "primary_key": ["id"]})
            })
            .collect();
        tables.push(json!({"name": "books", "class": "replayed",
            "columns": [{"name": "shelf", "type": "text", "not_null": true}],
            "primary_key": ["shelf", "id"], "partition_by": ["shelf"]}));
        tables.push(
            json!({"name": "registry", "class": "replayed", "explicit_access_only": true,
            "columns": [{"name": "entry", "type": "text"}], "primary_key": ["id"]}),
        );
        let schema = rocky_core::SchemaDefinition::parse(
            json!({"format": rocky_core::DEFINITION_FORMAT, "publisher": publisher,
                "application": "whole", "tables": tables})
            .to_string()
            .as_bytes(),
        )
        .unwrap()
        .build()
        .unwrap();
        let domain = Scope {
            mesh: MeshId([1; 16]),
            app: schema.app_id(),
            domain: DomainId([2; 16]),
        };
        let scopes: Vec<_> = schema
            .tables
            .iter()
            .map(|table| ResourceScope {
                domain,
                table: schema.table_id(table),
                encryption: schema.table_id(table),
            })
            .collect();
        let tables = WholeTables::new(&schema.manifest(&[domain.domain]), &scopes).unwrap();
        Self {
            root,
            identity: PrivateKey::from_bytes(&[3; 32]),
            node: PrivateKey::from_bytes(&[4; 32]),
            schema,
            domain,
            tables,
        }
    }
    fn scope(&self, name: &str) -> ResourceScope {
        let table = self.schema.table(name).unwrap();
        ResourceScope {
            domain: self.domain,
            table: self.schema.table_id(table),
            encryption: self.schema.table_id(table),
        }
    }
    fn chain(&self, granted: Granted) -> CredentialChain {
        let link = Authority::owner_in(
            &self.root,
            crate::DatabaseContext {
                publisher: self.root.public_key(),
                scope: self.domain,
            },
        )
        .unwrap()
        .issue_person(
            self.identity.public_key(),
            granted,
            crate::Validity {
                issued_at: TIME,
                lifetime: std::time::Duration::from_secs(3600),
                clock_skew: std::time::Duration::from_secs(0),
            },
        )
        .unwrap();
        crate::device_chain(link, &self.identity, &self.node).unwrap()
    }
    fn verified(&self, chain: &CredentialChain) -> crate::VerifiedChain {
        chain
            .verified(
                self.root.public_key(),
                &RevocationSet::new(),
                TIME,
                &self.tables,
            )
            .unwrap()
    }
    fn whole(&self, whole: WholeAccess) -> CredentialChain {
        self.chain(Granted {
            whole: Some(whole),
            ..Granted::default()
        })
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_wide_schema_under_full_access_stays_one_flag_and_skips_explicit_tables() {
    let fixture = Fixture::new(200);
    let chain = fixture.whole(WholeAccess::Full);
    let held = chain.held().unwrap();
    assert!(held.grants.is_empty() && held.extended().unwrap().partitions.is_empty());
    let (grants, partitions) = fixture
        .verified(&chain)
        .selectors(fixture.node.public_key(), &RevocationSet::new(), TIME)
        .unwrap();
    assert_eq!(
        grants.len(),
        201,
        "every covered table, but not the registry"
    );
    assert!(
        grants
            .iter()
            .all(|grant| grant.scope != fixture.scope("registry")
                && grant.rights == WholeAccess::Full.rights()
                && grant.fields == FieldSelection::All)
    );
    assert_eq!(
        partitions.len(),
        1,
        "the partitioned table also gets every partition"
    );
    assert_eq!(partitions[0].table, fixture.scope("books").table);
    assert!(
        fixture
            .verified(&chain)
            .author(
                fixture.node.public_key(),
                fixture.scope("registry").table,
                &RevocationSet::new(),
                TIME
            )
            .is_err(),
        "whole access never writes an explicitly granted table"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn whole_access_expands_only_against_the_tables_it_is_verified_with() {
    let fixture = Fixture::new(2);
    let chain = fixture.whole(WholeAccess::Read);
    let (grants, _) = chain
        .verified(
            fixture.root.public_key(),
            &RevocationSet::new(),
            TIME,
            &WholeTables::default(),
        )
        .unwrap()
        .selectors(fixture.node.public_key(), &RevocationSet::new(), TIME)
        .unwrap();
    assert!(grants.is_empty(), "no authenticated tables, no authority");
    let read = fixture.verified(&chain);
    let granted = read
        .author(
            fixture.node.public_key(),
            fixture.scope("t000").table,
            &RevocationSet::new(),
            TIME,
        )
        .unwrap();
    assert_eq!(granted[0].rights, [Right::Read, Right::Custody].into());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn store_access_holds_ciphertext_and_reads_nothing() {
    let fixture = Fixture::new(2);
    let (grants, _) = fixture
        .verified(&fixture.whole(WholeAccess::Store))
        .selectors(fixture.node.public_key(), &RevocationSet::new(), TIME)
        .unwrap();
    assert!(!grants.is_empty());
    assert!(
        grants
            .iter()
            .all(|grant| grant.rights == [Right::Custody].into()
                && grant.fields == FieldSelection::Only(BTreeSet::new()))
    );
    assert!(!WholeAccess::Store.reads());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn read_and_author_checks_accept_two_grants_on_one_scope() {
    let fixture = Fixture::new(1);
    let scope = fixture.scope("t000");
    let table = fixture.schema.table("t000").unwrap();
    let body = fixture
        .schema
        .field_id(table, table.column("body").unwrap());
    let chain = fixture.chain(Granted::explicit(vec![
        ResourceGrant {
            scope,
            rows: RowSelection::All,
            rights: [Right::Read, Right::Custody].into(),
            fields: FieldSelection::All,
        },
        ResourceGrant {
            scope,
            rows: RowSelection::All,
            rights: [Right::Update].into(),
            fields: FieldSelection::Only([body].into()),
        },
    ]));
    let verified = fixture.verified(&chain);
    let granted = verified
        .author(
            fixture.node.public_key(),
            scope.table,
            &RevocationSet::new(),
            TIME,
        )
        .unwrap();
    assert_eq!(granted.len(), 2);
    assert!(
        granted
            .iter()
            .any(|grant| grant.rights.contains(&Right::Read))
    );
    assert!(
        granted
            .iter()
            .any(|grant| grant.rights.contains(&Right::Update)
                && grant.fields == FieldSelection::Only([body].into()))
    );
    let books = fixture.scope("books");
    let tuple = PartitionTuple::new([(
        fixture.schema.field_id(
            fixture.schema.table("books").unwrap(),
            fixture
                .schema
                .table("books")
                .unwrap()
                .column("shelf")
                .unwrap(),
        ),
        PartitionValue::new(
            rocky_core::KeyColumn {
                affinity: rocky_core::Affinity::Text,
                collation: rocky_core::Collation::Binary,
            },
            rocky_core::SqlValue::Text("north".into()),
        )
        .unwrap(),
    )])
    .unwrap();
    assert!(
        fixture
            .verified(&fixture.whole(WholeAccess::Full))
            .partition(
                fixture.node.public_key(),
                books,
                &tuple,
                &RevocationSet::new(),
                TIME
            )
            .unwrap()
            .len()
            >= 2,
        "the table grant and the every-partition grant both resolve"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn only_a_collection_link_carries_whole_access() {
    let fixture = Fixture::new(1);
    let owner = Authority::owner_in(
        &fixture.root,
        crate::DatabaseContext {
            publisher: fixture.root.public_key(),
            scope: fixture.domain,
        },
    )
    .unwrap();
    let trusted = owner
        .issue_trusted(fixture.identity.public_key(), fixture.domain, TIME)
        .unwrap();
    let mut extended = trusted.capability.extended().unwrap();
    extended.whole = Some(WholeAccess::Full);
    let forged = crate::Capability {
        extension: crate::Extension::new(extended).unwrap(),
        ..trusted.capability.clone()
    };
    let forged = crate::SignedCapability::sign(forged, &fixture.root).unwrap();
    assert!(
        crate::device_chain(forged, &fixture.identity, &fixture.node)
            .unwrap()
            .verified(
                fixture.root.public_key(),
                &RevocationSet::new(),
                TIME,
                &fixture.tables,
            )
            .is_err()
    );
    assert_eq!(trusted.capability.kind, PrincipalKind::User);
}
