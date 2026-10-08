use rocky_core::*;
use todo_app::*;

fn id(node: NodeId, sequence: u64) -> OperationId {
    OperationId {
        node,
        sequence: AuthorSequence::new(sequence).unwrap(),
    }
}
fn op(node: NodeId, sequence: u64, time: u64, entity: OperationId, action: Action) -> Envelope {
    let id = id(node, sequence);
    Envelope {
        scope: fixtures::SCOPE,
        caller: fixtures::OWNER,
        versions: Versions::V1,
        id,
        entity,
        lamport: LamportTime::new(time).unwrap(),
        dependencies: required_dependencies(
            id,
            entity,
            matches!(action, Action::CreateTodo { .. }),
        )
        .unwrap(),
        payload: action.encode().unwrap(),
    }
}
fn create(node: NodeId, sequence: u64, time: u64, title: &str) -> Envelope {
    op(
        node,
        sequence,
        time,
        id(node, sequence),
        Action::CreateTodo {
            title: title.into(),
            completed: false,
        },
    )
}
fn apply(
    state: &mut TodoState,
    history: &mut AppliedOperations,
    operation: &Envelope,
) -> Result<Delivery, Error> {
    TodoApp.apply_ready(
        state,
        history,
        operation,
        &StaticPolicy::default(),
        AdmissionBoundary::Incoming,
    )
}
fn permutations<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
    if items.is_empty() {
        return vec![vec![]];
    }
    let mut result = vec![];
    for i in 0..items.len() {
        let mut tail = items.to_vec();
        let head = tail.remove(i);
        for mut order in permutations(&tail) {
            order.insert(0, head.clone());
            result.push(order);
        }
    }
    result
}
// Drain only ready work after each delivery, exercising real reducer order
// independently of replay's canonical topological schedule.
fn delivered(order: &[Envelope]) -> Replay {
    let mut state = TodoState::default();
    let mut history = AppliedOperations::new(fixtures::SCOPE);
    let mut pending = vec![];
    for operation in order {
        pending.push(operation.clone());
        loop {
            let mut progress = false;
            let mut remaining = vec![];
            for operation in pending {
                match apply(&mut state, &mut history, &operation).unwrap() {
                    Delivery::Pending(_) => remaining.push(operation),
                    Delivery::Ready | Delivery::Duplicate => progress = true,
                }
            }
            pending = remaining;
            if !progress {
                break;
            }
        }
    }
    assert!(pending.is_empty());
    Replay {
        state,
        applied: history,
    }
}
#[test]
fn payload_golden_frames_and_exact_utf8() {
    let fixtures = [
        (
            Action::CreateTodo {
                title: "A".into(),
                completed: false,
            },
            vec![0, 1, 0, 0, 0, 65, 0],
        ),
        (Action::SetTitle("B".into()), vec![1, 1, 0, 0, 0, 66]),
        (Action::SetCompleted(true), vec![2, 1]),
        (Action::DeleteTodo, vec![3]),
    ];
    for (action, bytes) in fixtures {
        assert_eq!(action.encode().unwrap(), bytes);
        assert_eq!(Action::decode(&bytes).unwrap(), action);
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            Action::decode(&trailing),
            Err(Error::Protocol(ProtocolError::TrailingBytes))
        );
        for end in 0..bytes.len() {
            assert!(Action::decode(&bytes[..end]).is_err());
        }
    }
    let action = Action::CreateTodo {
        title: " e\u{301} 🦀 \n".into(),
        completed: true,
    };
    assert_eq!(Action::decode(&action.encode().unwrap()).unwrap(), action);
    let title = Action::SetTitle("é".repeat(512));
    assert_eq!(Action::decode(&title.encode().unwrap()).unwrap(), title);
    for title in ["".into(), "\t \n\u{2003}".into(), "é".repeat(513)] {
        assert_eq!(Action::SetTitle(title).encode(), Err(Error::InvalidTitle));
    }
    assert_eq!(Action::decode(&[2, 2]), Err(Error::InvalidBoolean));
    assert_eq!(
        Action::decode(&[0, 1, 0, 0, 0, 65, 9]),
        Err(Error::InvalidBoolean)
    );
    assert_eq!(
        Action::decode(&[1, 1, 0, 0, 0, 255]),
        Err(Error::InvalidUtf8)
    );
    assert_eq!(
        Action::decode(&[1, 255, 255, 255, 255]),
        Err(Error::InvalidTitle)
    );
    assert_eq!(Action::decode(&[4]), Err(Error::UnknownAction(4)));
}
#[test]
fn all_delivery_orders_converge_with_independently_specified_winners() {
    let initial = create(fixtures::LAPTOP, 1, 1, "initial");
    let laptop_title = op(
        fixtures::LAPTOP,
        2,
        2,
        initial.id,
        Action::SetTitle("Laptop".into()),
    );
    let phone_title = op(
        fixtures::PHONE,
        1,
        2,
        initial.id,
        Action::SetTitle("Phone".into()),
    );
    let phone_completed = op(
        fixtures::PHONE,
        2,
        3,
        initial.id,
        Action::SetCompleted(true),
    );
    let second = create(fixtures::PHONE, 3, 4, "second");
    let operations = vec![
        initial.clone(),
        laptop_title.clone(),
        phone_title.clone(),
        phone_completed.clone(),
        second.clone(),
    ];
    let expected_entry = TodoEntry::Live {
        title: Register {
            value: "Phone".into(),
            stamp: Stamp {
                lamport: LamportTime::new(2).unwrap(),
                node: fixtures::PHONE,
                sequence: AuthorSequence::new(1).unwrap(),
            },
        },
        completed: Register {
            value: true,
            stamp: Stamp {
                lamport: LamportTime::new(3).unwrap(),
                node: fixtures::PHONE,
                sequence: AuthorSequence::new(2).unwrap(),
            },
        },
    };
    let expected = replay(operations.clone(), &StaticPolicy::default()).unwrap();
    assert_eq!(expected.state.entry(initial.id), Some(&expected_entry));
    assert_eq!(
        expected.state.visible_rows(),
        vec![
            VisibleRow {
                id: initial.id,
                title: "Phone".into(),
                completed: true
            },
            VisibleRow {
                id: second.id,
                title: "second".into(),
                completed: false
            }
        ]
    );
    for mut order in permutations(&operations) {
        let once = delivered(&order);
        assert_eq!(once, expected);
        order.extend(order.clone());
        assert_eq!(delivered(&order), expected);
        assert_eq!(replay(order, &StaticPolicy::default()).unwrap(), expected);
    }
    let successor = op(
        fixtures::LAPTOP,
        3,
        4,
        initial.id,
        Action::SetTitle("successor".into()),
    );
    let mut complete = operations;
    complete.push(successor.clone());
    let result = replay(complete, &StaticPolicy::default()).unwrap();
    assert_eq!(
        result.state.entry(initial.id),
        Some(&TodoEntry::Live {
            title: Register {
                value: "successor".into(),
                stamp: Stamp::from(&successor)
            },
            completed: Register {
                value: true,
                stamp: Stamp::from(&phone_completed)
            },
        })
    );
}
#[test]
fn delete_wins_every_delivery_order_even_against_greater_edit_stamps() {
    let initial = create(fixtures::LAPTOP, 1, 1, "initial");
    let title = op(
        fixtures::LAPTOP,
        2,
        2,
        initial.id,
        Action::SetTitle("Laptop".into()),
    );
    let delete = op(fixtures::LAPTOP, 3, 3, initial.id, Action::DeleteTodo);
    let remote = op(
        fixtures::PHONE,
        1,
        100,
        initial.id,
        Action::SetTitle("later stamp".into()),
    );
    let completion = op(
        fixtures::PHONE,
        2,
        101,
        initial.id,
        Action::SetCompleted(true),
    );
    let second = create(fixtures::PHONE, 3, 102, "survivor");
    let operations = vec![
        initial.clone(),
        title,
        delete,
        remote,
        completion,
        second.clone(),
    ];
    let expected = replay(operations.clone(), &StaticPolicy::default()).unwrap();
    assert_eq!(
        expected.state.entry(initial.id),
        Some(&TodoEntry::Tombstone)
    );
    assert_eq!(
        expected.state.visible_rows(),
        vec![VisibleRow {
            id: second.id,
            title: "survivor".into(),
            completed: false
        }]
    );
    for mut order in permutations(&operations) {
        order.push(initial.clone());
        order.extend(operations.clone());
        assert_eq!(delivered(&order), expected);
    }
}
#[test]
fn pending_duplicates_conflicts_and_replay_failures() {
    let initial = create(fixtures::LAPTOP, 1, 1, "initial");
    let setter = op(
        fixtures::PHONE,
        1,
        2,
        initial.id,
        Action::SetTitle("updated".into()),
    );
    let mut state = TodoState::default();
    let mut history = AppliedOperations::new(fixtures::SCOPE);
    assert_eq!(
        apply(&mut state, &mut history, &setter).unwrap(),
        Delivery::Pending(vec![initial.id])
    );
    assert_eq!(state, TodoState::default());
    assert_eq!(history.frontier(), AppliedFrontier::default());
    assert_eq!(
        replay([setter.clone()], &StaticPolicy::default()),
        Err(Error::UnresolvedDependencies(vec![setter.id]))
    );
    assert_eq!(
        apply(&mut state, &mut history, &initial).unwrap(),
        Delivery::Ready
    );
    let before = (state.clone(), history.clone());
    assert_eq!(
        apply(&mut state, &mut history, &initial).unwrap(),
        Delivery::Duplicate
    );
    assert_eq!((state.clone(), history.clone()), before);
    let mut conflict = initial.clone();
    conflict.payload = Action::CreateTodo {
        title: "changed".into(),
        completed: true,
    }
    .encode()
    .unwrap();
    assert_eq!(
        apply(&mut state, &mut history, &conflict),
        Err(Error::Protocol(ProtocolError::IdentityConflict(initial.id)))
    );
    assert_eq!((state.clone(), history.clone()), before);
    let mut pending_conflict = setter.clone();
    pending_conflict.payload = Action::SetTitle("conflict".into()).encode().unwrap();
    assert_eq!(
        replay([setter.clone(), pending_conflict], &StaticPolicy::default()),
        Err(Error::Protocol(ProtocolError::IdentityConflict(setter.id)))
    );
    let mut bad_clock = setter.clone();
    bad_clock.lamport = initial.lamport;
    assert_eq!(
        apply(&mut state, &mut history, &bad_clock),
        Err(Error::Protocol(ProtocolError::InvalidLamport {
            operation: setter.id,
            dependency: initial.id
        }))
    );
    assert_eq!((state.clone(), history.clone()), before);
    assert!(replay([initial, bad_clock], &StaticPolicy::default()).is_err());
    apply(&mut state, &mut history, &setter).unwrap();
    assert_eq!(state.visible_rows()[0].title, "updated");
}
#[test]
fn admission_denies_without_mutation_at_both_boundaries() {
    let initial = create(fixtures::LAPTOP, 1, 1, "initial");
    let mut cases = vec![];
    let default = StaticPolicy::default();
    let mut foreign = initial.clone();
    foreign.scope.mesh = MeshId([9; 16]);
    cases.push((foreign, default.clone(), Error::DeniedScope));
    let mut foreign = initial.clone();
    foreign.scope.app = AppId([9; 16]);
    cases.push((foreign, default.clone(), Error::DeniedScope));
    let mut foreign = initial.clone();
    foreign.scope.domain = DomainId([9; 16]);
    cases.push((foreign, default.clone(), Error::DeniedScope));
    let mut foreign = create(NodeId([9; 16]), 1, 1, "foreign");
    cases.push((foreign.clone(), default.clone(), Error::DeniedNode));
    foreign = initial.clone();
    foreign.caller = PrincipalId([9; 16]);
    cases.push((foreign, default.clone(), Error::DeniedCaller));
    for index in 0..4 {
        let action = [
            Action::CreateTodo {
                title: "new".into(),
                completed: false,
            },
            Action::SetTitle("new".into()),
            Action::SetCompleted(true),
            Action::DeleteTodo,
        ][index]
            .clone();
        let operation = if index == 0 {
            initial.clone()
        } else {
            op(fixtures::PHONE, 1, 2, initial.id, action.clone())
        };
        let mut policy = default.clone();
        policy.actions[index] = false;
        cases.push((operation, policy, Error::DeniedAction(action.kind())));
    }
    let mut policy = default.clone();
    policy.title = false;
    cases.push((initial.clone(), policy, Error::DeniedField(TITLE_FIELD_ID)));
    let mut policy = default.clone();
    policy.completed = false;
    cases.push((
        initial.clone(),
        policy,
        Error::DeniedField(COMPLETED_FIELD_ID),
    ));
    for (operation, policy, error) in cases {
        for boundary in [AdmissionBoundary::Local, AdmissionBoundary::Incoming] {
            let mut state = TodoState::default();
            let mut history = AppliedOperations::new(fixtures::SCOPE);
            let before = (state.clone(), history.clone());
            assert_eq!(
                TodoApp.apply_ready(&mut state, &mut history, &operation, &policy, boundary),
                Err(error.clone())
            );
            assert_eq!((state, history), before);
        }
    }
    assert!(default.admit_read(fixtures::SCOPE, fixtures::OWNER).is_ok());
    let mut policy = default;
    policy.read = false;
    assert_eq!(
        policy.admit_read(fixtures::SCOPE, fixtures::OWNER),
        Err(Error::DeniedRead)
    );
}
#[test]
fn local_tombstone_rejection_and_incoming_noop_history() {
    let initial = create(fixtures::LAPTOP, 1, 1, "initial");
    let delete = op(fixtures::LAPTOP, 2, 2, initial.id, Action::DeleteTodo);
    let mut result = replay([initial.clone(), delete], &StaticPolicy::default()).unwrap();
    let setter = op(
        fixtures::PHONE,
        1,
        3,
        initial.id,
        Action::SetCompleted(true),
    );
    let before = result.clone();
    assert_eq!(
        TodoApp.apply_ready(
            &mut result.state,
            &mut result.applied,
            &setter,
            &StaticPolicy::default(),
            AdmissionBoundary::Local
        ),
        Err(Error::Tombstoned(initial.id))
    );
    assert_eq!(result, before);
    assert_eq!(
        apply(&mut result.state, &mut result.applied, &setter).unwrap(),
        Delivery::Ready
    );
    assert_eq!(result.state, before.state);
    assert!(result.applied.canonical_bytes(setter.id).is_some());
}
#[test]
fn malformed_dependencies_versions_and_false_creates_do_not_mutate() {
    let initial = create(fixtures::LAPTOP, 1, 1, "initial");
    let mut wrong_entity = initial.clone();
    wrong_entity.entity = id(fixtures::PHONE, 1);
    let mut missing_dependency = op(fixtures::PHONE, 1, 2, initial.id, Action::DeleteTodo);
    missing_dependency.dependencies.clear();
    let mut unsupported = initial.clone();
    unsupported.versions.operation = 2;
    for operation in [wrong_entity, missing_dependency, unsupported] {
        for boundary in [AdmissionBoundary::Local, AdmissionBoundary::Incoming] {
            let mut state = TodoState::default();
            let mut history = AppliedOperations::new(fixtures::SCOPE);
            assert!(
                TodoApp
                    .apply_ready(
                        &mut state,
                        &mut history,
                        &operation,
                        &StaticPolicy::default(),
                        boundary
                    )
                    .is_err()
            );
            assert_eq!(state, TodoState::default());
            assert_eq!(history.frontier(), AppliedFrontier::default());
        }
    }
    let setter = op(
        fixtures::LAPTOP,
        2,
        2,
        initial.id,
        Action::SetTitle("updated".into()),
    );
    let invalid_target = op(fixtures::PHONE, 1, 3, setter.id, Action::DeleteTodo);
    assert_eq!(
        replay(
            [initial, setter.clone(), invalid_target],
            &StaticPolicy::default()
        ),
        Err(Error::MissingCreate(setter.id))
    );
    assert_eq!(DEFINITION.versions, Versions::V1);
    assert_eq!(DEFINITION.actions.len(), 4);
    assert_eq!(DEFINITION.table, TABLE_ID);
}

#[test]
fn buffered_dependency_clock_is_validated_before_readiness() {
    let missing_create = id(fixtures::LAPTOP, 1);
    let predecessor = op(
        fixtures::PHONE,
        1,
        5,
        missing_create,
        Action::SetTitle("pending".into()),
    );
    let invalid = op(
        fixtures::PHONE,
        2,
        4,
        missing_create,
        Action::SetCompleted(true),
    );
    assert_eq!(
        replay(
            [invalid.clone(), predecessor.clone()],
            &StaticPolicy::default()
        ),
        Err(Error::Protocol(ProtocolError::InvalidLamport {
            operation: invalid.id,
            dependency: predecessor.id
        }))
    );
}

#[test]
fn shared_application_contract_preserves_local_business_checks() {
    fn process<A: Application>(
        app: &A,
        state: &mut A::State,
        history: &mut AppliedOperations,
        operation: &Envelope,
        policy: &A::Policy,
        boundary: AdmissionBoundary,
    ) -> Result<Delivery, A::Error>
    where
        A::Error: From<ProtocolError>,
    {
        app.validate(operation)?;
        app.admit(operation, policy)?;
        let delivery = history.classify(operation)?;
        if delivery == Delivery::Ready {
            app.apply_ready(state, operation, boundary)?;
            history.record_applied(operation)?;
        }
        Ok(delivery)
    }
    let policy = StaticPolicy::default();
    let initial = create(fixtures::LAPTOP, 1, 1, "initial");
    let mut state = TodoState::default();
    let mut history = AppliedOperations::new(fixtures::SCOPE);
    assert_eq!(
        process(
            &TodoApp,
            &mut state,
            &mut history,
            &initial,
            &policy,
            AdmissionBoundary::Local
        )
        .unwrap(),
        Delivery::Ready
    );
    let delete = op(fixtures::LAPTOP, 2, 2, initial.id, Action::DeleteTodo);
    process(
        &TodoApp,
        &mut state,
        &mut history,
        &delete,
        &policy,
        AdmissionBoundary::Incoming,
    )
    .unwrap();
    let setter = op(
        fixtures::PHONE,
        1,
        3,
        initial.id,
        Action::SetTitle("new".into()),
    );
    let before = (state.clone(), history.clone());
    assert_eq!(
        process(
            &TodoApp,
            &mut state,
            &mut history,
            &setter,
            &policy,
            AdmissionBoundary::Local
        ),
        Err(Error::Tombstoned(initial.id))
    );
    assert_eq!((state.clone(), history.clone()), before);
    assert_eq!(
        process(
            &TodoApp,
            &mut state,
            &mut history,
            &setter,
            &policy,
            AdmissionBoundary::Incoming
        )
        .unwrap(),
        Delivery::Ready
    );
    assert_eq!(state, before.0);
}
