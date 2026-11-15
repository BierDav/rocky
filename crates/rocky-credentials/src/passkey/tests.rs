use super::*;

fn namespace(purpose: &str) -> PersonalNamespace {
    PersonalNamespace::new("notes.example", purpose).unwrap()
}
/// A stand-in authenticator: one credential's PRF over the evaluation input.
fn evaluate(credential: u8, namespace: &PersonalNamespace) -> VerifiedPasskeyPrf {
    let input = namespace.prf_input().unwrap();
    let output = crate::group::hkdf(&[credential; 32], input.as_bytes(), b"test authenticator");
    VerifiedPasskeyPrf::new(&input, output)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn derived(credential: u8, namespace: &PersonalNamespace) -> PersonalAccount {
    PersonalAccount::derive(evaluate(credential, namespace), namespace).unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_fixed_prf_restores_the_same_distinct_root_identity_and_locator() {
    let account = derived(1, &namespace("account root"));
    // An empty device evaluating the same credential derives it all again.
    let again = derived(1, &namespace("account root"));
    assert_eq!(account.root().secret(), again.root().secret());
    assert_eq!(account.identity().secret(), again.identity().secret());
    assert_eq!(account.locator(), again.locator());
    assert_eq!(account.discovery().topic(), again.discovery().topic());
    let locator = account.locator();
    assert_eq!(locator.publisher, account.root().public_key());
    assert_eq!(locator.owner, account.identity().public_key());
    assert_ne!(locator.owner, locator.publisher);
    let ns = namespace("account root");
    assert_eq!(locator.mesh, ns.mesh(locator.owner).unwrap());
    assert_eq!(locator.domain, ns.domain(locator.owner).unwrap());
    assert_eq!(
        locator.scope("notes").app,
        ApplicationManifest::app_id(*locator.publisher.as_bytes(), "notes")
    );
    // Three distinct secrets: root, identity and the discovery capability.
    let secrets = [
        account.root().secret(),
        account.identity().secret(),
        *account.discovery().topic(),
    ];
    assert!(secrets[0] != secrets[1] && secrets[1] != secrets[2] && secrets[0] != secrets[2]);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn the_root_and_identity_keep_the_existing_derivation_vectors() {
    let ns = namespace("account root");
    let input = ns.prf_input().unwrap();
    let account = PersonalAccount::derive(VerifiedPasskeyPrf::new(&input, [7; 32]), &ns).unwrap();
    // `RootKey::from_secret(prf)` and HKDF-SHA256 "rocky identity v1".
    assert_eq!(account.root().secret(), [7; 32]);
    assert_eq!(
        hex(&account.identity().secret()),
        "9c390ed6e479fa7ac5add3e9107652cd7cf143dac2b4466a3a1f7178be565a91"
    );
    // The evaluation input and the topic label are part of the format.
    assert_eq!(
        hex(input.as_bytes()),
        hex(&rocky_core::commitment(PRF_INPUT, &ns.canonical().unwrap()))
    );
    let topic = crate::group::hkdf(
        &[],
        &account.identity().secret(),
        &[
            b"rocky.personal.discovery.v1".as_slice(),
            &ns.canonical().unwrap(),
        ]
        .concat(),
    );
    assert_eq!(account.discovery().topic(), &topic);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn other_namespaces_and_credentials_name_other_accounts() {
    let account = derived(1, &namespace("account root"));
    let other_namespace = derived(1, &namespace("other root"));
    let other_credential = derived(2, &namespace("account root"));
    let other_rp = {
        let ns = PersonalNamespace::new("other.example", "account root").unwrap();
        derived(1, &ns)
    };
    for other in [&other_namespace, &other_credential, &other_rp] {
        assert_ne!(other.root().public_key(), account.root().public_key());
        assert_ne!(
            other.identity().public_key(),
            account.identity().public_key()
        );
        assert_ne!(other.locator().mesh, account.locator().mesh);
        assert_ne!(other.locator().domain, account.locator().domain);
        assert_ne!(other.discovery().topic(), account.discovery().topic());
    }
    assert_ne!(
        namespace("account root").prf_input().unwrap(),
        namespace("other root").prf_input().unwrap()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn the_topic_follows_the_identity_secret_not_the_public_identity() {
    // One owner public key, two namespaces: a topic computed from the
    // public identity alone would need only the key and the namespace.
    let ns = namespace("account root");
    let account = derived(1, &ns);
    let public = account.identity().public_key();
    for label in ["rocky.personal.discovery.v1", "rocky.discovery-topic.v1"] {
        assert_ne!(
            account.discovery().topic(),
            &rocky_core::commitment(label, public.as_bytes())
        );
    }
    // The same identity secret under another namespace finds another topic.
    let identity = account.identity().secret();
    let elsewhere = crate::group::hkdf(
        &[],
        &identity,
        &[DISCOVERY, &namespace("other root").canonical().unwrap()].concat(),
    );
    assert_ne!(account.discovery().topic(), &elsewhere);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn an_unusable_prf_is_refused() {
    let ns = namespace("account root");
    let input = ns.prf_input().unwrap();
    // Evaluated for another namespace's input.
    assert_eq!(
        PersonalAccount::derive(evaluate(1, &namespace("other root")), &ns).unwrap_err(),
        PrfUnsupported
    );
    // No PRF result: zeros, or the input echoed back.
    for output in [[0; 32], *input.as_bytes()] {
        let prf = VerifiedPasskeyPrf::new(&input, output);
        assert_eq!(
            PersonalAccount::derive(prf, &ns).unwrap_err(),
            PrfUnsupported
        );
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn debug_output_redacts_every_secret() {
    let ns = namespace("account root");
    let input = ns.prf_input().unwrap();
    let prf = VerifiedPasskeyPrf::new(&input, [0xab; 32]);
    assert_eq!(format!("{prf:?}"), "VerifiedPasskeyPrf(<redacted>)");
    let account = PersonalAccount::derive(prf, &ns).unwrap();
    let shown = format!("{account:?} {:?}", account.discovery());
    for secret in [
        account.root().secret(),
        account.identity().secret(),
        *account.discovery().topic(),
    ] {
        assert!(!shown.contains(&hex(&secret)));
    }
    assert!(!shown.contains("171, 171"));
}
