use super::*;
use rocky_core::{AppId, DomainId, MeshId};

fn domain() -> Scope {
    Scope {
        mesh: MeshId([1; 16]),
        app: AppId([2; 16]),
        domain: DomainId([3; 16]),
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn hkdf_matches_the_published_vectors() {
    // RFC 4231 test case 6: a key longer than the block is hashed first.
    assert_eq!(
        hex(&hmac(
            &[0xaa; 131],
            &[b"Test Using Larger Than Block-Size Key - Hash Key First"]
        )),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
    );
    // RFC 5869 test case 1, the first output block.
    let salt: Vec<u8> = (0..=0x0c).collect();
    let info: Vec<u8> = (0xf0..=0xf9).collect();
    assert_eq!(
        hex(&hkdf(&salt, &[0x0b; 22], &info)),
        "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf"
    );
}

#[test]
fn table_keys_are_bound_to_domain_group_table_and_epoch() {
    let secret = [7; 32];
    let key = |secret: &[u8; 32], domain, group, table, epoch| {
        derive_table_key(secret, domain, group, table, epoch).unwrap()
    };
    let base = key(&secret, domain(), [4; 32], [5; 32], 1);
    assert_eq!(base, key(&secret, domain(), [4; 32], [5; 32], 1));
    let other_domain = Scope {
        domain: DomainId([9; 16]),
        ..domain()
    };
    let variants = [
        key(&[8; 32], domain(), [4; 32], [5; 32], 1),
        key(&secret, other_domain, [4; 32], [5; 32], 1),
        key(&secret, domain(), [6; 32], [5; 32], 1),
        key(&secret, domain(), [4; 32], [6; 32], 1),
        key(&secret, domain(), [4; 32], [5; 32], 2),
    ];
    let distinct: BTreeSet<_> = variants.iter().chain([&base]).collect();
    assert_eq!(distinct.len(), variants.len() + 1);
    // Neither the secret nor a sibling's key is the derived key.
    assert!(!distinct.contains(&secret));
    assert!(derive_table_key(&secret, domain(), [4; 32], [5; 32], 0).is_err());
}

#[test]
fn group_envelopes_open_only_for_the_pinned_recipient_group_and_epoch() {
    let issuer = PrivateKey::new();
    let alice = PrivateKey::new();
    let bob = PrivateKey::new();
    let outsider = PrivateKey::new();
    let group = [4; 32];
    let epoch = rocky_core::KeyEpoch::new(1, &[7; 32]);
    let next = rocky_core::KeyEpoch::new(2, &[7; 32]);
    let recipients = [alice.public_key(), bob.public_key()].into();
    let envelopes =
        GroupSecretEnvelope::seal_many(&issuer, group, 1, [7; 32], &recipients).unwrap();
    assert_eq!(envelopes.len(), 2);
    for recipient in [&alice, &bob] {
        let envelope = envelopes
            .iter()
            .find(|envelope| envelope.recipient() == recipient.public_key())
            .unwrap();
        let decoded = GroupSecretEnvelope::from_bytes(&envelope.to_bytes().unwrap()).unwrap();
        assert_eq!((decoded.group(), decoded.epoch()), (group, epoch));
        let open =
            |key: &PrivateKey, by: PublicKey, group, epoch| decoded.open(key, by, group, epoch);
        assert_eq!(
            open(recipient, issuer.public_key(), group, epoch).unwrap(),
            [7; 32]
        );
        assert!(open(&outsider, issuer.public_key(), group, epoch).is_err());
        assert!(open(recipient, outsider.public_key(), group, epoch).is_err());
        assert!(open(recipient, issuer.public_key(), [5; 32], epoch).is_err());
        assert!(open(recipient, issuer.public_key(), group, next).is_err());
        let mut tampered = decoded.clone();
        tampered.body.sealed[20] ^= 1;
        assert!(tampered.verify(issuer.public_key(), group, epoch).is_err());
        // A valid issuer signature cannot transplant existing ciphertext.
        for change in [
            |body: &mut Body| body.epoch.generation = 2,
            |body: &mut Body| body.group = [5; 32],
            |body: &mut Body| body.epoch.secret_hash[0] ^= 1,
            |body: &mut Body| body.statements.push([9; 32]),
            |body: &mut Body| body.ephemeral = [0; 32],
        ] {
            let mut tampered = decoded.clone();
            change(&mut tampered.body);
            tampered.signature = issuer.sign(&signing_bytes(SIGNED, &tampered.body).unwrap());
            let (group, epoch) = (tampered.body.group, tampered.body.epoch);
            assert!(
                tampered
                    .open(recipient, issuer.public_key(), group, epoch)
                    .is_err()
            );
        }
        // The sealed length follows the secret length; any other is refused.
        let mut truncated = decoded.clone();
        truncated.body.sealed.pop();
        truncated.signature = issuer.sign(&signing_bytes(SIGNED, &truncated.body).unwrap());
        assert!(GroupSecretEnvelope::from_bytes(&truncated.to_bytes().unwrap()).is_err());
        let mut versioned = decoded.clone();
        versioned.body.version = 2;
        versioned.signature = issuer.sign(&signing_bytes(SIGNED, &versioned.body).unwrap());
        assert!(GroupSecretEnvelope::from_bytes(&versioned.to_bytes().unwrap()).is_err());
    }
    assert_ne!(envelopes[0].body.ephemeral, envelopes[1].body.ephemeral);
    assert!(GroupSecretEnvelope::seal_many(&issuer, group, 1, [7; 32], &BTreeSet::new()).is_err());
    assert!(GroupSecretEnvelope::seal(&issuer, group, 0, [7; 32], alice.public_key()).is_err());
    assert!(GroupSecretEnvelope::from_bytes(&[0; 5000]).is_err());
}
