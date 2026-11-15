use super::*;

fn account() -> PersonalAccount {
    let namespace = rocky_credentials::PersonalNamespace::new("notes.example", "account root")
        .expect("namespace");
    let input = namespace.prf_input().expect("input");
    PersonalAccount::derive(crate::VerifiedPasskeyPrf::new(&input, [4; 32]), &namespace)
        .expect("account")
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn only_an_owner_device_no_statement_ended_anchors() -> Result<(), Failure> {
    let account = account();
    let locator = account.locator();
    let scope = locator.scope("notes");
    let grant = rocky_credentials::Authority::owner(account.root()).issue_trusted(
        locator.owner,
        scope,
        1,
    )?;
    let node = PrivateKey::new();
    let chain = CredentialChain(vec![grant.clone()]).certify_personal(
        &account.identity(),
        &node,
        (locator.owner, [1; 32]),
    )?;
    let leaf = chain.certificate().ok_or("certificate")?;
    assert!(anchors(leaf, locator.owner, &RevocationSet::new())?);
    // The responder's own set carries the tombstone that ended it.
    let tombstone = chain.unlink(&account.identity(), scope.app, 2, node.public_key())?;
    let ended = RevocationSet::try_from(vec![tombstone])?;
    assert!(!anchors(leaf, locator.owner, &ended)?);
    // The root's own grant is no owner device.
    assert!(!anchors(&grant, locator.owner, &RevocationSet::new())?);
    Ok(())
}
