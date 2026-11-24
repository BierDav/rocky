use super::*;

fn group() -> GroupPartition {
    GroupPartition {
        record: GroupRecord {
            group: "list".into(),
            values: Vec::new(),
            address: [4; 32],
            name: [5; 32],
            epochs: Vec::new(),
        },
        members: Vec::new(),
    }
}

#[test]
fn group_epochs_are_bounded_before_a_secret_is_stored() {
    let mut record = group().record;
    let mut epochs: Vec<_> = (1..=MAX_GROUP_EPOCHS as u64)
        .map(|epoch| {
            rocky_core::KeyEpoch::new(
                1,
                &rocky_core::commitment("test.sibling", &epoch.to_be_bytes()),
            )
        })
        .collect();
    epochs.sort_unstable();
    let last = epochs.pop().unwrap();
    record.epochs = epochs;
    record.hold(last).unwrap();
    assert_eq!(record.epochs.len(), MAX_GROUP_EPOCHS);
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
    let held = record.epochs.clone();
    record.hold(last).unwrap();
    assert!(
        record
            .hold(rocky_core::KeyEpoch::new(1, &[255; 32]))
            .is_err()
    );
    assert_eq!(record.epochs, held);
    assert_eq!(record.current(), record.epochs.iter().max().copied());
}
