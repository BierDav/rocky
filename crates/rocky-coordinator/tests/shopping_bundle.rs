use rocky_coordinator::TrustBundle;

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn the_retired_provider_bundle_is_refused() {
    let published = include_str!("fixtures/retired-provider-bundle.json");
    assert!(TrustBundle::from_json(published).is_err());
}
