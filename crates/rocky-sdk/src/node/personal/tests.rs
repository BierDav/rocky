use super::*;

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_flood_of_hellos_evicts_no_issued_nonce() {
    let mut nonces = Nonces::default();
    let act = PersonalAct::Bootstrap { page: 0 };
    let kept = nonces.issue(1_000);
    for _ in 0..4 * MAX_NONCES {
        nonces.issue(1_000);
    }
    nonces
        .spend(&kept, act, 1_001)
        .expect("the first nonce still spends");
    // Each act once; another act under the same nonce still spends.
    assert!(nonces.spend(&kept, act, 1_002).is_err());
    nonces
        .spend(&kept, PersonalAct::Bootstrap { page: 1 }, 1_002)
        .expect("the next page");
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn only_a_live_nonce_this_responder_issued_spends() {
    let mut nonces = Nonces::default();
    let act = PersonalAct::Bootstrap { page: 0 };
    let issued = nonces.issue(1_000);
    let mut forged = issued;
    forged[31] ^= 1;
    assert!(nonces.spend(&forged, act, 1_001).is_err());
    // Another process's nonce, or one presented after its lifetime.
    assert!(Nonces::default().spend(&issued, act, 1_001).is_err());
    assert!(nonces.spend(&issued, act, 1_000 + NONCE_LIFETIME).is_err());
    assert!(nonces.spend(&issued, act, 999).is_err());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn spent_replay_state_is_bounded() {
    let mut nonces = Nonces::default();
    let act = PersonalAct::Bootstrap { page: 0 };
    for _ in 0..MAX_NONCES {
        let nonce = nonces.issue(1_000);
        nonces.spend(&nonce, act, 1_000).expect("spends");
    }
    let refused = nonces.issue(1_000);
    assert!(nonces.spend(&refused, act, 1_000).is_err());
    // Expired records make room again.
    let later = nonces.issue(1_000 + NONCE_LIFETIME);
    nonces
        .spend(&later, act, 1_000 + NONCE_LIFETIME)
        .expect("room after expiry");
}
