use rocky_core::*;
const SCOPE: Scope = Scope {
    mesh: MeshId([0x11; 16]),
    app: AppId([0x22; 16]),
    domain: DomainId([0x33; 16]),
};
fn hex(b: &[u8]) -> String {
    b.iter().map(|v| format!("{v:02x}")).collect()
}
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn the_genesis_anchor_keeps_its_vector() {
    assert_eq!(
        hex(&genesis(SCOPE, [7; 16])),
        "48ead3a3d0babf04aee931f6f43e86c2f38453111304988dd63867dcdf87469a"
    );
}
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_log_anchors_at_its_scope_genesis_and_tip_and_chains_big_endian_positions() {
    let log = TipLog {
        scope: SCOPE,
        tip: TipId([3; 32]),
    };
    let anchor = log_genesis(log, [7; 16]);
    let mut opening = genesis(SCOPE, [7; 16]).to_vec();
    opening.extend([3; 32]);
    assert_eq!(anchor, commitment("rocky.coordinated-genesis.v1", &opening));
    let mut entry = anchor.to_vec();
    entry.extend(1u64.to_be_bytes());
    entry.extend([9; 32]);
    assert_eq!(
        coordinated_chain(anchor, 1, [9; 32]),
        commitment("rocky.coordinated-chain.v1", &entry)
    );
    assert_ne!(
        coordinated_chain(anchor, 1, [9; 32]),
        coordinated_chain(anchor, 2, [9; 32])
    );
}
