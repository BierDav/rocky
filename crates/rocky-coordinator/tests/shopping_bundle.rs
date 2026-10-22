//! The retired provider certificate must never be treated as a current bootstrap.
use rocky_coordinator::certified::TrustBundle;

#[test]
fn the_retired_provider_bundle_is_refused() {
    let published = include_str!("fixtures/retired-provider-bundle.json");
    assert!(TrustBundle::from_json(published).is_err());
}
