//! Qualifies the scalar and key contracts of `rocky-core` against the pinned
//! SQLite: getter conversions, storage classes and key equality.
use rocky_core::{Affinity, Collation, KeyColumn, SqlValue, key_components};
use rusqlite::{Connection, types::Value};

fn engine(value: &SqlValue) -> Value {
    match value {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(value) => Value::Integer(*value),
        SqlValue::Real(value) => Value::Real(value.get()),
        SqlValue::Text(value) => Value::Text(value.clone()),
        SqlValue::Blob(value) => Value::Blob(value.clone()),
    }
}
fn samples() -> Vec<SqlValue> {
    let reals = [
        0.0,
        1.0,
        -1.5,
        0.1,
        0.30000000000000004,
        1.0 / 3.0,
        2.0 / 3.0,
        4.35,
        123_456_789.123_456_79,
        9.999999999999998e16,
        1e15,
        1e16,
        1e17,
        1e21,
        0.0001,
        0.00012345678901234567,
        1e-5,
        2.5e-7,
        123456789012345.0,
        1234567890123456.0,
        9007199254740993.0,
        -3.999,
        1e300,
        -1e300,
        5e-324,
        f64::MAX,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ];
    let texts = [
        "12abc",
        " -7.5e2x",
        "abc",
        "",
        "1e3",
        "9223372036854775807",
        "9223372036854775808",
        "-9223372036854775809",
        ".5",
        "5.",
        "0x10",
        "+3",
        " \t42",
        "1e",
        "-",
        "3.25",
    ];
    let mut values = vec![SqlValue::Null];
    values.extend([0, -1, 42, i64::MIN, i64::MAX].map(SqlValue::Integer));
    values.extend(reals.iter().filter_map(|real| SqlValue::real(*real)));
    values.extend(texts.iter().map(|text| SqlValue::Text((*text).into())));
    values.extend([&b"42"[..], b"x", b"-1.5e1", b""].map(|blob| SqlValue::Blob(blob.to_vec())));
    values
}

#[test]
fn getter_conversions_match_the_engine() {
    let conn = Connection::open_in_memory().expect("engine");
    for value in samples() {
        let cast = |target: &str| -> Value {
            conn.query_row(
                &format!("SELECT CAST(?1 AS {target})"),
                [engine(&value)],
                |row| row.get(0),
            )
            .expect("cast")
        };
        let integer = match &value {
            SqlValue::Null => Value::Null,
            value => Value::Integer(value.to_integer()),
        };
        assert_eq!(cast("INTEGER"), integer, "{value:?} as INTEGER");
        let real = match &value {
            SqlValue::Null => Value::Null,
            value => Value::Real(value.to_real()),
        };
        assert_eq!(cast("REAL"), real, "{value:?} as REAL");
        // Fifteen digits identify most doubles, and then the text is the
        // engine's. Otherwise, and for subnormals, both texts read back as
        // the same double; the engine's last digit is not always the nearest.
        let exact = match &value {
            SqlValue::Real(real) => {
                let real = real.get();
                !real.is_subnormal() && format!("{real:.14e}").parse() == Ok(real)
            }
            _ => true,
        };
        let text = value.to_text().map_or(Value::Null, Value::Text);
        let blob = value.to_blob().map_or(Value::Null, Value::Blob);
        if exact {
            assert_eq!(cast("TEXT"), text, "{value:?} as TEXT");
            assert_eq!(cast("BLOB"), blob, "{value:?} as BLOB");
        } else {
            let (Value::Text(engine), Value::Text(ours)) = (cast("TEXT"), text) else {
                panic!("{value:?} as TEXT");
            };
            assert_eq!(
                engine.parse::<f64>(),
                ours.parse::<f64>(),
                "{engine} {ours}"
            );
            assert_eq!(ours.parse::<f64>().expect("number"), value.to_real());
        }
    }
}

#[test]
fn columns_store_the_class_their_affinity_names() {
    let conn = Connection::open_in_memory().expect("engine");
    conn.execute_batch("CREATE TABLE t(i INTEGER,r REAL,x TEXT,b BLOB)")
        .expect("table");
    // Every conversion the strict codec accepts is the engine's own.
    for (column, affinity, value) in [
        ("i", Affinity::Integer, SqlValue::real(7.0).expect("real")),
        ("i", Affinity::Integer, SqlValue::Integer(i64::MIN)),
        ("r", Affinity::Real, SqlValue::Integer(1 << 53)),
        ("r", Affinity::Real, SqlValue::real(-2.5).expect("real")),
        ("x", Affinity::Text, SqlValue::Text("ü".into())),
        ("b", Affinity::Blob, SqlValue::Blob(vec![0])),
    ] {
        let stored = affinity.store(value.clone()).expect("stored");
        conn.execute("DELETE FROM t", []).expect("clear");
        conn.execute(
            &format!("INSERT INTO t({column}) VALUES(?1)"),
            [engine(&value)],
        )
        .expect("insert");
        let read: Value = conn
            .query_row(&format!("SELECT {column} FROM t"), [], |row| row.get(0))
            .expect("read");
        assert_eq!(read, engine(&stored), "{value:?} in {column}");
    }
    // The engine reads negative zero back as zero from a REAL column.
    conn.execute("DELETE FROM t", []).expect("clear");
    conn.execute("INSERT INTO t(r) VALUES(?1)", [-0.0f64])
        .expect("insert");
    let zero: f64 = conn
        .query_row("SELECT r FROM t", [], |row| row.get(0))
        .expect("read");
    assert_eq!(
        zero.to_bits(),
        SqlValue::real(-0.0).expect("real").to_real().to_bits()
    );
    // NaN is not a REAL: the engine stores NULL.
    conn.execute("DELETE FROM t", []).expect("clear");
    conn.execute("INSERT INTO t(r) VALUES(?1)", [f64::NAN])
        .expect("insert");
    let class: String = conn
        .query_row("SELECT typeof(r) FROM t", [], |row| row.get(0))
        .expect("read");
    assert_eq!(class, "null");
}

#[test]
fn key_equality_is_the_engines_under_each_collation() {
    let conn = Connection::open_in_memory().expect("engine");
    let texts = [
        "a", "A", "a ", "a  ", " a", "É", "é", "e\u{301}", "\u{e9}", "", " ", "ß", "SS", "a\0b",
        "a\0B", "Z", "z",
    ];
    for (name, collation) in [
        ("BINARY", Collation::Binary),
        ("NOCASE", Collation::Nocase),
        ("RTRIM", Collation::Rtrim),
    ] {
        let column = [KeyColumn {
            affinity: Affinity::Text,
            collation,
        }];
        let key =
            |text: &str| key_components(&column, &[SqlValue::Text(text.into())]).expect("key");
        for left in texts {
            for right in texts {
                let equal: bool = conn
                    .query_row(
                        &format!("SELECT ?1 = ?2 COLLATE {name}"),
                        [left, right],
                        |row| row.get(0),
                    )
                    .expect("compare");
                assert_eq!(key(left) == key(right), equal, "{left:?} {right:?} {name}");
            }
        }
    }
    // Compound keys with every key class: a primary key conflicts exactly
    // when the encodings are equal.
    conn.execute_batch(
        "CREATE TABLE k(t TEXT NOT NULL COLLATE NOCASE,i INTEGER NOT NULL,r REAL NOT NULL,b BLOB NOT NULL,PRIMARY KEY(t,i,r,b))",
    )
    .expect("table");
    let columns = [
        (Affinity::Text, Collation::Nocase),
        (Affinity::Integer, Collation::Binary),
        (Affinity::Real, Collation::Binary),
        (Affinity::Blob, Collation::Binary),
    ]
    .map(|(affinity, collation)| KeyColumn {
        affinity,
        collation,
    });
    let real = |value: f64| SqlValue::real(value).expect("real");
    let row = |t: &str, i: SqlValue, r: SqlValue, b: &[u8]| {
        vec![SqlValue::Text(t.into()), i, r, SqlValue::Blob(b.to_vec())]
    };
    let rows = [
        row("Ab", SqlValue::Integer(1), real(0.5), b"x"),
        row("aB", real(1.0), real(0.5), b"x"),
        row("ab", SqlValue::Integer(1), real(0.5), b"X"),
        row("ab", SqlValue::Integer(i64::MAX), real(0.5), b"x"),
        row("ab", SqlValue::Integer(i64::MAX - 1), real(0.5), b"x"),
        row("ab", SqlValue::Integer(i64::MIN), SqlValue::Integer(2), b""),
        row("ab", SqlValue::Integer(i64::MIN), real(2.0), b""),
        row("ab", SqlValue::Integer(i64::MIN), real(-0.0), b""),
        row("ab", SqlValue::Integer(i64::MIN), SqlValue::Integer(0), b""),
        row("a", SqlValue::Integer(0), real(f64::INFINITY), b"b"),
        row("a", SqlValue::Integer(0), real(f64::MAX), b"b"),
    ];
    let mut known: Vec<Vec<u8>> = Vec::new();
    for values in &rows {
        let key = key_components(&columns, values).expect("key");
        let inserted = conn
            .execute(
                "INSERT INTO k VALUES(?1,?2,?3,?4)",
                rusqlite::params_from_iter(values.iter().map(engine)),
            )
            .is_ok();
        assert_eq!(inserted, !known.contains(&key), "{values:?}");
        if inserted {
            known.push(key);
        }
    }
    assert_eq!(known.len(), 8);
}
