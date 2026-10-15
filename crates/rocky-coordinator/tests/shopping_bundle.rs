//! The retired provider certificate must never be treated as a current bootstrap.
use rocky_coordinator::certified::TrustBundle;

#[test]
fn the_retired_provider_bundle_is_refused() {
    let guide = include_str!("../../../PUBLIC_COORDINATOR.md");
    let published = guide
        .split("```json\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    assert!(TrustBundle::from_json(published).is_err());
}
