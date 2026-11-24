//! loadngo's known answers for this copy of `fips204`: for each parameter set, sixteen key pairs from fixed seeds
//! and a deterministic signature by each, hashed together. The digests below were taken on 2026-11-24 from the
//! crate with upstream's arithmetic and wiping (commit 698ecd05, itself cross-checked against AWS-LC, OpenSSL
//! and the NIST vector), before the work on wiping and on working in place. Any change that alters a key or a
//! signature by one bit changes a digest: a change to how the arithmetic is carried out must leave them alone.
#![cfg(all(feature = "ml-dsa-44", feature = "ml-dsa-65", feature = "ml-dsa-87"))]

use fips204::traits::{KeyGen, SerDes, Signer, Verifier};
use sha2::{Digest, Sha256};

macro_rules! digest_of {
    ($module:ident) => {{
        let mut hasher = Sha256::new();
        for n in 0u8..16 {
            let seed: [u8; 32] =
                core::array::from_fn(|i| (i as u8).wrapping_mul(n.wrapping_add(3)) ^ n);
            let (public_key, private_key) = fips204::$module::KG::keygen_from_seed(&seed);
            let message: Vec<u8> = (0..usize::from(n) * 17 + 1)
                .map(|i| (i as u8) ^ n)
                .collect();
            let context = [n; 3];
            let signing_seed: [u8; 32] = core::array::from_fn(|i| (i as u8) ^ n ^ 0x5a);
            let signature = private_key
                .try_sign_with_seed(&signing_seed, &message, &context)
                .expect("a signature");
            assert!(
                public_key.verify(&message, &signature, &context),
                "our own signature is refused"
            );
            let mut other = message.clone();
            other[0] ^= 1;
            assert!(
                !public_key.verify(&other, &signature, &context),
                "a changed message is accepted"
            );
            hasher.update(public_key.into_bytes());
            hasher.update(private_key.into_bytes());
            hasher.update(signature);
        }
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    }};
}

#[test]
fn keys_and_signatures_are_what_they_were() {
    assert_eq!(
        digest_of!(ml_dsa_44),
        "7b675d4a96ab48db60fc98bad2d700ebc6dcc6bd81bb9893254e375788d40ed4"
    );
    assert_eq!(
        digest_of!(ml_dsa_65),
        "efbff98ad7c8afbb10424c157be7c15edb6a7049950d76aa7cc945afb45ef1a0"
    );
    assert_eq!(
        digest_of!(ml_dsa_87),
        "817244aa0cba3abe786466e5b4f3edda8e621b120d1439a2e668e77c29de797c"
    );
}
