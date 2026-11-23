use super::*;

pub(super) struct History {
    pub(super) c: Constrained,
    pub(super) setup: Vec<EncryptedFrame>,
    pub(super) units: Vec<Vec<EncryptedFrame>>,
}

impl History {
    pub(super) fn frames(&self) -> Vec<EncryptedFrame> {
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

fn schedules(units: &[Vec<EncryptedFrame>], splits: bool) -> Vec<Vec<Vec<EncryptedFrame>>> {
    let every = 1usize << (units.len() - 1);
    let mut all = Vec::new();
    for (index, order) in permutations(units.len()).into_iter().enumerate() {
        let cut = if splits { index % every } else { every - 1 };
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
    all
}

pub(super) fn deliver_all(history: &History, splits: bool, expected: &Canonical) {
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
                        rocky_platform::fs::remove_file(&path).ok();
                    }
                })
            });
        }
    });
}

pub(super) fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

pub(super) async fn unit(
    c: &Constrained,
    device: &mut Device,
    statements: &[&str],
) -> Vec<EncryptedFrame> {
    device.commit(c, statements).await.unwrap();
    device.fresh().await
}

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
            match &session.run(sql, vec![]).await.unwrap().rows[0][0] {
                SqlValue::Blob(id) => super::Id(rocky_core::RowIdentity::from_bytes(id).unwrap()),
                other => panic!("{other:?}"),
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
    let nickname_clashing_with_the_first_ignoring_case = unit(
        &c,
        &mut d[2],
        &["INSERT INTO people(tenant,nickname,id) VALUES(1,'BETA',#id)"],
    )
    .await;
    units.push(nickname_clashing_with_the_first_ignoring_case);
    let second_active_owner_of_twenty_with_a_note_rolling_back = unit(
        &c,
        &mut d[3],
        &[
            "INSERT INTO notes(tenant,message,id) VALUES(1,'must roll back',#id)",
            &format!(
                "INSERT INTO memberships(tenant,team,person,role,active,seats,id) VALUES(1,{twenty},{alpha},'owner',1,1,#id)"
            ),
        ],
    )
    .await;
    units.push(second_active_owner_of_twenty_with_a_note_rolling_back);
    units.push(
        unit(
            &c,
            &mut d[2],
            &[&format!("DELETE FROM teams WHERE tenant=1 AND id={ten}")],
        )
        .await,
    );
    let membership_of_ten_replayed_where_ten_is_gone = unit(
        &c,
        &mut d[1],
        &[&format!(
            "INSERT INTO memberships(tenant,team,person,role,active,seats,id) VALUES(1,{ten},{alpha},'member',1,3,#id)"
        )],
    )
    .await;
    units.push(membership_of_ten_replayed_where_ten_is_gone);
    History { c, setup, units }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn composite_many_to_many_graph_converges_in_all_720_orders() {
    let root = tempfile::tempdir().unwrap();
    let history = runtime().block_on(graph_history(root.path()));
    let expected = canonical(&history.c, &history.frames());
    assert_eq!(expected.dropped(), 3, "{expected:?}");
    assert!(expected.rows["notes"].is_empty());
    assert_eq!(expected.rows["memberships"].len(), 1);
    deliver_all(&history, false, &expected);
}

fn checked_orders() -> Constrained {
    let integer =
        |name: &str| serde_json::json!({"name": name, "type": "integer", "not_null": true});
    let definition = serde_json::json!({
        "format": 7,
        "application": "replayed-orders",
        "tables": [
            replayed(
                "orders",
                serde_json::json!([
                    {"name": "status", "type": "text", "not_null": true},
                    integer("quantity"),
                    integer("price"),
                    integer("discount"),
                    {"name": "net", "type": "integer", "derived": "checked_mul(quantity, checked_sub(price, discount))"},
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
                serde_json::json!([{"name": "parent", "type": "blob", "not_null": true}, integer("amount")]),
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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn complex_checks_converge_in_all_120_orders() {
    let root = tempfile::tempdir().unwrap();
    let history = runtime().block_on(checks_history(root.path()));
    let expected = canonical(&history.c, &history.frames());
    assert_eq!(expected.dropped(), 3, "{expected:?}");
    assert_eq!(expected.rows["lines"].len(), 1);
    assert_eq!(
        expected.rows["orders"][0][4],
        SqlValue::Integer(0),
        "the generated net of quantity 100, price 10 and discount 10 is computed"
    );
    deliver_all(&history, false, &expected);
}

fn indexed_seats() -> Constrained {
    let definition = serde_json::json!({
        "format": 7,
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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn expression_and_partial_unique_indexes_converge_in_every_order() {
    let root = tempfile::tempdir().unwrap();
    let history = runtime().block_on(indexes_history(root.path()));
    let expected = canonical(&history.c, &history.frames());
    assert_eq!(expected.dropped(), 1, "{expected:?}");
    assert_eq!(expected.rows["seats"].len(), 3);
    deliver_all(&history, true, &expected);
}

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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn cascades_roll_back_with_their_transaction_in_every_order() {
    let root = tempfile::tempdir().unwrap();
    let history = runtime().block_on(cascades_history(root.path()));
    let expected = canonical(&history.c, &history.frames());
    assert_eq!(expected.dropped(), 1, "{expected:?}");
    assert_eq!(expected.rows["teams"].len(), 2);
    assert_eq!(expected.rows["members"].len(), 2);
    deliver_all(&history, true, &expected);
}

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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn assertion_caps_converge_in_every_order() {
    let root = tempfile::tempdir().unwrap();
    let history = runtime().block_on(caps_history(root.path()));
    let expected = canonical(&history.c, &history.frames());
    assert_eq!(expected.dropped(), 1, "{expected:?}");
    assert_eq!(expected.rows["members"].len(), 1);
    deliver_all(&history, true, &expected);
}
