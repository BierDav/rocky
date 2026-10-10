use super::*;
use rocky_core::{Affinity, AppId, Collation, DomainId, KeyColumn, MeshId, Scope};

fn member(table: &str, number: u8, parents: &[&str]) -> Member {
    let value = PartitionValue::new(
        KeyColumn {
            affinity: Affinity::Text,
            collation: Collation::Binary,
        },
        SqlValue::Text("A".into()),
    )
    .unwrap();
    let tuple = PartitionTuple::new([([number; 32], value)]).unwrap();
    let domain = Scope {
        mesh: MeshId([1; 16]),
        app: AppId([2; 16]),
        domain: DomainId([3; 16]),
    };
    Member {
        table: table.into(),
        scope: tuple.scope(domain, [number; 32]).unwrap(),
        tuple,
        parents: parents.iter().map(|parent| (*parent).into()).collect(),
    }
}
fn group() -> GroupPartition {
    GroupPartition {
        record: GroupRecord {
            group: "list".into(),
            values: Vec::new(),
            address: [4; 32],
            epochs: Vec::new(),
        },
        members: vec![
            member("lists", 1, &[]),
            member("items", 2, &["lists"]),
            member("attachments", 3, &["lists"]),
        ],
    }
}
fn names(members: Vec<&Member>) -> Vec<&str> {
    members.iter().map(|member| member.table.as_str()).collect()
}
fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).into()).collect()
}

#[test]
fn a_held_table_keeps_the_in_group_tables_it_references() {
    let group = group();
    assert_eq!(
        names(group.covered(&set(&[])).unwrap()),
        ["lists", "items", "attachments"]
    );
    assert_eq!(
        names(group.covered(&set(&["attachments"])).unwrap()),
        ["lists", "items"]
    );
    let refused = group.covered(&set(&["lists"])).unwrap_err().to_string();
    assert!(refused.contains("items references lists"), "{refused}");
    assert!(group.covered(&set(&["photos"])).is_err());
    assert!(
        group
            .covered(&set(&["lists", "items", "attachments"]))
            .is_err()
    );
    // Releasing keeps the named tables; a kept child needs its parent.
    assert_eq!(
        names(group.released(&set(&[])).unwrap()),
        ["lists", "items", "attachments"]
    );
    assert_eq!(
        names(group.released(&set(&["lists", "items"])).unwrap()),
        ["attachments"]
    );
    let refused = group.released(&set(&["items"])).unwrap_err().to_string();
    assert!(refused.contains("keeping it without lists"), "{refused}");
}

#[test]
fn group_epochs_are_bounded_before_a_secret_is_stored() {
    let mut record = group().record;
    for epoch in 1..=MAX_GROUP_EPOCHS as u64 {
        record
            .hold(rocky_core::KeyEpoch::new(
                1,
                &rocky_core::commitment("test.sibling", &epoch.to_be_bytes()),
            ))
            .unwrap();
    }
    assert!(
        record
            .admit(rocky_core::KeyEpoch::new(
                1,
                &rocky_core::commitment("test.sibling", &(MAX_GROUP_EPOCHS as u64).to_be_bytes())
            ))
            .is_ok()
    );
    assert!(
        record
            .admit(rocky_core::KeyEpoch::new(1, &[255; 32]))
            .is_err()
    );
    assert_eq!(record.current(), record.epochs.iter().max().copied());
}
