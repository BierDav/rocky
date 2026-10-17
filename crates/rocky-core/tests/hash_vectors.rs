//! Frozen with Python hashlib before enabling the target SHA-2 backend.
use sha2::{Digest, Sha256, Sha512};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn commitments_keep_independent_padding_and_prefix_vectors() {
    let cases: &[(&str, Vec<u8>, &str)] = &[
        (
            "",
            vec![],
            "df3f619804a92fdb4057192dc43dd748ea778adc52bc498ce80524c014b81119",
        ),
        (
            "é🪨",
            vec![0, 255, 1],
            "33f9346e37208659e4f62aaac022557877f5ff6b384d330a2c45b87783e8e836",
        ),
        (
            &"x".repeat(257),
            vec![0, 255],
            "1eefd3cb3447f6ea5c9226a2d0a8fa0ba76e18147a437655dbfda815de397979",
        ),
        (
            "p",
            (0..50).map(|i| (i % 256) as u8).collect(),
            "04fa95822da16b7e18630d1e46fcea6550f21824f80cba2109ae4d2f1c025ec7",
        ),
        (
            "p",
            (0..51).map(|i| (i % 256) as u8).collect(),
            "fb006d8a85dcdc82dbb2aa02da2134d9e25fa012d7c549ba3afd0ba278d6484b",
        ),
        (
            "p",
            (0..58).map(|i| (i % 256) as u8).collect(),
            "6b48060760e9934ac0fa803e8621c016c56920e548e06bc09d5310ca2d709530",
        ),
        (
            "p",
            (0..59).map(|i| (i % 256) as u8).collect(),
            "449b48c2c745087fe1b36b2de5df762d6db2649fadb2e5e48ff16324e2512245",
        ),
        (
            "p",
            (0..114).map(|i| (i % 256) as u8).collect(),
            "2672b18ec2b9dd8e70f811b20bf0e2bc6d65c16c83da7c50431a73542296f3ad",
        ),
        (
            "p",
            (0..115).map(|i| (i % 256) as u8).collect(),
            "c935cdcb449d062b51fc0d7fcaea992beae41c701413b1433d9f1e3044aa301b",
        ),
        (
            "p",
            (0..4096).map(|i| (i % 256) as u8).collect(),
            "ea1745626a4ea5a5f15be7817f9fddcf22b5605d9ab8f31f8a926d2f765e70b0",
        ),
    ];
    for (prefix, payload, expected) in cases {
        assert_eq!(hex(&rocky_core::commitment(prefix, payload)), *expected);
    }
}

#[test]
fn raw_sha256_and_sha512_keep_independent_vectors() {
    let cases: &[(usize, &str, &str)] = &[
        (
            0,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e",
        ),
        (
            3,
            "ae4b3280e56e2faf83f414a6e3dabe9d5fbe18976544c05fed121accb85b53fc",
            "8081da5f9c1e3d0e1aa16f604d5e5064543cff5d7bace2bb312252461e151b3fe0f034ea8dc1dacff3361a892d625fbe1b614cda265f87a473c24b0fa1d91dfd",
        ),
        (
            55,
            "463eb28e72f82e0a96c0a4cc53690c571281131f672aa229e0d45ae59b598b59",
            "6856647f269c2ee3d8128f0b25427659d880641ef343300dd3cd4679168f58d6527fda70b4ebc854e2065e172b7d58c1536992c0810599259ba84a2b40c65414",
        ),
        (
            56,
            "da2ae4d6b36748f2a318f23e7ab1dfdf45acdc9d049bd80e59de82a60895f562",
            "8b12b2f6fe400a51d29656e2b8c42a1bbfe6fcf3e425da430db05d1a2dda14790dee20fa8b22d8762afffe4988a5c98a4430d22a17e41e23d90fa61ab75671a9",
        ),
        (
            63,
            "29af2686fd53374a36b0846694cc342177e428d1647515f078784d69cdb9e488",
            "9dc9c5598e55dc42955695320839788e353f1d7f6ba74df74c80a8a52f463c0697f57f68835d1418f4ce9b6530cd79bd0f4c6f7e13c93feb1218c0b65c2c0561",
        ),
        (
            64,
            "fdeab9acf3710362bd2658cdc9a29e8f9c757fcf9811603a8c447cd1d9151108",
            "ee4320ebaf3fdb4f2c832b137200c08e235e0fa7bbd0eb1740c7063ba8a0d151da77e003398e1714a955d475b05e3e950b639503b452ec185de4229bc4873949",
        ),
        (
            111,
            "60780e9451bdc43cf4530ffc95cbb0c4eb24dae2c39f55f334d679e076c08065",
            "a1a111449b198d9b1f538bad7f3fc1022b3a5b1a5e90a0bc860de8512746cbc31599e6c834de3a3235327af0b51ff57bf7acf1974a73014d9c3953812edc7c8d",
        ),
        (
            112,
            "09373f127d34e61dbbaa8bc4499c87074f2ddb10e1b465f506d7d70a15011979",
            "c5fbd731d19d2ae1180f001be72c2c1aaba1d7b094b3748880e24593b8e117a750e11c1bd867cc2f96dace8c8b74abd2d5c4f236be444e77d30d1916174070b9",
        ),
        (
            127,
            "92ca0fa6651ee2f97b884b7246a562fa71250fedefe5ebf270d31c546bfea976",
            "eab89674feaa34e27aebeeff3c0a4d70070bb872d5e9f186cf1dbbdee517b6e35724d629ff025a5b07185e911ada7e3c8acf830aa0e4f71777bd2d44f504f7f0",
        ),
        (
            128,
            "471fb943aa23c511f6f72f8d1652d9c880cfa392ad80503120547703e56a2be5",
            "1dffd5e3adb71d45d2245939665521ae001a317a03720a45732ba1900ca3b8351fc5c9b4ca513eba6f80bc7b1d1fdad4abd13491cb824d61b08d8c0e1561b3f7",
        ),
        (
            4096,
            "c8f5d0341d54d951a71b136e6e2afcb14d11ed8489a7ae126a8fee0df6ecf193",
            "034a1bd3ad5dbddf6c9aed6b1705661487e110dc7e158fe330c94363e8ffb53b1c92f883010fd73ce8a86115b7b4712ba0f3a9279760ed6220a5773eb54425f0",
        ),
    ];
    for (length, sha256, sha512) in cases {
        let bytes: Vec<_> = (0..*length).map(|i| (i % 256) as u8).collect();
        assert_eq!(hex(&Sha256::digest(&bytes)), *sha256);
        assert_eq!(hex(&Sha512::digest(&bytes)), *sha512);
    }
}
