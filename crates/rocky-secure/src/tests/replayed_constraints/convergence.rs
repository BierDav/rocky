//! The constraint fixtures carried from the prototype. Every fixture is a
//! setup every receiver starts from and units of concurrent history, and
//! every delivery schedule of them equals the from-scratch canonical replay.
use super::*;

/// What every schedule delivers; phase 6's equivalence runs over the same.
struct History {
    c: Constrained,
    setup: Vec<EncryptedFrame>,
    units: Vec<Vec<EncryptedFrame>>,
}

impl History {
    fn frames(&self) -> Vec<EncryptedFrame> {
        let units = self.units.iter().flatten();
        self.setup.iter().chain(units).cloned().collect()
    }
}

fn permutations(n: usize) -> Vec<Vec<usize>> {
    if n == 0 {
        return vec![Vec::new()];
    }
    let mut all = Vec::new();
    for rest in permutations(n - 1) {
        for at in 0..=rest.len() {
            let mut order = rest.clone();
            order.insert(at, n - 1);
            all.push(order);
        }
    }
    all
}

/// Every order of `units`, one packet each; with `splits`, also every split
/// of each order into consecutive packets.
fn schedules(units: &[Vec<EncryptedFrame>], splits: bool) -> Vec<Vec<Vec<EncryptedFrame>>> {
    let cuts = 1usize << (units.len() - 1);
    let mut all = Vec::new();
    for order in permutations(units.len()) {
        for cut in if splits { 0..cuts } else { cuts - 1..cuts } {
            let mut packets = vec![Vec::new()];
            for (position, unit) in order.iter().enumerate() {
                if position > 0 && cut & (1 << (position - 1)) != 0 {
                    packets.push(Vec::new());
                }
                packets
                    .last_mut()
                    .unwrap()
                    .extend(units[*unit].iter().cloned());
            }
            all.push(packets);
        }
    }
    all
}

/// Runs every schedule on a fresh receiver, spread over threads.
fn deliver_all(history: &History, splits: bool, expected: &Canonical) {
    let schedules = schedules(&history.units, splits);
    let root = tempfile::tempdir().unwrap();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    std::thread::scope(|scope| {
        for thread in 0..threads {
            let (root, schedules) = (root.path(), &schedules);
            scope.spawn(move || {
                runtime().block_on(async {
                    for (index, packets) in
                        schedules.iter().enumerate().skip(thread).step_by(threads)
                    {
                        let path = root.join(format!("r{index}.db"));
                        let receiver = history.c.open(&path).await;
                        receiver.receive(&history.setup).await;
                        for packet in packets {
                            receiver.receive(packet).await;
                        }
                        let state = receiver.state(&history.c, expected).await;
                        assert_eq!(&state, expected, "schedule {index}");
                        drop(receiver);
                        std::fs::remove_file(&path).ok();
                    }
                })
            });
        }
    });
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn replayed(name: &str, columns: serde_json::Value, rest: serde_json::Value) -> serde_json::Value {
    let mut table = serde_json::json!({
        "name": name, "class": "replayed", "columns": columns, "primary_key": ["id"],
    });
    table
        .as_object_mut()
        .unwrap()
        .extend(rest.as_object().unwrap().clone());
    table
}

/// Teams and people of one tenant, joined by `memberships` through composite
/// deferred keys. A person's nickname is unique ignoring case and a team has
/// one active owner.
fn graph() -> Constrained {
    let tenant = serde_json::json!({"name": "tenant", "type": "integer", "not_null": true});
    let partitioned = |extra: serde_json::Value| {
        let mut rest = serde_json::json!({
            "primary_key": ["tenant", "id"], "partition_by": ["tenant"], "partition_group": "tenants",
        });
        rest.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        rest
    };
    let definition = serde_json::json!({
        "format": 6,
        "application": "replayed-graph",
        "tables": [
            replayed("teams", serde_json::json!([tenant, {"name": "name", "type": "text", "not_null": true}]), partitioned(serde_json::json!({}))),
            replayed(
                "people",
                serde_json::json!([tenant, {"name": "nickname", "type": "text", "not_null": true}]),
                partitioned(serde_json::json!({"indexes": [
                    {"name": "unique_nickname", "unique": true,
                     "columns": [{"name": "tenant", "descending": false}],
                     "expressions": ["lower(nickname)"]}
                ]})),
            ),
            replayed("notes", serde_json::json!([tenant, {"name": "message", "type": "text", "not_null": true}]), partitioned(serde_json::json!({}))),
            replayed(
                "memberships",
                serde_json::json!([
                    tenant,
                    {"name": "team", "type": "integer", "not_null": true},
                    {"name": "person", "type": "integer", "not_null": true},
                    {"name": "role", "type": "text", "not_null": true},
                    {"name": "active", "type": "integer", "not_null": true},
                    {"name": "seats", "type": "integer", "not_null": true}
                ]),
                partitioned(serde_json::json!({
                    "checks": [
                        "role IN ('owner', 'member')",
                        "active IN (0, 1)",
                        "seats BETWEEN 1 AND 10",
                        "CASE WHEN role = 'owner' THEN active = 1 AND seats <= 2 ELSE 1 END"
                    ],
                    "indexes": [{"name": "one_membership", "unique": true, "columns": [
                        {"name": "tenant", "descending": false},
                        {"name": "team", "descending": false},
                        {"name": "person", "descending": false}
                    ]}, {"name": "one_active_owner", "unique": true, "columns": [
                        {"name": "tenant", "descending": false},
                        {"name": "team", "descending": false}
                    ], "where": "role = 'owner' AND active = 1"}],
                    "foreign_keys": [
                        {"columns": ["tenant", "team"], "table": "teams", "on_delete": "cascade", "deferred": true},
                        {"columns": ["tenant", "person"], "table": "people", "on_delete": "cascade", "deferred": true}
                    ]
                })),
            ),
        ],
    });
    Constrained::build(definition, Some(1), |_| {})
}

/// Commits `statements` on `device`, returning the frames as one unit.
async fn unit(c: &Constrained, device: &mut Device, statements: &[&str]) -> Vec<EncryptedFrame> {
    device.commit(c, statements).await.unwrap();
    device.fresh().await
}

/// Six transactions of four authors. The owner's membership needs the team
/// and person of the first, so it arrives before them in half the orders.
/// Three lose where they replay, and the note beside the fourth goes with it.
async fn graph_history(root: &Path) -> History {
    let c = graph();
    let (mut d, ids, setup) = devices(
        &c,
        root,
        4,
        &[
            "INSERT INTO teams(tenant,name,id) VALUES(1,'ten',#id)",
            "INSERT INTO people(tenant,nickname,id) VALUES(1,'alpha',#id)",
        ],
    )
    .await;
    let (ten, alpha) = (ids[0], ids[1]);
    let mut units = Vec::new();
    let first = unit(
        &c,
        &mut d[0],
        &[
            "INSERT INTO teams(tenant,name,id) VALUES(1,'twenty',#id)",
            "INSERT INTO people(tenant,nickname,id) VALUES(1,'beta',#id)",
        ],
    )
    .await;
    d[1].receive(&first).await;
    d[3].receive(&first).await;
    let session = d[0].session(&c).await;
    let row = |sql: &'static str| {
        let session = &session;
        async move {
            match session.run(sql, vec![]).await.unwrap().rows[0][0] {
                SqlValue::Integer(id) => id,
                ref other => panic!("{other:?}"),
            }
        }
    };
    let twenty = row("SELECT id FROM teams WHERE tenant=1 AND name='twenty'").await;
    let beta = row("SELECT id FROM people WHERE tenant=1 AND nickname='beta'").await;
    drop(session);
    units.push(first);
    units.push(
        unit(
            &c,
            &mut d[1],
            &[&format!(
                "INSERT INTO memberships(tenant,team,person,role,active,seats,id) VALUES(1,{twenty},{beta},'owner',1,2,#id)"
            )],
        )
        .await,
    );
    // Its nickname clashes with the first's ignoring case.
    units.push(
        unit(
            &c,
            &mut d[2],
            &["INSERT INTO people(tenant,nickname,id) VALUES(1,'BETA',#id)"],
        )
        .await,
    );
    // A second active owner of twenty: its note rolls back with it.
    units.push(
        unit(
            &c,
            &mut d[3],
            &[
                "INSERT INTO notes(tenant,message,id) VALUES(1,'must roll back',#id)",
                &format!(
                    "INSERT INTO memberships(tenant,team,person,role,active,seats,id) VALUES(1,{twenty},{alpha},'owner',1,1,#id)"
                ),
            ],
        )
        .await,
    );
    units.push(
        unit(
            &c,
            &mut d[2],
            &[&format!("DELETE FROM teams WHERE tenant=1 AND id={ten}")],
        )
        .await,
    );
    // Ten is gone where this replays.
    units.push(
        unit(
            &c,
            &mut d[1],
            &[&format!(
                "INSERT INTO memberships(tenant,team,person,role,active,seats,id) VALUES(1,{ten},{alpha},'member',1,3,#id)"
            )],
        )
        .await,
    );
    History { c, setup, units }
}

#[test]
fn composite_many_to_many_graph_converges_in_all_720_orders() {
    let root = tempfile::tempdir().unwrap();
    let history = runtime().block_on(graph_history(root.path()));
    let expected = canonical(&history.c, &history.frames());
    assert_eq!(expected.dropped(), 3, "{expected:?}");
    assert!(expected.rows["notes"].is_empty());
    assert_eq!(expected.rows["memberships"].len(), 1);
    deliver_all(&history, false, &expected);
}

/// An order's values are checked together: its discount within its price,
/// its generated net bounded and a posted order's reference. `line_cap` bounds its
/// lines, as the prototype's trigger did, for updates and deletes too.
fn checked_orders() -> Constrained {
    let integer =
        |name: &str| serde_json::json!({"name": name, "type": "integer", "not_null": true});
    let definition = serde_json::json!({
        "format": 6,
        "application": "replayed-orders",
        "tables": [
            replayed(
                "orders",
                serde_json::json!([
                    {"name": "status", "type": "text", "not_null": true},
                    integer("quantity"),
                    integer("price"),
                    integer("discount"),
                    {"name": "net", "type": "integer", "derived": "quantity * (price - discount)"},
                    {"name": "reference", "type": "text"},
                    {"name": "optional_amount", "type": "integer"}
                ]),
                serde_json::json!({"checks": [
                    "status IN ('draft', 'posted', 'void')",
                    "quantity BETWEEN 1 AND 100",
                    "price >= 0",
                    "discount BETWEEN 0 AND price",
                    "net <= 1000",
                    "optional_amount >= 0",
                    "CASE WHEN status = 'posted' THEN reference IS NOT NULL AND length(trim(reference)) >= 3 ELSE 1 END"
                ]}),
            ),
            replayed(
                "lines",
                serde_json::json!([integer("parent"), integer("amount")]),
                serde_json::json!({
                    "checks": ["amount > 0"],
                    "foreign_keys": [{"columns": ["parent"], "table": "orders", "on_delete": "cascade"}]
                }),
            ),
        ],
        "assertions": [{"name": "line_cap",
            "select_sql": "SELECT l.parent FROM lines l GROUP BY l.parent HAVING SUM(l.amount) > 100"}],
    });
    Constrained::with(definition, |_| {})
}

/// Five authors change one order, each seeing only the setup. Every
/// transaction holds where it was written; the discount makes a lower price
/// fail, the quantity and price make the third's net too large, and the
/// fifth's line passes the cap, taking its reference with it.
async fn checks_history(root: &Path) -> History {
    let c = checked_orders();
    let (mut d, ids, setup) = devices(
        &c,
        root,
        5,
        &["INSERT INTO orders(status,quantity,price,discount,id) VALUES('draft',1,10,0,#id)"],
    )
    .await;
    let order = ids[0];
    let mut units = Vec::new();
    for (index, statements) in [
        vec![format!("UPDATE orders SET discount=10 WHERE id={order}")],
        vec![format!("UPDATE orders SET price=5 WHERE id={order}")],
        vec![
            format!("UPDATE orders SET quantity=100, price=10 WHERE id={order}"),
            format!("INSERT INTO lines(parent,amount,id) VALUES({order},60,#id)"),
        ],
        vec![format!("UPDATE orders SET price=30 WHERE id={order}")],
        vec![
            format!("UPDATE orders SET reference='must roll back' WHERE id={order}"),
            format!("INSERT INTO lines(parent,amount,id) VALUES({order},50,#id)"),
        ],
    ]
    .into_iter()
    .enumerate()
    {
        let statements: Vec<&str> = statements.iter().map(String::as_str).collect();
        units.push(unit(&c, &mut d[index], &statements).await);
    }
    History { c, setup, units }
}

#[test]
fn complex_checks_converge_in_all_120_orders() {
    let root = tempfile::tempdir().unwrap();
    let history = runtime().block_on(checks_history(root.path()));
    let expected = canonical(&history.c, &history.frames());
    assert_eq!(expected.dropped(), 3, "{expected:?}");
    assert_eq!(expected.rows["lines"].len(), 1);
    // quantity 100, price 10, discount 10: the generated net is computed.
    assert_eq!(expected.rows["orders"][0][4], SqlValue::Integer(0));
    deliver_all(&history, false, &expected);
}

/// `handle` is unique ignoring case and a team has one active lead. The
/// third frees the name and the lead seat the first two contend for.
fn indexed_seats() -> Constrained {
    let definition = serde_json::json!({
        "format": 6,
        "application": "replayed-seats",
        "tables": [replayed(
            "seats",
            serde_json::json!([
                {"name": "handle", "type": "text", "not_null": true},
                {"name": "team", "type": "integer", "not_null": true},
                {"name": "lead", "type": "integer", "not_null": true}
            ]),
            serde_json::json!({"indexes": [
                {"name": "unique_handle", "unique": true, "columns": [],
                 "expressions": ["lower(handle)"]},
                {"name": "one_lead", "unique": true,
                 "columns": [{"name": "team", "descending": false}], "where": "lead = 1"}
            ]}),
        )],
    });
    Constrained::with(definition, |_| {})
}

/// Ada leads team 1. A concurrent `ADA` clashes with her whenever it
/// replays; `ada`, ordered after she renames and steps down, takes both her
/// name and her seat, while a spare seat of the team never clashes.
async fn indexes_history(root: &Path) -> History {
    let c = indexed_seats();
    let (mut d, _, setup) = devices(&c, root, 3, &[]).await;
    let mut units = Vec::new();
    let insert = |handle: &str, team: i64, lead: i64| {
        format!("INSERT INTO seats(handle,team,lead,id) VALUES('{handle}',{team},{lead},#id)")
    };
    units.push(unit(&c, &mut d[0], &[&insert("Ada", 1, 1)]).await);
    units.push(unit(&c, &mut d[1], &[&insert("ADA", 2, 1)]).await);
    units.push(
        unit(
            &c,
            &mut d[0],
            &[
                "UPDATE seats SET lead=0, handle='old' WHERE handle='Ada'",
                &insert("spare", 1, 0),
            ],
        )
        .await,
    );
    units.push(unit(&c, &mut d[2], &[&insert("ada", 1, 1)]).await);
    History { c, setup, units }
}

#[test]
fn expression_and_partial_unique_indexes_converge_in_every_schedule() {
    let root = tempfile::tempdir().unwrap();
    let history = runtime().block_on(indexes_history(root.path()));
    let expected = canonical(&history.c, &history.frames());
    assert_eq!(expected.dropped(), 1, "{expected:?}");
    assert_eq!(expected.rows["seats"].len(), 3);
    deliver_all(&history, true, &expected);
}

/// A delete that cascades drops with the raise beside it, so green and its
/// member stay and the later member of green applies.
async fn cascades_history(root: &Path) -> History {
    let c = Constrained::new();
    let (mut d, ids, setup) = devices(
        &c,
        root,
        4,
        &[
            "INSERT INTO teams(name,cap,id) VALUES('red',2,#id)",
            "INSERT INTO teams(name,cap,id) VALUES('green',3,#id)",
            "INSERT INTO teams(name,cap,id) VALUES('blue',3,#id)",
        ],
    )
    .await;
    let (red, green, blue) = (ids[0], ids[1], ids[2]);
    let added = unit(
        &c,
        &mut d[3],
        &[&format!(
            "INSERT INTO members(team,email,id) VALUES({green},'g@example',#id)"
        )],
    )
    .await;
    for device in &d[..3] {
        device.receive(&added).await;
    }
    let setup = setup.into_iter().chain(added).collect();
    let mut units = Vec::new();
    units.push(
        unit(
            &c,
            &mut d[1],
            &[&format!("UPDATE teams SET cap=cap+1 WHERE id={red}")],
        )
        .await,
    );
    units.push(
        unit(
            &c,
            &mut d[0],
            &[
                &format!("DELETE FROM teams WHERE id={green}"),
                &format!("UPDATE teams SET cap=cap+1 WHERE id={red}"),
            ],
        )
        .await,
    );
    units.push(
        unit(
            &c,
            &mut d[2],
            &[&format!(
                "INSERT INTO members(team,email,id) VALUES({green},'h@example',#id)"
            )],
        )
        .await,
    );
    units.push(
        unit(
            &c,
            &mut d[1],
            &[&format!("DELETE FROM teams WHERE id={blue}")],
        )
        .await,
    );
    History { c, setup, units }
}

#[test]
fn cascades_roll_back_with_their_transaction_in_every_schedule() {
    let root = tempfile::tempdir().unwrap();
    let history = runtime().block_on(cascades_history(root.path()));
    let expected = canonical(&history.c, &history.frames());
    assert_eq!(expected.dropped(), 1, "{expected:?}");
    assert_eq!(expected.rows["teams"].len(), 2);
    assert_eq!(expected.rows["members"].len(), 2);
    deliver_all(&history, true, &expected);
}

/// Red holds one member. The first author adds and removes one; the second
/// and third each add one: the second fits only after the removal, and the
/// third never does.
async fn caps_history(root: &Path) -> History {
    let c = Constrained::new();
    let (mut d, ids, setup) = devices(
        &c,
        root,
        3,
        &["INSERT INTO teams(name,cap,id) VALUES('red',1,#id)"],
    )
    .await;
    let red = ids[0];
    let add =
        |email: &str| format!("INSERT INTO members(team,email,id) VALUES({red},'{email}',#id)");
    let units = vec![
        unit(&c, &mut d[0], &[&add("one@example")]).await,
        unit(
            &c,
            &mut d[0],
            &["DELETE FROM members WHERE email='one@example'"],
        )
        .await,
        unit(&c, &mut d[1], &[&add("two@example")]).await,
        unit(&c, &mut d[2], &[&add("three@example")]).await,
    ];
    History { c, setup, units }
}

#[test]
fn assertion_caps_converge_in_every_schedule() {
    let root = tempfile::tempdir().unwrap();
    let history = runtime().block_on(caps_history(root.path()));
    let expected = canonical(&history.c, &history.frames());
    assert_eq!(expected.dropped(), 1, "{expected:?}");
    assert_eq!(expected.rows["members"].len(), 1);
    deliver_all(&history, true, &expected);
}
