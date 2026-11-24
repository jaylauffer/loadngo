//! TRIAL ONLY (2026-11-24, Jay: "try fips204"): the pure-Rust ML-DSA-65 that pq-crypto now uses, against AWS-LC,
//! the implementation it replaced. The same seed must give the same keys, and each must accept the other's
//! signatures and refuse a damaged one. Removed with the aws-lc-rs test dependency once its result is recorded.

use aws_lc_rs::encoding::AsRawBytes;
use aws_lc_rs::signature::{
    KeyPair, PqdsaKeyPair, UnparsedPublicKey, ML_DSA_65, ML_DSA_65_SIGNING,
};
use fips204::ml_dsa_65;
use fips204::traits::{KeyGen, SerDes};
use loadngo_pq_crypto::{
    CryptoError, MlDsa65Scheme, PqSignatureScheme, PrivateKey, PublicKey, Signature,
    SignatureSchemeId,
};

/// Seeds that differ in every byte from one to the next (a counter through SHA-free arithmetic:
/// nothing here needs to be unpredictable, only varied).
fn seed(n: u32) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut x = n.wrapping_mul(0x9e37_79b9).wrapping_add(0x7f4a_7c15);
    for byte in &mut out {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *byte = (x >> 24) as u8;
    }
    out
}

#[test]
fn the_same_seed_gives_the_same_keys_and_each_accepts_the_others_signatures() {
    let scheme = MlDsa65Scheme;
    for n in 0..48u32 {
        let seed = seed(n);
        // Keys.
        let aws = PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, &seed).expect("AWS-LC key pair");
        let aws_public = aws.public_key().as_ref().to_vec();
        let aws_private = aws
            .private_key()
            .as_raw_bytes()
            .expect("AWS-LC raw key")
            .as_ref()
            .to_vec();
        let (public, private) = ml_dsa_65::KG::keygen_from_seed(&seed);
        let (public, private) = (public.into_bytes().to_vec(), private.into_bytes().to_vec());
        assert_eq!(public, aws_public, "public keys differ for seed {n}");
        assert_eq!(private, aws_private, "private keys differ for seed {n}");

        let public_key = PublicKey::new(SignatureSchemeId::MlDsa65, public).expect("public key");
        let private_key =
            PrivateKey::new(SignatureSchemeId::MlDsa65, private).expect("private key");
        let message =
            format!("loadngo repository head {n}: a message of some length to sign").into_bytes();

        // Ours, checked by AWS-LC.
        let ours = scheme
            .sign(&private_key, &message)
            .expect("sign with fips204");
        UnparsedPublicKey::new(&ML_DSA_65, &aws_public)
            .verify(&message, &ours.bytes)
            .unwrap_or_else(|_| panic!("AWS-LC refused our signature for seed {n}"));

        // AWS-LC's, checked by ours.
        let mut theirs = vec![0u8; ML_DSA_65_SIGNING.signature_len()];
        let length = aws.sign(&message, &mut theirs).expect("sign with AWS-LC");
        theirs.truncate(length);
        let theirs = Signature::new(SignatureSchemeId::MlDsa65, theirs).expect("signature");
        scheme
            .verify(&public_key, &message, &theirs)
            .unwrap_or_else(|_| panic!("we refused AWS-LC's signature for seed {n}"));

        // A changed message, and a signature with one bit flipped, are refused by both.
        let mut other = message.clone();
        other[0] ^= 1;
        assert_eq!(
            scheme.verify(&public_key, &other, &theirs),
            Err(CryptoError::InvalidSignature)
        );
        assert!(UnparsedPublicKey::new(&ML_DSA_65, &aws_public)
            .verify(&other, &ours.bytes)
            .is_err());
        for at in [0usize, 31, 1500, 3308] {
            let mut bad = theirs.bytes.clone();
            bad[at] ^= 0x04;
            let bad_signature =
                Signature::new(SignatureSchemeId::MlDsa65, bad.clone()).expect("signature");
            assert_eq!(
                scheme.verify(&public_key, &message, &bad_signature),
                Err(CryptoError::InvalidSignature),
                "we accepted a damaged signature (seed {n}, byte {at})"
            );
            assert!(
                UnparsedPublicKey::new(&ML_DSA_65, &aws_public)
                    .verify(&message, &bad)
                    .is_err(),
                "AWS-LC accepted a damaged signature (seed {n}, byte {at})"
            );
        }
    }
}

#[test]
#[ignore = "a timing, for the trial's report: cargo test --release ... -- --ignored --nocapture"]
fn how_long_each_takes() {
    use std::time::Instant;
    let seed = seed(7);
    let aws = PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, &seed).expect("AWS-LC key pair");
    let aws_public = aws.public_key().as_ref().to_vec();
    let (public, private) = ml_dsa_65::KG::keygen_from_seed(&seed);
    let public_key = PublicKey::new(SignatureSchemeId::MlDsa65, public.into_bytes().to_vec())
        .expect("public key");
    let private_key = PrivateKey::new(SignatureSchemeId::MlDsa65, private.into_bytes().to_vec())
        .expect("private key");
    let scheme = MlDsa65Scheme;
    let message = vec![0x5au8; 512];
    const ROUNDS: u32 = 400;

    let t = Instant::now();
    let mut ours = None;
    for _ in 0..ROUNDS {
        ours = Some(scheme.sign(&private_key, &message).expect("sign"));
    }
    let ours_sign = t.elapsed() / ROUNDS;
    let ours = ours.expect("a signature");
    let t = Instant::now();
    for _ in 0..ROUNDS {
        scheme.verify(&public_key, &message, &ours).expect("verify");
    }
    let ours_verify = t.elapsed() / ROUNDS;
    let t = Instant::now();
    for n in 0..ROUNDS {
        let _ = ml_dsa_65::KG::keygen_from_seed(&self::seed(n));
    }
    let ours_keygen = t.elapsed() / ROUNDS;

    let mut theirs = vec![0u8; ML_DSA_65_SIGNING.signature_len()];
    let t = Instant::now();
    for _ in 0..ROUNDS {
        aws.sign(&message, &mut theirs).expect("sign");
    }
    let aws_sign = t.elapsed() / ROUNDS;
    let t = Instant::now();
    for _ in 0..ROUNDS {
        UnparsedPublicKey::new(&ML_DSA_65, &aws_public)
            .verify(&message, &theirs)
            .expect("verify");
    }
    let aws_verify = t.elapsed() / ROUNDS;
    let t = Instant::now();
    for n in 0..ROUNDS {
        let _ = PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, &self::seed(n)).expect("key pair");
    }
    let aws_keygen = t.elapsed() / ROUNDS;

    println!("TIMING fips204: keygen {ours_keygen:?}, sign {ours_sign:?}, verify {ours_verify:?}");
    println!("TIMING aws-lc:  keygen {aws_keygen:?}, sign {aws_sign:?}, verify {aws_verify:?}");
}
