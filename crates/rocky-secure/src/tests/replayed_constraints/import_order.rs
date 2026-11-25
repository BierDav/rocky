use super::*;

async fn chains_of(c: &Constrained, imports: &[&'static str]) -> Vec<Vec<&'static str>> {
    let root = rocky_platform::fs::Scratch::new().await;
    let device = c.open(&root.path().join("d.db")).await;
    device
        .replica
        .in_dependency_chains(imports.to_vec(), |name| c.scope(name))
        .await
        .unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_partition_imports_a_child_scope_after_its_parents_and_an_unlinked_scope_apart() {
    assert_eq!(
        chains_of(&graph(), &["memberships", "notes", "people", "teams"]).await,
        vec![vec!["people", "teams", "memberships"], vec!["notes"]]
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn scopes_linked_by_a_key_import_parents_first_in_their_own_chain() {
    assert_eq!(
        chains_of(
            &Constrained::new(),
            &["lines", "members", "ranges", "orders", "teams"]
        )
        .await,
        vec![
            vec!["orders", "lines"],
            vec!["teams", "members"],
            vec!["ranges"]
        ]
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn scopes_referencing_each_other_import_in_one_chain() {
    let pointing = |name: &str, other: &str| {
        replayed(
            name,
            serde_json::json!([{"name": "other", "type": "blob"}]),
            serde_json::json!({"foreign_keys": [
                {"columns": ["other"], "table": other, "on_delete": "set_null", "deferred": true}
            ]}),
        )
    };
    let c = Constrained::with(
        serde_json::json!({
            "format": 7,
            "application": "replayed-cycle",
            "tables": [
                pointing("eggs", "hens"),
                pointing("hens", "eggs"),
                replayed("barns", serde_json::json!([{"name": "name", "type": "text"}]), serde_json::json!({}))
            ],
        }),
        |_| {},
    );
    assert_eq!(
        chains_of(&c, &["hens", "barns", "eggs"]).await,
        vec![vec!["hens", "eggs"], vec!["barns"]]
    );
}
