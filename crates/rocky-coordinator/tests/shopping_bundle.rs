//! The example's published certificate must validate before application startup.
use rocky_coordinator::certified::TrustBundle;

#[test]
fn shopping_and_its_public_guide_pin_the_same_valid_service() {
    let source = include_str!(
        "../../../examples/shoppinglist-app/shared/src/data/coordination/PublicCoordinator.kt"
    );
    let json = source.split("\"\"\"").nth(1).unwrap();
    let bundle = TrustBundle::from_json(json).unwrap();
    let genesis = bundle.genesis().unwrap();

    let guide = include_str!("../../../PUBLIC_COORDINATOR.md");
    let published = guide
        .split("```json\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let published = TrustBundle::from_json(published)
        .unwrap()
        .genesis()
        .unwrap();
    assert_eq!(genesis.id().unwrap(), published.id().unwrap());
    assert_eq!(genesis.genesis.public.unwrap().endpoints.len(), 4);
}
