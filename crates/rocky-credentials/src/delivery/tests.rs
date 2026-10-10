use super::*;

fn key(seed: u8) -> PrivateKey {
    PrivateKey::from_bytes(&[seed; 32])
}

#[test]
fn only_the_recipient_opens_a_delivery_and_nothing_else_is_in_the_clear() {
    let (recipient, other) = (key(1), key(2));
    let payload = b"group: list, values: [7], envelope: ...".to_vec();
    let delivery = SealedDelivery::seal(recipient.public_key(), &payload).unwrap();
    assert_eq!(delivery.open(&recipient).unwrap(), payload);
    assert!(delivery.open(&other).is_err());
    assert_eq!(delivery.recipient(), recipient.public_key());
    assert_eq!(delivery.payload_len(), payload.len());
    let bytes = delivery.to_bytes().unwrap();
    assert!(
        !bytes.windows(4).any(|window| window == b"list"),
        "the payload is sealed"
    );
    // Two sealings of one payload share no ciphertext or digest.
    let again = SealedDelivery::seal(recipient.public_key(), &payload).unwrap();
    assert_ne!(again.digest().unwrap(), delivery.digest().unwrap());
    assert_eq!(SealedDelivery::from_bytes(&bytes).unwrap(), delivery);
}

#[test]
fn a_changed_recipient_ephemeral_or_ciphertext_is_refused() {
    let recipient = key(1);
    let delivery = SealedDelivery::seal(recipient.public_key(), b"payload").unwrap();
    let mut readdressed = delivery.clone();
    readdressed.recipient = key(2).public_key();
    assert!(readdressed.open(&key(2)).is_err());
    let mut ephemeral = delivery.clone();
    ephemeral.ephemeral[0] ^= 1;
    assert!(ephemeral.open(&recipient).is_err());
    let mut sealed = delivery.clone();
    let last = sealed.sealed.len() - 1;
    sealed.sealed[last] ^= 1;
    assert!(sealed.open(&recipient).is_err());
    let mut version = delivery;
    version.version = 2;
    assert!(SealedDelivery::from_bytes(&encode(&version).unwrap()).is_err());
    assert!(SealedDelivery::seal(recipient.public_key(), &[]).is_err());
    assert!(
        SealedDelivery::seal(recipient.public_key(), &vec![0; MAX_DELIVERY_PAYLOAD + 1]).is_err()
    );
}

#[test]
fn a_receipt_verifies_only_for_its_recipient_and_digest() {
    let recipient = key(1);
    let receipt = DeliveryReceipt::sign(&recipient, [9; 32]).unwrap();
    receipt.verify(recipient.public_key(), [9; 32]).unwrap();
    assert!(receipt.verify(key(2).public_key(), [9; 32]).is_err());
    assert!(receipt.verify(recipient.public_key(), [8; 32]).is_err());
    let forged = DeliveryReceipt::sign(&key(2), [9; 32]).unwrap();
    assert!(forged.verify(recipient.public_key(), [9; 32]).is_err());
    let decoded = DeliveryReceipt::from_bytes(&receipt.to_bytes().unwrap()).unwrap();
    assert_eq!(decoded, receipt);
}
