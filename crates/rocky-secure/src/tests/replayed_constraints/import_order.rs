use super::*;
use crate::DependencyNode;

async fn graph_of(c: &Constrained, imports: &[&'static str]) -> Vec<DependencyNode<&'static str>> {
    let root = rocky_platform::fs::Scratch::new().await;
    let device = c.open(&root.path().join("d.db")).await;
    device
        .replica
        .dependency_graph(imports.to_vec(), |name| c.scope(name))
        .await
        .unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_partition_imports_a_child_scope_after_its_parents_and_an_unlinked_scope_apart() {
    assert_eq!(
        graph_of(&graph(), &["memberships", "notes", "people", "teams"]).await,
        vec![
            DependencyNode {
                items: vec!["notes"],
                parents: vec![],
            },
            DependencyNode {
                items: vec!["people"],
                parents: vec![],
            },
            DependencyNode {
                items: vec!["teams"],
                parents: vec![],
            },
            DependencyNode {
                items: vec!["memberships"],
                parents: vec![1, 2],
            },
        ]
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn scopes_linked_by_a_key_import_parents_first_in_their_own_nodes() {
    assert_eq!(
        graph_of(
            &Constrained::new(),
            &["lines", "members", "ranges", "orders", "teams"]
        )
        .await,
        vec![
            DependencyNode {
                items: vec!["ranges"],
                parents: vec![],
            },
            DependencyNode {
                items: vec!["orders"],
                parents: vec![],
            },
            DependencyNode {
                items: vec!["lines"],
                parents: vec![1],
            },
            DependencyNode {
                items: vec!["teams"],
                parents: vec![],
            },
            DependencyNode {
                items: vec!["members"],
                parents: vec![3],
            },
        ]
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn sibling_children_of_one_parent_are_independent_nodes() {
    let child = |name: &str| {
        replayed(
            name,
            serde_json::json!([{"name": "parent", "type": "blob", "not_null": true}]),
            serde_json::json!({"foreign_keys": [
                {"columns": ["parent"], "table": "hub", "on_delete": "restrict"}
            ]}),
        )
    };
    let c = Constrained::with(
        serde_json::json!({
            "format": 10,
            "application": "star-graph",
            "tables": [
                replayed("hub", serde_json::json!([{"name": "name", "type": "text"}]), serde_json::json!({})),
                child("c1"),
                child("c2"),
                child("c3"),
                child("c4"),
            ],
        }),
        |_| {},
    );
    assert_eq!(
        graph_of(&c, &["c4", "c2", "c1", "hub", "c3"]).await,
        vec![
            DependencyNode {
                items: vec!["hub"],
                parents: vec![],
            },
            DependencyNode {
                items: vec!["c4"],
                parents: vec![0],
            },
            DependencyNode {
                items: vec!["c2"],
                parents: vec![0],
            },
            DependencyNode {
                items: vec!["c1"],
                parents: vec![0],
            },
            DependencyNode {
                items: vec!["c3"],
                parents: vec![0],
            },
        ]
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_five_table_chain_orders_linearly_parent_to_child() {
    let step = |name: &str, parent: &str| {
        replayed(
            name,
            serde_json::json!([{"name": "parent", "type": "blob", "not_null": true}]),
            serde_json::json!({"foreign_keys": [
                {"columns": ["parent"], "table": parent, "on_delete": "restrict"}
            ]}),
        )
    };
    let c = Constrained::with(
        serde_json::json!({
            "format": 10,
            "application": "chain-graph",
            "tables": [
                replayed("t1", serde_json::json!([{"name": "name", "type": "text"}]), serde_json::json!({})),
                step("t2", "t1"),
                step("t3", "t2"),
                step("t4", "t3"),
                step("t5", "t4"),
            ],
        }),
        |_| {},
    );
    assert_eq!(
        graph_of(&c, &["t5", "t3", "t1", "t4", "t2"]).await,
        vec![
            DependencyNode {
                items: vec!["t1"],
                parents: vec![],
            },
            DependencyNode {
                items: vec!["t2"],
                parents: vec![0],
            },
            DependencyNode {
                items: vec!["t3"],
                parents: vec![0, 1],
            },
            DependencyNode {
                items: vec!["t4"],
                parents: vec![0, 1, 2],
            },
            DependencyNode {
                items: vec!["t5"],
                parents: vec![0, 1, 2, 3],
            },
        ]
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn scopes_referencing_each_other_collapse_into_one_node() {
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
            "format": 10,
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
        graph_of(&c, &["hens", "barns", "eggs"]).await,
        vec![
            DependencyNode {
                items: vec!["hens", "eggs"],
                parents: vec![],
            },
            DependencyNode {
                items: vec!["barns"],
                parents: vec![],
            },
        ]
    );
}
